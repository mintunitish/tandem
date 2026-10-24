//! Pure ACP schema -> tandem-core conversions.

use agent_client_protocol::schema::v1 as acp;
use std::collections::HashMap;
use tandem_core as core;

pub(crate) fn tool_kind(k: &acp::ToolKind) -> core::ToolKind {
    use core::ToolKind as K;
    match k {
        acp::ToolKind::Read => K::Read,
        acp::ToolKind::Edit => K::Edit,
        acp::ToolKind::Delete => K::Delete,
        acp::ToolKind::Move => K::Move,
        acp::ToolKind::Search => K::Search,
        acp::ToolKind::Execute => K::Execute,
        acp::ToolKind::Think => K::Think,
        acp::ToolKind::Fetch => K::Fetch,
        _ => K::Other,
    }
}

pub(crate) fn tool_status(s: &acp::ToolCallStatus) -> core::ToolStatus {
    match s {
        acp::ToolCallStatus::InProgress => core::ToolStatus::InProgress,
        acp::ToolCallStatus::Completed => core::ToolStatus::Completed,
        acp::ToolCallStatus::Failed => core::ToolStatus::Failed,
        _ => core::ToolStatus::Pending,
    }
}

pub(crate) fn stop_reason(r: &acp::StopReason) -> core::StopReason {
    match r {
        acp::StopReason::EndTurn => core::StopReason::EndTurn,
        acp::StopReason::MaxTokens | acp::StopReason::MaxTurnRequests => {
            core::StopReason::MaxTokens
        }
        acp::StopReason::Refusal => core::StopReason::Refusal,
        acp::StopReason::Cancelled => core::StopReason::Cancelled,
        _ => core::StopReason::Error,
    }
}

pub(crate) fn option_kind(k: &acp::PermissionOptionKind) -> core::PermissionOptionKind {
    use core::PermissionOptionKind as K;
    match k {
        acp::PermissionOptionKind::AllowAlways => K::AllowAlways,
        acp::PermissionOptionKind::RejectOnce => K::RejectOnce,
        acp::PermissionOptionKind::RejectAlways => K::RejectAlways,
        _ => K::AllowOnce,
    }
}

pub(crate) fn content_text(c: &acp::ContentBlock) -> String {
    match c {
        acp::ContentBlock::Text(t) => t.text.clone(),
        acp::ContentBlock::ResourceLink(l) => l.uri.clone(),
        acp::ContentBlock::Image(_) => "[image]".into(),
        acp::ContentBlock::Audio(_) => "[audio]".into(),
        acp::ContentBlock::Resource(_) => "[resource]".into(),
        _ => String::new(),
    }
}

/// Shell command / SQL / URL when the agent puts it under a well-known key, else compact JSON.
pub(crate) fn raw_input(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::String(s) => s.clone(),
        Value::Object(m) => {
            for key in ["command", "cmd", "sql", "query", "url"] {
                match m.get(key) {
                    Some(Value::String(s)) => return s.clone(),
                    Some(Value::Array(a)) => {
                        return a
                            .iter()
                            .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string))
                            .collect::<Vec<_>>()
                            .join(" ");
                    }
                    _ => {}
                }
            }
            v.to_string()
        }
        _ => v.to_string(),
    }
}

fn apply_content(call: &mut core::ToolCall, content: &[acp::ToolCallContent]) {
    let mut text = Vec::new();
    for c in content {
        match c {
            acp::ToolCallContent::Content(c) => text.push(content_text(&c.content)),
            acp::ToolCallContent::Diff(d) => {
                call.diff = Some(core::FileDiff {
                    path: d.path.clone(),
                    old_path: None,
                    change: if d.old_text.is_some() {
                        core::FileChange::Modified
                    } else {
                        core::FileChange::Added
                    },
                    old_text: d.old_text.clone(),
                    new_text: Some(d.new_text.clone()),
                    hunks: Vec::new(),
                    binary: false,
                });
            }
            acp::ToolCallContent::Terminal(t) => text.push(format!("[terminal {}]", t.terminal_id)),
            _ => {}
        }
    }
    if !text.is_empty() {
        call.content = Some(text.join("\n"));
    }
}

