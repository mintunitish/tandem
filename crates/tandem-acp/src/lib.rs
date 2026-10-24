//! tandem-acp: ACP client that spawns unmodified vendor agent binaries and streams
//! [`tandem_core::AgentEvent`]s. See docs/specs/2026-10-24-tandem-design.md.

pub mod client;
pub mod launch;
pub mod log;
mod map;
pub mod registry;

pub use client::{AcpAgent, AgentHandle, DefaultClassifier, McpServerSpec, PermissionClassifier};
pub use launch::{SpawnOptions, child_env, reset_stop, stop_all};
pub use log::ReplayLog;
pub use registry::{Availability, DetectedAgent, builtin, detect_installed, get};
