//! Per-agent quota providers.

pub mod acp;
pub mod claude;
pub mod claude_logs;
pub mod codex;
pub mod codex_logs;
pub mod gemini;
pub mod goose;

use crate::model::{Source, Unit, Window, WindowKind};
use crate::provider::QuotaProvider;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::sync::Arc;
use tandem_core::{AgentDescriptor, QuotaKind, Usage};

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
        ..Default::default()
    };
    (usage != Usage::default()).then_some(usage)
}

/// Informational "session" token window, plus "session cost" and "context" (window fill
/// from ACP `usage_update`) when known.
pub(crate) fn session_windows(total: &Usage, source: Source, now: DateTime<Utc>) -> Vec<Window> {
    let mut v = vec![];
    let tokens = total.total();
    if tokens > 0 || (total.cost_usd.is_none() && total.context_used.is_none()) {
        v.push(Window::new("session", Unit::Tokens, source, now).with_kind(WindowKind::Usage).with_amounts(Some(tokens as f64), None));
    }
    if let Some(c) = total.cost_usd {
        v.push(Window::new("session cost", Unit::Usd, source, now).with_kind(WindowKind::Usage).with_amounts(Some(c), None));
    }
    if total.context_used.is_some() || total.context_size.is_some() {
        let (used, size) = (total.context_used.map(|u| u as f64), total.context_size.map(|s| s as f64));
        v.push(Window::new("context", Unit::Tokens, source, now).with_kind(WindowKind::Context).with_amounts(used, size));
    }
    v
}

/// Parses `AgentEvent::Raw` JSON, `None` when it isn't JSON.
pub(crate) fn raw_json(json: &str) -> Option<Value> {
    serde_json::from_str(json).ok()
}

/// Whether providers may touch the machine (vendor logs, `codex app-server`, Goose's db).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Live,
    /// In-memory only: stream events, no log scans, no subprocesses (tests, UI fixtures).
    Offline,
}

/// The quota provider for an agent, chosen by `desc.quota_kind`, already started.
/// `None` for [`QuotaKind::None`]. Must be called inside a tokio runtime.
pub fn for_agent(desc: &AgentDescriptor, mode: Mode) -> Option<Arc<dyn QuotaProvider>> {
    let offline = mode == Mode::Offline;
    Some(match desc.quota_kind {
        QuotaKind::Claude => {
            let c = claude::ClaudeProvider::new(claude::ClaudeOptions { offline, agent: desc.id.clone(), ..Default::default() });
            c.start();
            c
        }
        QuotaKind::Codex => {
            let c = codex::CodexProvider::new(codex::CodexOptions { offline, agent: desc.id.clone(), ..Default::default() });
            if !offline {
                let c2 = c.clone();
                tokio::spawn(async move {
                    let _ = c2.start().await;
                });
            }
            c
        }
        QuotaKind::Gemini => Arc::new(gemini::GeminiProvider::new(desc.id.clone())),
        QuotaKind::Goose => {
            let g = goose::GooseProvider::for_agent(desc.id.clone(), if offline { None } else { goose::default_db_path() });
            g.refresh();
            Arc::new(g)
        }
        QuotaKind::AcpGeneric => Arc::new(acp::AcpGenericProvider::new(desc.id.clone())),
        QuotaKind::None => return None,
    })
}
