//! tandem-review: pure review logic over [`DiffSet`]s (no git, no UI).
//!
//! * [`state`]: per-hunk accept/reject, navigation, materialization of accepted hunks.
//! * [`symbols`]: tree-sitter symbol lookup ("what did this hunk touch").
//! * [`conflicts`]: intent-conflict detection across concurrent agent tasks.
//! * [`blame`]: agent blame entries with line-shift bookkeeping.
//! * [`summary`]: counts, touched symbols and risk flags for a DiffSet.
//!
//! See `docs/specs/2026-10-24-tandem-design.md` ("Review", "Agent Blame").

pub mod blame;
pub mod conflicts;
pub mod hunks;
pub mod state;
pub mod summary;
pub mod symbols;

pub use blame::{BlameIndex, blame_entries, merge_blame};
pub use conflicts::{ConflictKind, ConflictSide, IntentConflict, Severity, detect};
pub use hunks::diff_texts;
pub use state::{Decision, HunkRef, ReviewCounts, ReviewState};
pub use summary::{ReviewSummary, describe};
pub use symbols::{Lang, LineRange, SymbolKind, SymbolRef, enclosing_symbols};

use serde::{Deserialize, Serialize};
use tandem_core::{AgentId, Decider, TaskId};

/// Who produced a DiffSet and why. Proposed for `tandem-core` (see crate report).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskMeta {
    pub task: TaskId,
    pub agent: AgentId,
    /// The user's prompt for the task. Conflicts show its first line,
    /// blame stores its first 120 characters.
    pub intent: String,
    /// Who approved the edit permission, when known (for blame).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_by: Option<Decider>,
}

impl TaskMeta {
    pub fn new(task: impl Into<TaskId>, agent: impl Into<AgentId>, intent: impl Into<String>) -> Self {
        Self { task: task.into(), agent: agent.into(), intent: intent.into(), permission_by: None }
    }

    /// First non-empty line of the prompt, trimmed.
    pub fn intent_line(&self) -> &str {
        self.intent.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
    }

    /// First 120 characters of the prompt (char-safe), whitespace collapsed.
    pub fn prompt_excerpt(&self) -> String {
        let collapsed = self.intent.split_whitespace().collect::<Vec<_>>().join(" ");
        collapsed.chars().take(120).collect()
    }
}
