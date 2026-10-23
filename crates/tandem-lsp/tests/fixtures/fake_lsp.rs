//! Scripted fake language server for tandem-lsp integration tests. Speaks LSP over
//! stdio with blocking I/O; deliberately independent of the crate's own framing code.

use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

type Out = Arc<Mutex<std::io::Stdout>>;

fn write(out: &Out, msg: &Value) {
    let body = msg.to_string();
    if let Ok(mut o) = out.lock() {
        let _ = write!(o, "Content-Length: {}\r\n\r\n{}", body.len(), body);
        let _ = o.flush();
    }
}

fn notify(out: &Out, method: &str, params: Value) {
    write(out, &json!({"jsonrpc": "2.0", "method": method, "params": params}));
}

fn log(out: &Out, msg: &str) {
    notify(out, "window/logMessage", json!({"type": 3, "message": msg}));
}

fn read_msg(input: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0u8; len?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn main() {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut did_change_count = 0u64;
    let mut last_text = String::new();
    eprintln!("fake-lsp started");
    while let Some(msg) = read_msg(&mut input) {
        let method = msg.get("method").and_then(Value::as_str).map(str::to_string);
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(method) = method else {
            // Response to one of our server->client requests.
            match id.as_ref().and_then(Value::as_str) {
                Some("srv-cfg") => {
                    let n = msg["result"].as_array().map_or(0, Vec::len);
                    log(&out, &format!("configuration ok: {n}"));
                }
                Some("srv-reg") if msg.get("error").is_none() => log(&out, "register ok"),
                _ => {}
            }
            continue;
        };
        let uri = params["textDocument"]["uri"].clone();
        let pos = params["position"].clone();
        let reply = |result: Value| {
            write(&out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
        };
        match method.as_str() {
            "initialize" => {
                write(&out, &json!({"jsonrpc": "2.0", "id": "srv-cfg", "method": "workspace/configuration",
                    "params": {"items": [{"section": "a"}, {"section": "b"}]}}));
                write(&out, &json!({"jsonrpc": "2.0", "id": 7, "method": "window/workDoneProgress/create",
                    "params": {"token": "idx"}}));
                reply(json!({
                    "capabilities": {
                        "textDocumentSync": {"openClose": true, "change": 1, "save": {"includeText": true}},
                        "completionProvider": {"resolveProvider": true, "triggerCharacters": ["."]},
                        "hoverProvider": true, "definitionProvider": true, "referencesProvider": true,
                        "renameProvider": true, "documentSymbolProvider": true, "workspaceSymbolProvider": true,
                        "codeActionProvider": true, "documentFormattingProvider": true,
                        "signatureHelpProvider": {"triggerCharacters": ["("]}
                    },
                    "serverInfo": {"name": "fake-lsp", "version": "1.0"}
                }));
            }
            "initialized" => {
                write(&out, &json!({"jsonrpc": "2.0", "id": "srv-reg", "method": "client/registerCapability",
                    "params": {"registrations": []}}));
                notify(&out, "$/progress", json!({"token": "idx", "value": {"kind": "begin", "title": "Indexing"}}));
                notify(&out, "$/progress", json!({"token": "idx", "value": {"kind": "end"}}));
                notify(&out, "window/showMessage", json!({"type": 3, "message": "hello from fake"}));
            }
            "textDocument/didOpen" => {
                let td = &params["textDocument"];
                notify(&out, "textDocument/publishDiagnostics", json!({
                    "uri": td["uri"], "version": td["version"],
                    "diagnostics": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                        "severity": 1, "source": "fake", "message": "fake error"}]
                }));
            }
            "textDocument/didChange" => {
                did_change_count += 1;
                last_text = params["contentChanges"][0]["text"].as_str().unwrap_or_default().to_string();
            }
            "textDocument/didSave" => {
                log(&out, &format!("saved with text: {}", params["text"].as_str().unwrap_or("<none>")));
            }
            "textDocument/didClose" => {
                notify(&out, "textDocument/publishDiagnostics", json!({"uri": uri, "diagnostics": []}));
            }
            "fake/stats" => reply(json!({"didChange": did_change_count, "lastText": last_text})),
            "fake/slow" => {
                let out = out.clone();
                let id = id.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    write(&out, &json!({"jsonrpc": "2.0", "id": id, "result": "slow done"}));
                });
            }
            "fake/never" => {}
            "fake/crash" => std::process::exit(3),
            "textDocument/completion" => reply(json!({"isIncomplete": false, "items": [
                {"label": "alpha", "kind": 3, "data": 1},
                {"label": "beta", "kind": 6, "insertText": "beta($1)", "insertTextFormat": 2}
            ]})),
            "completionItem/resolve" => {
                let mut item = params.clone();
                item["detail"] = json!(format!("resolved {}", params["label"].as_str().unwrap_or("")));
                reply(item);
            }
            "textDocument/hover" => reply(json!({"contents": {"kind": "markdown",
                "value": format!("**hover** {}:{}", pos["line"], pos["character"])}})),
            "textDocument/definition" => reply(json!([{
                "targetUri": uri,
                "targetRange": {"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 0}},
                "targetSelectionRange": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 9}}
            }])),
            "textDocument/references" => reply(json!([
                {"uri": uri, "range": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 9}}},
                {"uri": uri, "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 3}}}
            ])),
            "textDocument/rename" => {
                let mut changes = serde_json::Map::new();
                changes.insert(uri.as_str().unwrap_or_default().to_string(), json!([{"range":
                    {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 9}}, "newText": params["newName"]}]));
                reply(json!({"changes": changes}));
            }
            "textDocument/documentSymbol" => reply(json!([{"name": "foo", "kind": 13,
                "location": {"uri": uri, "range": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 9}}}}])),
            "workspace/symbol" => reply(json!([{"name": format!("sym_{}", params["query"].as_str().unwrap_or("")), "kind": 12,
                "location": {"uri": "file:///tmp/x.ts", "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}}}])),
            "textDocument/codeAction" => {
                let n = params["context"]["diagnostics"].as_array().map_or(0, Vec::len);
                reply(json!([{"title": format!("Fix it ({n} diags)"), "kind": "quickfix"}]));
            }
            "textDocument/formatting" => reply(json!([{"range": {"start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 0}}, "newText": format!("// tab {}\n", params["options"]["tabSize"])}])),
            "textDocument/signatureHelp" => reply(json!({"signatures": [{"label": "foo(a: number)",
                "parameters": [{"label": "a: number"}]}], "activeSignature": 0, "activeParameter": 0})),
            "shutdown" => reply(Value::Null),
            "exit" => std::process::exit(0),
            _ if id.is_some() => write(&out, &json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("unknown {method}")}})),
            _ => {}
        }
    }
}
