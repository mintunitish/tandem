//! Quota pool wiring: one provider per detected agent, chosen by its descriptor's
//! `quota_kind` (`tandem_quota::providers::for_agent`).
//!
//! `TANDEM_QUOTA_OFFLINE=1` (set by the UI tests) maps to `Mode::Offline`: every provider
//! stays in-memory (no log scans, no app-server) and the pool is not persisted.

use std::sync::Arc;
use tandem_core::{AgentDescriptor, AgentId};
use tandem_quota::QuotaPool;
use tandem_quota::providers::{self, Mode};

fn mode() -> Mode {
    match std::env::var_os("TANDEM_QUOTA_OFFLINE") {
        Some(v) if !v.is_empty() && v != "0" => Mode::Offline,
        _ => Mode::Live,
    }
}

/// Build the pool and register providers on the tokio runtime. The UI polls
/// [`QuotaPool::state`] (no cross-thread wakeups into gpui).
pub fn start_pool(agents: Vec<AgentId>) -> Arc<QuotaPool> {
    let mode = mode();
    let _guard = crate::runtime::tokio().enter();
    let pool = QuotaPool::new(mode == Mode::Live);
    let p = pool.clone();
    crate::runtime::tokio().spawn(async move {
        for agent in agents {
            // Custom agents without a built-in descriptor get the generic ACP provider.
            let desc = tandem_acp::get(agent.as_str()).unwrap_or_else(|| AgentDescriptor { id: agent, ..Default::default() });
            if let Some(provider) = providers::for_agent(&desc, mode) {
                p.add(provider);
            }
        }
    });
    pool
}
