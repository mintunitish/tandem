//! tandem-harness: conformance scenarios and repo benchmarks across agents.
//! See docs/specs/2026-10-24-tandem-design.md ("Harness", "Repo Leaderboard").
//!
//! - [`scenario`]: YAML formats (`harness/scenarios`, `harness/bench`) and `validate`.
//! - [`runner`]: the [`AgentRunner`] seam plus [`MockRunner`] and permission deciders.
//! - [`run`]: fresh fixture worktree per run, event capture, [`conformance`] / [`bench`].
//! - [`eval`]: expectation evaluators.
//! - [`report`]: JSON + markdown report, leaderboard, SVG chart.

pub mod acp_runner;
pub mod eval;
pub mod report;
pub mod run;
pub mod runner;
pub mod scenario;

pub use acp_runner::AcpRunner;
pub use report::{LeaderRow, Report, RunKind, RunResult, Status, leaderboard_svg, render_dir};
pub use run::{CANARY_SECRET_ENV, CANARY_SECRET_VALUE, RunOptions, bench, conformance};
pub use runner::{AgentRunner, MockRunner, MockStep, PermissionDecider, RunOutcome};
pub use scenario::{BenchTask, Check, Scenario, load_bench, load_scenarios, validate};
