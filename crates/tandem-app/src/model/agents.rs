//! Agent cockpit model. Uses `tandem_core` types directly so `tandem-acp` / `tandem-quota`
//! can feed real events through `AgentsModel::apply(TaskEvent)` later.

use chrono::Utc;
use tandem_core::{
    AgentEvent, AgentId, PermissionOption, PermissionOptionKind, PermissionRequest, QuotaSource,
    QuotaWindow, RequestId, RiskClass, Role, TaskEvent, TaskId, ToolCall, ToolCallId, ToolKind,
    ToolStatus,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    NeedsApproval,
    Done,
    Failed,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Running => "Running",
            TaskStatus::NeedsApproval => "Needs approval",
            TaskStatus::Done => "Done",
            TaskStatus::Failed => "Failed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AgentTask {
    pub id: TaskId,
    pub agent: AgentId,
    pub title: String,
    pub status: TaskStatus,
    pub elapsed: String,
    pub events: Vec<AgentEvent>,
}

/// Display info per agent kind (avatar letter + color as 0xRRGGBB).
pub fn agent_badge(agent: &AgentId) -> (&'static str, u32, &'static str) {
    match agent.as_str() {
        "claude" => ("C", 0xD97757, "Claude Code"),
        "codex" => ("X", 0x10A37F, "Codex"),
        "gemini" => ("G", 0x4285F4, "Gemini CLI"),
        "opencode" => ("O", 0xC77DBB, "OpenCode"),
        "goose" => ("Gs", 0xF2C55C, "Goose"),
        _ => ("?", 0x868A91, "Agent"),
    }
}

pub struct AgentsModel {
    pub tasks: Vec<AgentTask>,
    pub quota: Vec<QuotaWindow>,
    pub selected: usize,
}

impl AgentsModel {
    /// Integration point: `tandem-acp` pushes `TaskEvent`s from its `EventRx` here.
    pub fn apply(&mut self, ev: TaskEvent) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == ev.task) {
            match &ev.event {
                AgentEvent::PermissionRequested(_) => t.status = TaskStatus::NeedsApproval,
                AgentEvent::Done { .. } => t.status = TaskStatus::Done,
                AgentEvent::Error { .. } => t.status = TaskStatus::Failed,
                _ => {}
            }
            t.events.push(ev.event);
        }
    }

    pub fn running_count(&self) -> usize {
        self.tasks.iter().filter(|t| matches!(t.status, TaskStatus::Running | TaskStatus::NeedsApproval)).count()
    }

    pub fn fake() -> Self {
        let tool = |id: &str, title: &str, kind: ToolKind, status: ToolStatus, input: Option<&str>| ToolCall {
            id: ToolCallId::from(id),
            title: title.into(),
            kind,
            status,
            locations: vec![],
            raw_input: input.map(Into::into),
            content: None,
            diff: None,
        };
        let text = |role: Role, s: &str| AgentEvent::TextDelta { role, text: s.into() };
        let perm = PermissionRequest {
            id: RequestId::from("req-1"),
            tool_call: tool("t4", "Run shell command", ToolKind::Execute, ToolStatus::Pending, Some("rm -rf ./dist ./node_modules/.cache")),
            options: vec![
                PermissionOption { id: "allow".into(), label: "Allow".into(), kind: PermissionOptionKind::AllowOnce },
                PermissionOption { id: "always".into(), label: "Allow always".into(), kind: PermissionOptionKind::AllowAlways },
                PermissionOption { id: "reject".into(), label: "Reject".into(), kind: PermissionOptionKind::RejectOnce },
            ],
            risk: RiskClass::Destructive,
            reason: "Recursive delete outside the task worktree".into(),
        };
        let claude_events = vec![
            text(Role::User, "Add retry with exponential backoff to the HTTP client and cover it with tests."),
            text(Role::Thought, "I'll look at the client first, then add a RetryPolicy and tests."),
            AgentEvent::ToolCall(tool("t1", "Read src/http/client.ts", ToolKind::Read, ToolStatus::Completed, None)),
            AgentEvent::ToolCall(tool("t2", "Search \"fetchWithTimeout\"", ToolKind::Search, ToolStatus::Completed, None)),
            text(Role::Assistant, "The client wraps `fetch` in `fetchWithTimeout`. I'll add a `RetryPolicy` with jittered backoff and wire it through `request()`."),
            AgentEvent::ToolCall(tool("t3", "Edit src/http/client.ts (+42 −7)", ToolKind::Edit, ToolStatus::Completed, None)),
            text(Role::Assistant, "Before running the tests I want to clear the build cache."),
            AgentEvent::PermissionRequested(perm),
        ];
        let tasks = vec![
            AgentTask { id: TaskId::from("task-1"), agent: "claude".into(), title: "Add retry/backoff to HTTP client".into(), status: TaskStatus::NeedsApproval, elapsed: "4m 12s".into(), events: claude_events },
            AgentTask { id: TaskId::from("task-2"), agent: "codex".into(), title: "Migrate settings page to React Query".into(), status: TaskStatus::Running, elapsed: "11m 40s".into(), events: vec![text(Role::User, "Migrate the settings page data fetching to React Query.")] },
            AgentTask { id: TaskId::from("task-3"), agent: "gemini".into(), title: "Write e2e tests for checkout".into(), status: TaskStatus::Running, elapsed: "2m 03s".into(), events: vec![text(Role::User, "Write Playwright e2e tests for the checkout flow.")] },
            AgentTask { id: TaskId::from("task-4"), agent: "opencode".into(), title: "Fix flaky date formatting test".into(), status: TaskStatus::Done, elapsed: "6m 55s".into(), events: vec![text(Role::User, "Fix the flaky date formatting test in utils.")] },
            AgentTask { id: TaskId::from("task-5"), agent: "goose".into(), title: "Bump eslint to v9 flat config".into(), status: TaskStatus::Failed, elapsed: "1m 18s".into(), events: vec![] },
        ];
        let window = |agent: &str, label: &str, used: Option<f64>| QuotaWindow {
            agent: agent.into(),
            label: label.into(),
            used_fraction: used,
            used_tokens: None,
            limit_tokens: None,
            reset_at: None,
            source: if used.is_some() { QuotaSource::Derived } else { QuotaSource::Estimated },
            observed_at: Utc::now(),
        };
        let quota = vec![
            window("claude", "5h", Some(0.34)),
            window("claude", "weekly", Some(0.61)),
            window("codex", "5h", Some(0.12)),
            window("gemini", "daily", None),
        ];
        Self { tasks, quota, selected: 0 }
    }
}
