use crate::{AgentId, Decider, TaskId};
use serde::{Deserialize, Serialize};

/// Who produced a DiffSet and why (review conflicts, agent blame).
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
