//! Quota pool wiring: one provider per detected agent, chosen by its descriptor's
//! `quota_kind` (`tandem_quota::providers::for_agent`).

use std::sync::Arc;
use tandem_core::AgentDescriptor;
use tandem_quota::QuotaPool;
use tandem_quota::providers::{self, Mode};

/// How quota providers run, set once by `app::init` (`TANDEM_QUOTA_OFFLINE=1` → `Mode::Offline`:
/// providers stay in-memory and the pool is not persisted).
pub struct QuotaMode(pub Mode);

impl gpui_kit::Global for QuotaMode {}

impl QuotaMode {
    pub fn from_env() -> Self {
        Self(match std::env::var_os("TANDEM_QUOTA_OFFLINE") {
            Some(v) if !v.is_empty() && v != "0" => Mode::Offline,
            _ => Mode::Live,
        })
    }
}

/// Build the pool and register providers on the tokio runtime. The UI polls
/// [`QuotaPool::state`] (no cross-thread wakeups into gpui).
pub fn start_pool(agents: Vec<AgentDescriptor>, mode: Mode) -> Arc<QuotaPool> {
    let _guard = crate::runtime::tokio().enter();
    let pool = QuotaPool::new(mode == Mode::Live);
    let p = pool.clone();
    crate::runtime::tokio().spawn(async move {
        for d in &agents {
            if let Some(provider) = providers::for_agent(d, mode) {
                p.add(provider);
            }
        }
    });
    pool
}
