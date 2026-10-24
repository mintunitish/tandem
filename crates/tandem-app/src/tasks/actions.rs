//! Workspace-level actions the cockpit side exposes to the rest of the app.

use gpui_kit::*;
use serde::Deserialize;
use std::path::PathBuf;

/// Open a Terminal tab running `program args…` in `cwd` (agent "Sign in" uses this).
/// Handled by the Workspace: shows the Terminal tool window and queues the session in
/// [`crate::tasks::terminal`] for the terminal panel to adopt.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = tandem, no_json)]
pub struct OpenTerminalWith {
    pub title: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

/// Agent Blame for one line: which task/agent/prompt produced it. Dispatch from the editor
/// gutter; the Workspace answers with a notification and selects the task in the cockpit.
/// For a synchronous lookup use [`crate::tasks::blame::lookup`].
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = tandem, no_json)]
pub struct ShowAgentBlame {
    /// Absolute, or relative to the project root.
    pub path: PathBuf,
    /// 1-based line.
    pub line: u32,
}
