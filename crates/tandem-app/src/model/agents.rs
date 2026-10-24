//! Agent cockpit model: the task list the cockpit renders, fed by `tandem-acp` events
//! (`AgentsModel::apply`) and the quota pool (`AgentsModel::pool`). Orchestration (worktrees,
//! spawning, replay logs) lives in `crate::tasks`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tandem_core::{
    AgentEvent, AgentId, DiffSet, PermissionRequest, RequestId, RiskClass, Role, StopReason, TaskEvent, TaskId, TaskPhase, ToolCall,
    Usage,
};
use tandem_quota::PoolState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Worktree + agent process are being set up.
    Starting,
    Running,
    NeedsApproval,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Starting => "Starting",
            TaskStatus::Running => "Running",
            TaskStatus::NeedsApproval => "Needs approval",
            TaskStatus::Done => "Done",
            TaskStatus::Failed => "Failed",
            TaskStatus::Cancelled => "Cancelled",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, TaskStatus::Starting | TaskStatus::Running | TaskStatus::NeedsApproval)
    }
}

#[derive(Clone, Debug)]
pub struct AgentTask {
    pub id: TaskId,
    pub agent: AgentId,
    pub title: String,
    pub prompt: String,
    pub phase: TaskPhase,
    pub presets: Vec<String>,
    pub status: TaskStatus,
    pub events: Vec<AgentEvent>,
    pub worktree: Option<PathBuf>,
    pub base_commit: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub usage: Usage,
    /// Router explanation when the agent was picked by "Route for me".
    pub route_reason: Option<String>,
    /// The agent's worktree diff, computed when a turn ends.
    pub diff: Option<DiffSet>,
    /// Restored from disk: the agent process is gone, only the log remains.
    pub restored: bool,
    pub error: Option<String>,
    /// Permission requests the user answered but the agent has not acknowledged yet.
    pub deciding: Vec<RequestId>,
    /// Setup notes shown above the transcript (skills, attached databases).
    pub notes: Vec<String>,
}

impl AgentTask {
    pub fn new(id: TaskId, agent: AgentId, prompt: String, phase: TaskPhase) -> Self {
        Self {
            title: title_from_prompt(&prompt),
            id,
            agent,
            prompt,
            phase,
            presets: vec![],
            status: TaskStatus::Starting,
            events: vec![],
            worktree: None,
            base_commit: None,
            started_at: Utc::now(),
            finished_at: None,
            usage: Usage::default(),
            route_reason: None,
            diff: None,
            restored: false,
            error: None,
            deciding: vec![],
            notes: vec![],
        }
    }

    pub fn elapsed(&self) -> chrono::Duration {
        self.finished_at.unwrap_or_else(Utc::now) - self.started_at
    }

    pub fn elapsed_label(&self) -> String {
        let s = self.elapsed().num_seconds().max(0);
        if s >= 3600 {
            format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
        } else if s >= 60 {
            format!("{}m {:02}s", s / 60, s % 60)
        } else {
            format!("{s}s")
        }
    }

    /// Permission requests still waiting for a decision (not resolved, not auto-allowed).
    pub fn open_permissions(&self) -> Vec<&PermissionRequest> {
        self.events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::PermissionRequested(r) if !self.is_resolved(&r.id) && !(r.risk == RiskClass::Safe && self.auto_allowed(r)) => Some(r),
                _ => None,
            })
            .collect()
    }

    pub fn is_resolved(&self, id: &RequestId) -> bool {
        self.events.iter().any(|e| matches!(e, AgentEvent::PermissionResolved { request, .. } if request == id))
    }

    /// Safe requests are answered by policy inside tandem-acp; they never reach a card.
    pub fn auto_allowed(&self, r: &PermissionRequest) -> bool {
        r.risk == RiskClass::Safe
    }

    /// Latest merged state of a tool call (initial call + its updates).
    pub fn merged_tool(&self, id: &tandem_core::ToolCallId) -> Option<ToolCall> {
        let mut out: Option<ToolCall> = None;
        for e in &self.events {
            let (AgentEvent::ToolCall(tc) | AgentEvent::ToolCallUpdate(tc)) = e else { continue };
            if &tc.id != id {
                continue;
            }
            match &mut out {
                None => out = Some(tc.clone()),
                Some(m) => merge_tool(m, tc),
            }
        }
        out
    }

    pub fn tokens(&self) -> u64 {
        let u = &self.usage;
        u.input_tokens + u.output_tokens + u.cache_read_tokens + u.cache_write_tokens
    }
}

