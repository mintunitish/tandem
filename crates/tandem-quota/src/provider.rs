//! Provider trait and the shared state every provider embeds.

use crate::model::{merge, QuotaSnapshot};
use chrono::Utc;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use tandem_core::{bounded, AgentEvent, AgentId, Receiver, Sender};

/// One agent's quota source. Object-safe: `reconcile` returns a boxed future
/// (an `async fn` in the trait would not be usable as `Arc<dyn QuotaProvider>`).
pub trait QuotaProvider: Send + Sync {
    /// The shared snapshot state + broadcast every provider embeds.
    fn core(&self) -> &ProviderCore;
    fn agent(&self) -> AgentId {
        self.core().agent()
    }
    /// Current merged snapshot, with expired windows dropped.
    fn snapshot(&self) -> QuotaSnapshot {
        self.core().current()
    }
    /// Every change as a full snapshot. Each call returns an independent receiver.
    fn subscribe(&self) -> Receiver<QuotaSnapshot> {
        self.core().subscribe()
    }
    /// Feed one event of this agent's task stream (stream-json taps, ACP usage, ...).
    fn ingest_event(&self, ev: &AgentEvent);
    /// Re-read the authoritative source (e.g. after a turn ends). Default: nothing.
    fn reconcile(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// Fan-out of latest values to any number of subscribers. Slow subscribers miss updates
/// (each message is a full state, so the next one supersedes it).
pub struct Broadcaster<T: Clone> {
    subs: Mutex<Vec<Sender<T>>>,
}
impl<T: Clone> Default for Broadcaster<T> {
    fn default() -> Self {
        Self { subs: Mutex::new(Vec::new()) }
    }
}
impl<T: Clone> Broadcaster<T> {
    pub fn subscribe(&self) -> Receiver<T> {
        let (tx, rx) = bounded(32);
        self.subs.lock().push(tx);
        rx
    }
    pub fn send(&self, v: &T) {
        self.subs.lock().retain(|tx| !tx.is_closed() && !matches!(tx.try_send(v.clone()), Err(e) if e.is_closed()));
    }
}

/// Snapshot state + broadcast, embedded in every provider.
pub struct ProviderCore {
    agent: AgentId,
    state: Mutex<QuotaSnapshot>,
    bus: Broadcaster<QuotaSnapshot>,
}

impl ProviderCore {
    pub fn new(agent: AgentId) -> Self {
        Self { state: Mutex::new(QuotaSnapshot::empty(agent.clone(), Utc::now())), agent, bus: Broadcaster::default() }
    }
    pub fn agent(&self) -> AgentId {
        self.agent.clone()
    }
    /// Merges a partial observation into the state and broadcasts the result.
    pub fn apply(&self, mut delta: QuotaSnapshot) -> QuotaSnapshot {
        // Parsers stamp their default agent id; the provider's own id wins.
        delta.agent = self.agent.clone();
        let merged = {
            let mut st = self.state.lock();
            *st = merge(&st, &delta);
            st.clone()
        };
        self.bus.send(&merged);
        merged
    }
    pub fn set_error(&self, err: impl Into<String>) {
        let snap = {
            let mut st = self.state.lock();
            st.error = Some(err.into());
            st.clone()
        };
        self.bus.send(&snap);
    }
    /// Raw state (no expiry).
    pub fn raw(&self) -> QuotaSnapshot {
        self.state.lock().clone()
    }
    /// State with expired windows dropped.
    pub fn current(&self) -> QuotaSnapshot {
        let mut s = self.raw();
        s.expire(Utc::now());
        s
    }
    pub fn subscribe(&self) -> Receiver<QuotaSnapshot> {
        self.bus.subscribe()
    }
    /// Empty delta stamped now, for building partial updates.
    pub fn delta(&self) -> QuotaSnapshot {
        QuotaSnapshot::empty(self.agent.clone(), Utc::now())
    }
}
