//! Any ACP agent: `AgentEvent::Usage` (tandem-acp maps `usage_update` `size`/`used` into
//! `context_size`/`context_used` and its cumulative cost into deltas). Context + spend,
//! not quota.

use super::session_windows;
use crate::model::Source;
use crate::provider::{ProviderCore, QuotaProvider};
use chrono::Utc;
use parking_lot::Mutex;
use tandem_core::{AgentEvent, AgentId, Usage};

pub struct AcpGenericProvider {
    core: ProviderCore,
    session: Mutex<Usage>,
}

impl AcpGenericProvider {
    pub fn new(agent: AgentId) -> Self {
        Self { core: ProviderCore::new(agent), session: Mutex::new(Usage::default()) }
    }
}

impl QuotaProvider for AcpGenericProvider {
    fn core(&self) -> &ProviderCore {
        &self.core
    }
    fn ingest_event(&self, ev: &AgentEvent) {
        let AgentEvent::Usage(u) = ev else { return };
        let windows = {
            let mut s = self.session.lock();
            s.add(u);
            session_windows(&s, Source::AgentEvent, Utc::now())
        };
        let mut d = self.core.delta();
        d.windows = windows;
        self.core.apply(d);
    }
}