pub fn merge_tool(into: &mut ToolCall, upd: &ToolCall) {
    if !upd.title.is_empty() {
        into.title = upd.title.clone();
    }
    into.status = upd.status;
    if upd.kind != tandem_core::ToolKind::Other {
        into.kind = upd.kind;
    }
    if !upd.locations.is_empty() {
        into.locations = upd.locations.clone();
    }
    if upd.raw_input.is_some() {
        into.raw_input = upd.raw_input.clone();
    }
    if upd.content.is_some() {
        into.content = upd.content.clone();
    }
    if upd.diff.is_some() {
        into.diff = upd.diff.clone();
    }
}

pub fn title_from_prompt(prompt: &str) -> String {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Untitled task");
    let mut t: String = line.chars().take(72).collect();
    if line.chars().count() > 72 {
        t.push('…');
    }
    t
}

/// Display info per agent kind (avatar letter + color as 0xRRGGBB + display name).
pub fn agent_badge(agent: &AgentId) -> (&'static str, u32, &'static str) {
    match agent.as_str() {
        "claude" => ("C", 0xD97757, "Claude Code"),
        "codex" => ("X", 0x10A37F, "Codex"),
        "gemini" => ("G", 0x4285F4, "Gemini CLI"),
        "opencode" => ("O", 0xC77DBB, "OpenCode"),
        "goose" => ("Gs", 0xF2C55C, "Goose"),
        "kiro" => ("K", 0x9046FF, "Kiro CLI"),
        "cline" => ("Cl", 0x5B8DEF, "Cline"),
        "copilot" => ("Co", 0x8957E5, "Copilot CLI"),
        "qwen" => ("Q", 0x615CED, "Qwen Code"),
        "fake" => ("F", 0x868A91, "Fake agent"),
        _ => ("?", 0x868A91, "Agent"),
    }
}

#[derive(Default)]
pub struct AgentsModel {
    pub tasks: Vec<AgentTask>,
    pub pool: PoolState,
    pub selected: usize,
}

impl AgentsModel {
    pub fn task(&self, id: &TaskId) -> Option<&AgentTask> {
        self.tasks.iter().find(|t| &t.id == id)
    }

    pub fn task_mut(&mut self, id: &TaskId) -> Option<&mut AgentTask> {
        self.tasks.iter_mut().find(|t| &t.id == id)
    }

    pub fn selected_task(&self) -> Option<&AgentTask> {
        self.tasks.get(self.selected)
    }

