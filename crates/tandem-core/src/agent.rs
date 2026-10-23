use crate::{AgentId, PermissionRequest, SessionId, TaskId, ToolCallId, Usage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// How an agent is launched. Always the vendor's unmodified binary.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
