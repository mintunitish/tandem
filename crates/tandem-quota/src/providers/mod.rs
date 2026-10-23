//! Per-agent quota providers.

pub mod acp;
pub mod claude;
pub mod claude_logs;
pub mod codex;
pub mod codex_logs;
pub mod gemini;
pub mod goose;

use crate::model::{Source, Unit, Window, WindowKind};
use chrono::{DateTime, Utc};
use serde_json::Value;
use tandem_core::Usage;

/// First present, non-null key.
pub(crate) fn get<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k).filter(|x| !x.is_null()))
}
pub(crate) fn get_f64(v: &Value, keys: &[&str]) -> Option<f64> {
    get(v, keys).and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
}
pub(crate) fn get_u64(v: &Value, keys: &[&str]) -> u64 {
    get_f64(v, keys).map(|f| f.max(0.0) as u64).unwrap_or(0)
}

/// Token usage from any vendor's usage object (Claude `message.usage`, Codex snake or camel case).
pub fn usage_from_json(u: &Value) -> Option<Usage> {
    if !u.is_object() {
        return None;
    }
    let usage = Usage {
        input_tokens: get_u64(u, &["input_tokens", "inputTokens", "prompt_tokens"]),
        output_tokens: get_u64(u, &["output_tokens", "outputTokens", "completion_tokens"]),
        cache_read_tokens: get_u64(u, &["cache_read_input_tokens", "cached_input_tokens", "cachedInputTokens", "cacheReadInputTokens"]),
        cache_write_tokens: get_u64(u, &["cache_creation_input_tokens", "cache_write_input_tokens", "cacheWriteInputTokens", "cacheCreationInputTokens"]),
        cost_usd: get_f64(u, &["total_cost_usd", "cost_usd", "costUSD"]),
    };
    (usage != Usage::default()).then_some(usage)
}

pub fn usage_total(u: &Usage) -> u64 {
    u.input_tokens + u.output_tokens + u.cache_read_tokens + u.cache_write_tokens
}

/// Informational "session" token window (+ "session cost" when known).
pub(crate) fn session_windows(total: &Usage, source: Source, now: DateTime<Utc>) -> Vec<Window> {
    let mut v = vec![Window::new("session", Unit::Tokens, source, now)
        .with_kind(WindowKind::Usage)
        .with_amounts(Some(usage_total(total) as f64), None)];
    if let Some(c) = total.cost_usd {
        v.push(Window::new("session cost", Unit::Usd, source, now).with_kind(WindowKind::Usage).with_amounts(Some(c), None));
    }
    v
}

/// Parses `AgentEvent::Raw` JSON, `None` when it isn't JSON.
pub(crate) fn raw_json(json: &str) -> Option<Value> {
    serde_json::from_str(json).ok()
}
