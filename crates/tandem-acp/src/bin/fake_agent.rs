//! Minimal scripted ACP v1 agent over stdio, used by the integration tests.
//!
//! Prompt text controls behaviour:
//! - contains "hang": emits one chunk, waits for `session/cancel`, ends with `cancelled`.
//! - otherwise: text, plan, tool call, permission request, fs write/read (inside and
//!   outside cwd), tool update reflecting the decision, usage, `end_turn`.

use serde_json::{Value, json};
use std::io::Write;

struct Io {
    lines: std::io::Lines<std::io::StdinLock<'static>>,
    next_id: u64,
    session: String,
    cancelled: bool,
}

impl Io {
    fn send(&self, v: Value) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }
    fn update(&self, update: Value) {
        self.send(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":self.session,"update":update}}));
    }
    fn read(&mut self) -> Option<Value> {
        loop {
            let line = self.lines.next()?.ok()?;
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                return Some(v);
            }
        }
    }
    /// Send a request and wait for its response; notes cancels seen meanwhile.
    fn request(&mut self, method: &str, params: Value) -> Option<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let msg = self.read()?;
            if msg.get("method").and_then(Value::as_str) == Some("session/cancel") {
                self.cancelled = true;
                continue;
            }
            if msg.get("id") == Some(&json!(id)) && msg.get("method").is_none() {
                return Some(msg);
            }
        }
    }
}

fn main() {
    let mut io = Io {
        lines: std::io::stdin().lines(),
        next_id: 1000,
        session: "fake-session".into(),
        cancelled: false,
    };
    let mut cwd = String::new();
    while let Some(msg) = io.read() {
        let id = msg.get("id").cloned();
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let reply =
            |io: &Io, result: Value| io.send(json!({"jsonrpc":"2.0","id":id,"result":result}));
        match method.as_str() {
            "initialize" => reply(&io, json!({"protocolVersion":1,"agentCapabilities":{},"authMethods":[],"agentInfo":{"name":"fake","version":"0"}})),
            "session/new" => {
                cwd = params["cwd"].as_str().unwrap_or_default().to_string();
                reply(&io, json!({"sessionId": io.session}));
            }
            "session/prompt" => {
                let text = params["prompt"][0]["text"].as_str().unwrap_or_default().to_string();
                io.cancelled = false;
                if text.contains("hang") {
                    io.update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"working"}}));
                    while !io.cancelled {
                        match io.read() {
                            Some(m) if m.get("method").and_then(Value::as_str) == Some("session/cancel") => io.cancelled = true,
                            Some(_) => {}
                            None => return,
                        }
                    }
                    reply(&io, json!({"stopReason":"cancelled"}));
                    continue;
                }
                io.update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hello"}}));
                io.update(json!({"sessionUpdate":"plan","entries":[{"content":"clean build","priority":"high","status":"in_progress"}]}));
                io.update(json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"Remove build dir","kind":"execute",
                    "status":"pending","rawInput":{"command":"rm -rf build"},"locations":[{"path":format!("{cwd}/build")}]}));
                let perm = io.request("session/request_permission", json!({"sessionId":io.session,"toolCall":{"toolCallId":"t1"},
                    "options":[{"optionId":"allow","name":"Allow","kind":"allow_once"},{"optionId":"reject","name":"Reject","kind":"reject_once"}]}));
                let Some(perm) = perm else { return };
                let outcome = &perm["result"]["outcome"];
                let chosen = if outcome["outcome"] == "selected" { outcome["optionId"].as_str().unwrap_or("?").to_string() } else { "cancelled".into() };
                let w = io.request("fs/write_text_file", json!({"sessionId":io.session,"path":format!("{cwd}/out.txt"),"content":"a\nb\nc\n"}));
                let r = io.request("fs/read_text_file", json!({"sessionId":io.session,"path":format!("{cwd}/out.txt"),"line":2,"limit":1}));
                let bad = io.request("fs/read_text_file", json!({"sessionId":io.session,"path":"/etc/passwd"}));
                let (Some(w), Some(r), Some(bad)) = (w, r, bad) else { return };
                let fs = format!("write_ok={} read={:?} outside_rejected={}",
                    w.get("error").is_none(), r["result"]["content"].as_str().unwrap_or(""), bad.get("error").is_some());
                let status = if chosen == "allow" { "completed" } else { "failed" };
                io.update(json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":status,
                    "content":[{"type":"content","content":{"type":"text","text":format!("permission={chosen} {fs}")}}]}));
                io.update(json!({"sessionUpdate":"usage_update","used":100,"size":1000,"cost":{"amount":0.25,"currency":"USD"}}));
                reply(&io, json!({"stopReason":"end_turn","usage":{"totalTokens":30,"inputTokens":10,"outputTokens":20}}));
            }
            _ if id.is_some() => io.send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}})),
            _ => {}
        }
    }
}
