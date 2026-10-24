use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: Option<f64>,
    /// Tokens currently in the context window, when the agent reports it (ACP `usage_update`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_used: Option<u64>,
    /// Size of the context window, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u64>,
}
impl Usage {
    /// Input + output + cache read + cache write tokens.
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
    /// Sums token counts and costs; context fields take the latest reported value.
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens; self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens; self.cache_write_tokens += o.cache_write_tokens;
        self.cost_usd = match (self.cost_usd, o.cost_usd) { (Some(a), Some(b)) => Some(a + b), (a, None) => a, (None, b) => b };
        self.context_used = o.context_used.or(self.context_used);
        self.context_size = o.context_size.or(self.context_size);
    }
}

/// Router input: what the task needs; output: ranked agents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase { Plan, Implement, Test, Review, Any }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_sums_tokens_and_keeps_latest_context() {
        let mut u = Usage { input_tokens: 1, output_tokens: 2, cost_usd: Some(0.5), context_used: Some(100), context_size: Some(1000), ..Default::default() };
        u.add(&Usage { input_tokens: 10, cache_read_tokens: 3, context_used: Some(40), ..Default::default() });
        assert_eq!((u.input_tokens, u.output_tokens, u.cache_read_tokens), (11, 2, 3));
        assert_eq!(u.total(), 16);
        assert_eq!(u.cost_usd, Some(0.5));
        // Latest reported context wins, even when smaller (compaction); unreported keeps old.
        assert_eq!((u.context_used, u.context_size), (Some(40), Some(1000)));
        u.add(&Usage { cost_usd: Some(0.25), ..Default::default() });
        assert_eq!(u.cost_usd, Some(0.75));
        assert_eq!(u.context_used, Some(40));
    }

    #[test]
    fn legacy_json_without_context_fields() {
        let u: Usage = serde_json::from_str(r#"{"input_tokens":5,"output_tokens":6,"cache_read_tokens":0,"cache_write_tokens":0,"cost_usd":null}"#).unwrap();
        assert_eq!((u.input_tokens, u.context_used, u.context_size), (5, None, None));
        let s = serde_json::to_string(&u).unwrap();
        assert!(!s.contains("context_used"), "{s}");
    }
}