pub(crate) fn tool_call(t: &acp::ToolCall) -> core::ToolCall {
    let mut call = core::ToolCall {
        id: core::ToolCallId(t.tool_call_id.to_string()),
        title: t.title.clone(),
        kind: tool_kind(&t.kind),
        status: tool_status(&t.status),
        locations: t.locations.iter().map(|l| l.path.clone()).collect(),
        raw_input: t.raw_input.as_ref().map(raw_input),
        content: None,
        diff: None,
    };
    apply_content(&mut call, &t.content);
    call
}

/// Merge a partial update into the last known state of the call (or a blank one).
pub(crate) fn merge_update(
    known: Option<&core::ToolCall>,
    u: &acp::ToolCallUpdate,
) -> core::ToolCall {
    let mut call = known.cloned().unwrap_or_else(|| core::ToolCall {
        id: core::ToolCallId(u.tool_call_id.to_string()),
        title: String::new(),
        kind: core::ToolKind::Other,
        status: core::ToolStatus::Pending,
        locations: Vec::new(),
        raw_input: None,
        content: None,
        diff: None,
    });
    let f = &u.fields;
    if let Some(k) = &f.kind {
        call.kind = tool_kind(k);
    }
    if let Some(s) = &f.status {
        call.status = tool_status(s);
    }
    if let Some(t) = &f.title {
        call.title = t.clone();
    }
    if let Some(l) = &f.locations {
        call.locations = l.iter().map(|l| l.path.clone()).collect();
    }
    if let Some(r) = &f.raw_input {
        call.raw_input = Some(raw_input(r));
    }
    if let Some(c) = &f.content {
        apply_content(&mut call, c);
    }
    call
}

/// Converts session/update notifications to events, keeping tool-call state so updates carry full calls.
#[derive(Default)]
pub(crate) struct UpdateMapper {
    calls: HashMap<String, core::ToolCall>,
    last_cost: Option<f64>,
}

impl UpdateMapper {
    pub(crate) fn permission_call(&mut self, u: &acp::ToolCallUpdate) -> core::ToolCall {
        let call = merge_update(self.calls.get(u.tool_call_id.0.as_ref()), u);
        self.calls.insert(call.id.0.clone(), call.clone());
        call
    }

    pub(crate) fn map(&mut self, u: &acp::SessionUpdate) -> Option<core::AgentEvent> {
        use core::AgentEvent as E;
        Some(match u {
            acp::SessionUpdate::AgentMessageChunk(c) => E::TextDelta {
                role: core::Role::Assistant,
                text: content_text(&c.content),
            },
            acp::SessionUpdate::AgentThoughtChunk(c) => E::TextDelta {
                role: core::Role::Thought,
                text: content_text(&c.content),
            },
            acp::SessionUpdate::UserMessageChunk(c) => E::TextDelta {
                role: core::Role::User,
                text: content_text(&c.content),
            },
            acp::SessionUpdate::ToolCall(t) => {
                let call = tool_call(t);
                self.calls.insert(call.id.0.clone(), call.clone());
                E::ToolCall(call)
            }
            acp::SessionUpdate::ToolCallUpdate(t) => E::ToolCallUpdate(self.permission_call(t)),
            acp::SessionUpdate::Plan(p) => E::Plan {
                entries: p
                    .entries
                    .iter()
                    .map(|e| core::PlanEntry {
                        content: e.content.clone(),
                        status: match e.status {
                            acp::PlanEntryStatus::InProgress => core::ToolStatus::InProgress,
                            acp::PlanEntryStatus::Completed => core::ToolStatus::Completed,
                            _ => core::ToolStatus::Pending,
                        },
                        priority: serde_json::to_value(&e.priority)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string)),
                    })
                    .collect(),
            },
            // Context fill is a gauge (latest wins). ACP reports cumulative session cost;
            // core::Usage cost is additive, so emit the delta.
            acp::SessionUpdate::UsageUpdate(uu) => {
                let cost = uu
                    .cost
                    .as_ref()
                    .filter(|c| c.currency.eq_ignore_ascii_case("USD"))
                    .map(|c| c.amount);
                let cost_usd = cost.map(|c| c - self.last_cost.replace(c).unwrap_or(0.0));
                E::Usage(core::Usage {
                    cost_usd,
                    context_used: Some(uu.used),
                    context_size: Some(uu.size),
                    ..Default::default()
                })
            }
            _ => return None,
        })
    }
}

