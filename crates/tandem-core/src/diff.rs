use crate::{AgentId, TaskId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind { Context, Added, Removed }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffLine { pub kind: LineKind, pub old_no: Option<u32>, pub new_no: Option<u32>, pub text: String }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hunk {
    pub old_start: u32, pub old_lines: u32,
    pub new_start: u32, pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange { Added, Modified, Deleted, Renamed }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: PathBuf,
    pub old_path: Option<PathBuf>,
    pub change: FileChange,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
}

/// All changes a task produced: worktree vs its base commit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffSet { pub task: TaskId, pub agent: AgentId, pub base_commit: String, pub files: Vec<FileDiff> }

/// Agent blame: who produced a line and from which prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlameEntry {
    pub path: PathBuf,
    pub new_start: u32, pub new_lines: u32,
    pub task: TaskId,
    pub agent: AgentId,
    pub prompt_excerpt: String,
    pub permission_by: Option<crate::Decider>,
}
