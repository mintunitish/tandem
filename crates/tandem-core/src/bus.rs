//! Runtime-agnostic channels. Backend crates run on tokio; the gpui app polls these
//! from its own executor. `async-channel` works on both without adapters.
pub use async_channel::{bounded, unbounded, Receiver, Sender, TryRecvError};

use crate::{AgentEvent, TaskId};

#[derive(Clone, Debug)]
pub struct TaskEvent { pub task: TaskId, pub event: AgentEvent }

pub type EventTx = Sender<TaskEvent>;
pub type EventRx = Receiver<TaskEvent>;

/// Default capacity of a task's event channel.
pub const EVENT_CAPACITY: usize = 8192;

/// Bounded task event channel. When it is full, senders wait: the ACP reader stops
/// pulling lines from the agent's stdout until the consumer catches up (backpressure),
/// instead of growing memory without limit. Consumers must therefore drain promptly and
/// never block on slow I/O between receives.
pub fn bounded_events(cap: usize) -> (EventTx, EventRx) {
    bounded(cap.max(1))
}
