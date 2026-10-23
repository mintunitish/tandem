//! Runtime-agnostic channels. Backend crates run on tokio; the gpui app polls these
//! from its own executor. `async-channel` works on both without adapters.
pub use async_channel::{bounded, unbounded, Receiver, Sender, TryRecvError};

use crate::{AgentEvent, TaskId};

#[derive(Clone, Debug)]
pub struct TaskEvent { pub task: TaskId, pub event: AgentEvent }

pub type EventTx = Sender<TaskEvent>;
pub type EventRx = Receiver<TaskEvent>;
