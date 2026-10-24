use crate::{AgentId, PermissionRequest, SessionId, TaskId, ToolCallId, Usage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Which quota provider tracks an agent's plan usage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaKind {
    Codex,
    Claude,
    /// Generic ACP usage updates (the default for unknown agents).
    #[default]
    AcpGeneric,
    Gemini,
    Goose,
    /// No quota tracking.
    None,
}

/// How an agent is launched. Always the vendor's unmodified binary.
///
/// The capability fields below (env allow-list, config dirs, strict-permission launch,
/// login command, badge, quota provider) are filled for built-ins by
/// `tandem_acp::registry`; custom agents may set them in `.tandem/agents.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub id: AgentId,
    pub display_name: String,
    pub vendor: String,
    /// Executable name or absolute path.
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Shown when the binary is missing, e.g. "npm i -g @zed-industries/claude-agent-acp".
    pub install_hint: String,
    /// True when the command speaks ACP on stdio.
    pub acp: bool,
    /// Where the user's subscription login lives, informational only. Never read.
    pub auth_note: String,
    /// Vendor env var prefixes the agent inherits (e.g. `ANTHROPIC_`).
    #[serde(default)]
    pub env_prefixes: Vec<String>,
    /// Vendor env var names the agent inherits (e.g. `GITHUB_TOKEN`).
    #[serde(default)]
    pub env_exact: Vec<String>,
    /// The agent's own config/state dirs, relative to `$HOME` (writable in the sandbox).
    #[serde(default)]
    pub home_dirs: Vec<String>,
    /// Extra args that make the agent ask (ACP `session/request_permission`) instead of
    /// auto-approving shell commands and out-of-worktree writes.
    #[serde(default)]
    pub strict_args: Vec<String>,
    /// Extra env for the same purpose, set after scrubbing.
    #[serde(default)]
    pub strict_env: BTreeMap<String, String>,
    /// Vendor login command (program + args) for "Sign in".
    #[serde(default)]
    pub login_command: Option<Vec<String>>,
    /// Avatar letters and color as hex `RRGGBB`.
    #[serde(default)]
    pub badge: (String, String),
    #[serde(default)]
    pub quota_kind: QuotaKind,
}

/// Stdio MCP server handed to the agent at `session/new`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerSpec {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role { User, Assistant, Thought }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind { Read, Edit, Delete, Move, Search, Execute, Think, Fetch, Other }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus { Pending, InProgress, Completed, Failed }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub title: String,
    pub kind: ToolKind,
    pub status: ToolStatus,
    /// Files touched, when the agent reports them.
    pub locations: Vec<PathBuf>,
    /// Raw command or input, when known (shell command, SQL, URL).
    pub raw_input: Option<String>,
    /// Content as text (diff, output); ACP v2 structured diffs land in `diff`.
    pub content: Option<String>,
    pub diff: Option<crate::FileDiff>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason { EndTurn, MaxTokens, Cancelled, Refusal, Error }

/// Everything the UI, replay log and harness consume. One stream per task.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionStarted { session: SessionId, agent: AgentId, cwd: PathBuf },
    TextDelta { role: Role, text: String },
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCall),
    Plan { entries: Vec<PlanEntry> },
    PermissionRequested(PermissionRequest),
    PermissionResolved { request: crate::RequestId, option: String, by: Decider },
    Usage(Usage),
    Done { reason: StopReason },
    Error { message: String },
    /// Raw JSON-RPC line, kept for the flight recorder. Not shown by default.
    Raw { direction: Direction, json: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction { ToAgent, FromAgent }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decider { User, Policy, Harness }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanEntry { pub content: String, pub status: ToolStatus, pub priority: Option<String> }

/// Timestamped envelope written to `.tandem/tasks/<task>/log.jsonl`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogRecord { pub at: DateTime<Utc>, pub task: TaskId, pub event: AgentEvent }
