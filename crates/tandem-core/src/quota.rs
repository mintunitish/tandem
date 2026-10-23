use crate::AgentId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: Option<f64>,
}
impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens; self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens; self.cache_write_tokens += o.cache_write_tokens;
        self.cost_usd = match (self.cost_usd, o.cost_usd) { (Some(a), Some(b)) => Some(a + b), (a, None) => a, (None, b) => b };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaSource {
    /// Vendor reported the number (e.g. Codex app-server rate limits).
    Official,
    /// Derived from the agent's own local logs or stream events.
    Derived,
    /// Our estimate from history.
    Estimated,
}

/// One rolling window of a plan, e.g. Claude "5-hour", "weekly"; Codex "5h", "weekly"; API "monthly budget".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuotaWindow {
    pub agent: AgentId,
    pub label: String,
    /// 0.0..=1.0 fraction used when known.
    pub used_fraction: Option<f64>,
    pub used_tokens: Option<u64>,
    pub limit_tokens: Option<u64>,
    pub reset_at: Option<DateTime<Utc>>,
    pub source: QuotaSource,
    pub observed_at: DateTime<Utc>,
}
impl QuotaWindow {
    pub fn headroom(&self) -> Option<f64> { self.used_fraction.map(|u| (1.0 - u).clamp(0.0, 1.0)) }
}

/// Router input: what the task needs; output: ranked agents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase { Plan, Implement, Test, Review, Any }
