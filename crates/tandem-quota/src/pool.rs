//! `QuotaPool`: aggregates providers into one `PoolState` broadcast, persisted so the
//! gauge is populated at launch.

use crate::model::{merge, QuotaSnapshot};
use crate::persist;
use crate::provider::{Broadcaster, QuotaProvider};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tandem_core::{AgentEvent, AgentId, Receiver};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PoolState {
    pub snapshots: BTreeMap<AgentId, QuotaSnapshot>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl PoolState {
    pub fn expire(&mut self, now: DateTime<Utc>) {
        for s in self.snapshots.values_mut() {
            s.expire(now);
        }
    }
    /// Merges one agent snapshot in.
    pub fn upsert(&mut self, snap: QuotaSnapshot) {
        let merged = match self.snapshots.get(&snap.agent) {
            Some(old) => merge(old, &snap),
            None => snap,
        };
        self.updated_at = Some(self.updated_at.map_or(merged.observed_at, |u| u.max(merged.observed_at)));
        self.snapshots.insert(merged.agent.clone(), merged);
    }
}

pub struct QuotaPool {
    providers: Mutex<BTreeMap<AgentId, Arc<dyn QuotaProvider>>>,
    state: Mutex<PoolState>,
    bus: Broadcaster<PoolState>,
    persist_path: Option<PathBuf>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl QuotaPool {
    /// In-memory pool (no persistence).
    pub fn new() -> Arc<Self> {
        Self::build(None, PoolState::default())
    }
    /// Pool that loads `path` at start and rewrites it on every change.
    pub fn with_persistence(path: PathBuf) -> Arc<Self> {
        let mut st = persist::load_state(&path).unwrap_or_default();
        st.expire(Utc::now());
        Self::build(Some(path), st)
    }
    /// Pool persisted at `~/.tandem/quota/state.json`.
    pub fn with_default_persistence() -> Arc<Self> {
        Self::with_persistence(persist::state_path())
    }
    fn build(persist_path: Option<PathBuf>, state: PoolState) -> Arc<Self> {
        Arc::new(Self {
            providers: Mutex::new(BTreeMap::new()),
            state: Mutex::new(state),
            bus: Broadcaster::default(),
            persist_path,
            tasks: Mutex::new(Vec::new()),
        })
    }

    /// Registers a provider and forwards its updates. Must be called inside a tokio runtime.
    pub fn add(self: &Arc<Self>, provider: Arc<dyn QuotaProvider>) {
        let rx = provider.subscribe();
        self.providers.lock().insert(provider.agent(), provider.clone());
        let weak = Arc::downgrade(self);
        let h = tokio::spawn(async move {
            let first = provider.snapshot().await;
            match weak.upgrade() {
                Some(pool) => pool.update(first),
                None => return,
            }
            while let Ok(snap) = rx.recv().await {
                let Some(pool) = weak.upgrade() else { return };
                pool.update(snap);
            }
        });
        self.tasks.lock().push(h);
    }

    /// Merges one snapshot, broadcasts and persists the pool.
    pub fn update(&self, snap: QuotaSnapshot) {
        let st = {
            let mut st = self.state.lock();
            st.upsert(snap);
            st.clone()
        };
        if let Some(p) = &self.persist_path
            && let Err(e) = persist::save_state(p, &st) {
                tracing::warn!("quota state save failed: {e}");
            }
        self.bus.send(&st);
    }

    /// Current pool with expired windows dropped.
    pub fn state(&self) -> PoolState {
        let mut st = self.state.lock().clone();
        st.expire(Utc::now());
        st
    }
    pub fn subscribe(&self) -> Receiver<PoolState> {
        self.bus.subscribe()
    }
    pub fn provider(&self, agent: &AgentId) -> Option<Arc<dyn QuotaProvider>> {
        self.providers.lock().get(agent).cloned()
    }
    /// Routes a task event to the agent's provider.
    pub fn ingest(&self, agent: &AgentId, ev: &AgentEvent) {
        if let Some(p) = self.provider(agent) {
            p.ingest_event(ev);
        }
    }
    pub async fn reconcile_all(&self) {
        let ps: Vec<_> = self.providers.lock().values().cloned().collect();
        futures::future::join_all(ps.iter().map(|p| p.reconcile())).await;
    }
}

impl Drop for QuotaPool {
    fn drop(&mut self) {
        for t in self.tasks.lock().drain(..) {
            t.abort();
        }
    }
}
