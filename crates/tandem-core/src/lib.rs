//! Shared types for every Tandem crate. No UI, no async runtime here; the only I/O is the
//! small `fs` helper module.
//! Backend crates depend on this crate only; `tandem-app` depends on everything.

pub mod ids;
pub mod agent;
pub mod permission;
pub mod diff;
pub mod quota;
pub mod bus;
pub mod task;
pub mod fs;
pub mod fmt;

pub use ids::*;
pub use agent::*;
pub use permission::*;
pub use diff::*;
pub use quota::*;
pub use bus::*;
pub use task::*;