pub(crate) fn end_turn_usage(u: &acp::Usage) -> core::Usage {
    core::Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_tokens: u.cached_read_tokens.unwrap_or(0),
        cache_write_tokens: u.cached_write_tokens.unwrap_or(0),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn update(v: serde_json::Value) -> acp::SessionUpdate {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn maps_text_and_tool_calls_with_merge() {
        let mut m = UpdateMapper::default();
        let e = m.map(&update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}))).unwrap();
        assert!(
            matches!(e, core::AgentEvent::TextDelta { role: core::Role::Assistant, ref text } if text == "hi")
        );

        let e = m
            .map(&update(
                json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"Run","kind":"execute",
            "rawInput":{"command":["rm","-rf","build"]},"locations":[{"path":"/w/build"}]}),
            ))
            .unwrap();
        let core::AgentEvent::ToolCall(c) = e else {
            panic!()
        };
        assert_eq!(c.kind, core::ToolKind::Execute);
        assert_eq!(c.raw_input.as_deref(), Some("rm -rf build"));
        assert_eq!(c.locations, vec![std::path::PathBuf::from("/w/build")]);

        let e = m.map(&update(json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed",
            "content":[{"type":"content","content":{"type":"text","text":"done"}},{"type":"diff","path":"/w/a","oldText":"a","newText":"b"}]}))).unwrap();
        let core::AgentEvent::ToolCallUpdate(c) = e else {
            panic!()
        };
        assert_eq!(
            (c.title.as_str(), c.status, c.content.as_deref()),
            ("Run", core::ToolStatus::Completed, Some("done"))
        );
        assert_eq!(c.diff.unwrap().change, core::FileChange::Modified);
    }

    #[test]
    fn maps_plan_and_usage_delta() {
        let mut m = UpdateMapper::default();
        let e = m.map(&update(json!({"sessionUpdate":"plan","entries":[{"content":"x","priority":"high","status":"in_progress"}]}))).unwrap();
        let core::AgentEvent::Plan { entries } = e else {
            panic!()
        };
        assert_eq!(
            (entries[0].status, entries[0].priority.as_deref()),
            (core::ToolStatus::InProgress, Some("high"))
        );

        let u = |c: f64| {
            update(
                json!({"sessionUpdate":"usage_update","used":1,"size":2,"cost":{"amount":c,"currency":"USD"}}),
            )
        };
        m.map(&u(0.5));
        let Some(core::AgentEvent::Usage(us)) = m.map(&u(0.75)) else {
            panic!()
        };
        assert_eq!(us.cost_usd, Some(0.25));
        assert_eq!((us.context_used, us.context_size), (Some(1), Some(2)));
        let Some(core::AgentEvent::Usage(us)) = m.map(&update(json!({"sessionUpdate":"usage_update","used":7,"size":9}))) else {
            panic!()
        };
        assert_eq!((us.cost_usd, us.context_used, us.context_size), (None, Some(7), Some(9)));
    }

    #[test]
    fn maps_enums() {
        assert_eq!(
            stop_reason(&acp::StopReason::MaxTurnRequests),
            core::StopReason::MaxTokens
        );
        assert_eq!(
            option_kind(&acp::PermissionOptionKind::RejectOnce),
            core::PermissionOptionKind::RejectOnce
        );
        assert_eq!(raw_input(&json!({"url":"https://x"})), "https://x");
        assert_eq!(raw_input(&json!({"a":1})), r#"{"a":1}"#);
    }
}
