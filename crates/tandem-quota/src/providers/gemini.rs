//! Gemini CLI (ACP): per-session token usage from `AgentEvent::Usage`. Quota is TUI-only
//! → status Unknown.

use super::session_windows;
use crate::model::Source;
use crate::provider::{ProviderCore, QuotaProvider};
use chrono::Utc;
use parking_lot::Mutex;
use tandem_core::{AgentEvent, AgentId, Usage};

pub struct GeminiProvider {
    core: ProviderCore,
    session: Mutex<Usage>,
}

impl GeminiProvider {
    pub fn new(agent: AgentId) -> Self {
        Self { core: ProviderCore::new(agent), session: Mutex::new(Usage::default()) }
    }
}

impl QuotaProvider for GeminiProvider {
    fn core(&self) -> &ProviderCore {
        &self.core
    }
    fn ingest_event(&self, ev: &AgentEvent) {
        let AgentEvent::Usage(u) = ev else { return };
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
