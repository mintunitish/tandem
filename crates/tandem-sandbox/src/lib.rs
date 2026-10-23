//! tandem-sandbox: the safety runtime ("Kill Switch") for Tandem.
//!
//! - [`classify`]: risk classification of agent tool calls (Safe / Review / Destructive).
//! - [`seatbelt`]: macOS `sandbox-exec` profile generation and command wrapping.
//! - [`env`]: environment scrubbing for agent processes.
//! - [`kill`]: process-group registry with a global STOP.
//! - [`mcp_pin`]: hash-pinning of MCP tool definitions.
//!
//! See docs/specs/2026-10-24-tandem-design.md, "Security model" and "Kill Switch".

pub mod classify;
pub mod env;
pub mod kill;
pub mod mcp_pin;
pub mod paths;
pub mod seatbelt;
pub mod shell;

pub use classify::Classifier;
pub use kill::KillSwitch;
pub use mcp_pin::{PinStatus, PinStore};
pub use seatbelt::SandboxSpec;
