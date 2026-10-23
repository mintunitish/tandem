//! Gemini CLI / Antigravity: `-o stream-json` `result` event `stats` (token counts;
//! sub-fields undocumented, parsed defensively). Quota is TUI-only → status Unknown.

use super::{get, get_u64, raw_json, session_windows, usage_from_json};
use crate::model::{QuotaSnapshot, Source};
use crate::provider::{ProviderCore, QuotaProvider};
use chrono::Utc;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::Value;
use tandem_core::{AgentEvent, AgentId, Receiver, Usage};

/// `result.stats` → turn usage. Accepts flat `{input_tokens, output_tokens, ...}`,
/// `{models:{<m>:{tokens:{prompt|input, candidates|output, cached}}}}`, or `total_tokens` only.
pub fn parse_result_stats(v: &Value) -> Option<Usage> {
    if v.get("type").and_then(|t| t.as_str()) != Some("result") {
        return None;
    }
    let stats = get(v, &["stats", "usage"])?;
    if let Some(models) = stats.get("models").and_then(|m| m.as_object()) {
        let mut total = Usage::default();
        for m in models.values() {
            let t = m.get("tokens").unwrap_or(m);
            total.add(&Usage {
                input_tokens: get_u64(t, &["prompt", "input", "input_tokens"]),
                output_tokens: get_u64(t, &["candidates", "output", "output_tokens"]),
                cache_read_tokens: get_u64(t, &["cached", "cached_tokens"]),
                ..Default::default()
            });
        }
        if total != Usage::default() {
            return Some(total);
        }
    }
    usage_from_json(stats).or_else(|| {
        let t = get_u64(stats, &["total_tokens", "totalTokens"]);
        (t > 0).then(|| Usage { input_tokens: t, ..Default::default() })
    })
}

pub struct GeminiProvider {
    core: ProviderCore,
    session: Mutex<Usage>,
}

impl GeminiProvider {
    pub fn new(agent: AgentId) -> Self {
        Self { core: ProviderCore::new(agent), session: Mutex::new(Usage::default()) }
    }
    fn add(&self, u: &Usage) {
        let total = {
            let mut s = self.session.lock();
            s.add(u);
            s.clone()
        };
        let mut d = self.core.delta();
        d.windows = session_windows(&total, Source::AgentEvent, Utc::now());
        self.core.apply(d);
    }
}

impl QuotaProvider for GeminiProvider {
    fn agent(&self) -> AgentId {
        self.core.agent()
    }
    fn snapshot(&self) -> BoxFuture<'_, QuotaSnapshot> {
        Box::pin(async move { self.core.current() })
    }
    fn subscribe(&self) -> Receiver<QuotaSnapshot> {
        self.core.subscribe()
    }
    fn ingest_event(&self, ev: &AgentEvent) {
        match ev {
            AgentEvent::Usage(u) => self.add(u),
            AgentEvent::Raw { json, .. } => {
                if let Some(u) = raw_json(json).as_ref().and_then(parse_result_stats) {
                    self.add(&u);
                }
            }
            _ => {}
        }
    }
}