    /// Fold one agent event into its task. Consecutive text chunks of the same role merge into
    /// one paragraph; `Raw` JSON-RPC lines stay in the replay log only.
    pub fn apply(&mut self, ev: TaskEvent) {
        let Some(t) = self.tasks.iter_mut().find(|t| t.id == ev.task) else { return };
        match &ev.event {
            AgentEvent::Raw { .. } => return,
            AgentEvent::SessionStarted { .. } if t.status == TaskStatus::Starting => t.status = TaskStatus::Running,
            AgentEvent::PermissionRequested(r) if r.risk != RiskClass::Safe => t.status = TaskStatus::NeedsApproval,
            AgentEvent::PermissionResolved { request, .. } => {
                t.deciding.retain(|d| d != request);
                if t.status == TaskStatus::NeedsApproval {
                    t.status = TaskStatus::Running;
                }
            }
            AgentEvent::Usage(u) => t.usage.add(u),
            AgentEvent::Done { reason } => {
                t.finished_at = Some(Utc::now());
                t.status = match reason {
                    StopReason::Cancelled => TaskStatus::Cancelled,
                    StopReason::Error => TaskStatus::Failed,
                    _ if t.status == TaskStatus::Failed => TaskStatus::Failed,
                    _ => TaskStatus::Done,
                };
            }
            AgentEvent::Error { message } => {
                t.error = Some(message.clone());
                t.status = TaskStatus::Failed;
            }
            AgentEvent::TextDelta { role, text } if *role != Role::User => {
                if let Some(AgentEvent::TextDelta { role: last_role, text: last }) = t.events.last_mut()
                    && last_role == role
                {
                    last.push_str(text);
                    return;
                }
            }
            _ => {}
        }
        if t.status == TaskStatus::Starting && !matches!(ev.event, AgentEvent::Done { .. } | AgentEvent::Error { .. }) {
            t.status = TaskStatus::Running;
        }
        t.events.push(ev.event);
        // A resolved request may leave others open.
        if t.status == TaskStatus::Running && !t.open_permissions().is_empty() {
            t.status = TaskStatus::NeedsApproval;
        }
    }

    pub fn running_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.status.is_active()).count()
    }

    pub fn waiting_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.status == TaskStatus::NeedsApproval).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tandem_core::{PermissionOption, PermissionOptionKind, ToolCallId, ToolKind, ToolStatus};

    fn tool(id: &str, status: ToolStatus) -> ToolCall {
        ToolCall { id: ToolCallId::from(id), title: "Run".into(), kind: ToolKind::Execute, status, locations: vec![], raw_input: Some("rm -rf build".into()), content: None, diff: None }
    }

    #[test]
    fn apply_merges_text_and_tracks_permissions() {
        let mut m = AgentsModel::default();
        let id = TaskId::from("t1");
        m.tasks.push(AgentTask::new(id.clone(), "claude".into(), "do it".into(), TaskPhase::Implement));
        let ev = |event| TaskEvent { task: id.clone(), event };
        m.apply(ev(AgentEvent::TextDelta { role: Role::Assistant, text: "Hel".into() }));
        m.apply(ev(AgentEvent::TextDelta { role: Role::Assistant, text: "lo".into() }));
        assert_eq!(m.tasks[0].events.len(), 1);
        assert!(matches!(&m.tasks[0].events[0], AgentEvent::TextDelta { text, .. } if text == "Hello"));
        let req = PermissionRequest {
            id: RequestId::from("r1"),
            tool_call: tool("t", ToolStatus::Pending),
            options: vec![PermissionOption { id: "allow".into(), label: "Allow".into(), kind: PermissionOptionKind::AllowOnce }],
            risk: RiskClass::Destructive,
            reason: "recursive delete".into(),
        };
        m.apply(ev(AgentEvent::ToolCall(tool("t", ToolStatus::Pending))));
        m.apply(ev(AgentEvent::PermissionRequested(req)));
        assert_eq!(m.tasks[0].status, TaskStatus::NeedsApproval);
        assert_eq!(m.tasks[0].open_permissions().len(), 1);
        m.apply(ev(AgentEvent::PermissionResolved { request: RequestId::from("r1"), option: "allow".into(), by: tandem_core::Decider::User }));
        assert_eq!(m.tasks[0].status, TaskStatus::Running);
        m.apply(ev(AgentEvent::ToolCallUpdate(ToolCall { title: String::new(), ..tool("t", ToolStatus::Completed) })));
        let merged = m.tasks[0].merged_tool(&ToolCallId::from("t")).unwrap();
        assert_eq!((merged.title.as_str(), merged.status), ("Run", ToolStatus::Completed));
        m.apply(ev(AgentEvent::Done { reason: StopReason::EndTurn }));
        assert_eq!(m.tasks[0].status, TaskStatus::Done);
        assert_eq!(m.running_count(), 0);
    }
}
