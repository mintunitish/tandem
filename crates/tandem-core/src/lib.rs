//! Shared types for every Tandem crate. No I/O, no UI, no async runtime here.
//! Backend crates depend on this crate only; `tandem-app` depends on everything.

pub mod ids;
pub mod agent;
pub mod permission;
pub mod diff;
pub mod quota;
pub mod bus;

pub use ids::*;
pub use agent::*;
pub use permission::*;
pub use diff::*;
pub use quota::*;
pub use bus::*;
