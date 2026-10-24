//! Agent cockpit model: the task list the cockpit renders, fed by `tandem-acp` events
//! (`AgentsModel::apply`) and the quota pool (`AgentsModel::pool`). Orchestration (worktrees,
//! spawning, replay logs) lives in `crate::tasks`.

use chrono::{DateTime, Utc};
use gpui_kit::SharedString;
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;
use tandem_core::{
    AgentDescriptor, AgentEvent, AgentId, DiffSet, RequestId, RiskClass, Role, StopReason, TaskEvent, TaskId, TaskPhase, ToolCall, ToolCallId,
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
    /// Merged state of every tool call (initial call + its updates), kept by [`AgentsModel::apply`].
    tools: HashMap<ToolCallId, ToolCall>,
    /// Permission requests that got a `PermissionResolved`.
    resolved: HashSet<RequestId>,
    /// Non-safe permission requests still waiting for a decision.
    pending: HashSet<RequestId>,
    /// Rendered text per event (text deltas only), built on first use and reset when a delta
    /// grows, so the transcript does not copy every string each frame.
    labels: Vec<OnceCell<SharedString>>,
    /// Expanded tool-card text per tool call, built on first use and reset on every update.
    tool_details: HashMap<ToolCallId, OnceCell<ToolDetail>>,
    /// An `Error` event is in `events` (so the transcript need not scan for it).
    pub has_error_event: bool,
}

/// Display text of an expanded tool card.
#[derive(Clone, Debug, Default)]
pub struct ToolDetail {
    pub raw_input: Option<SharedString>,
    pub locations: Vec<SharedString>,
    pub patch: Option<SharedString>,
    /// Content, cut to [`TOOL_CONTENT_MAX`] chars.
    pub content: Option<SharedString>,
}

/// Longest tool output shown in a card.
pub const TOOL_CONTENT_MAX: usize = 4000;

impl ToolDetail {
    fn of(tc: &ToolCall) -> Self {
        Self {
            raw_input: tc.raw_input.clone().map(Into::into),
            locations: tc.locations.iter().map(|l| l.display().to_string().into()).collect(),
            patch: tc.diff.as_ref().map(|d| d.to_patch().into()),
            content: tc.content.as_deref().map(|c| tandem_core::fmt::ellipsize(c, TOOL_CONTENT_MAX).into_owned().into()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.raw_input.is_none() && self.locations.is_empty() && self.patch.is_none() && self.content.is_none()
    }
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
            tools: HashMap::new(),
            resolved: HashSet::new(),
            pending: HashSet::new(),
            labels: vec![],
            tool_details: HashMap::new(),
            has_error_event: false,
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

    /// Number of permission requests still waiting for a decision (safe ones are answered by
    /// policy inside tandem-acp and never reach a card).
    pub fn open_permission_count(&self) -> usize {
        self.pending.len()
    }

    pub fn is_resolved(&self, id: &RequestId) -> bool {
        self.resolved.contains(id)
    }

    /// Latest merged state of a tool call (initial call + its updates).
    pub fn merged_tool(&self, id: &ToolCallId) -> Option<&ToolCall> {
        self.tools.get(id)
    }

    /// Cached display text of tool call `id` (expanded card).
    pub fn tool_detail(&self, id: &ToolCallId) -> Option<&ToolDetail> {
        let tc = self.tools.get(id)?;
        Some(self.tool_details.get(id)?.get_or_init(|| ToolDetail::of(tc)))
    }

    /// Display text of event `i` when it is a text delta (thoughts get their "Thinking:" prefix).
    pub fn label(&self, i: usize) -> Option<SharedString> {
        let AgentEvent::TextDelta { role, text } = self.events.get(i)? else { return None };
        let cell = self.labels.get(i)?;
        Some(
            cell.get_or_init(|| match role {
                Role::Thought => format!("Thinking: {}", text.trim()).into(),
                _ => text.clone().into(),
            })
            .clone(),
        )
    }

    pub fn tokens(&self) -> u64 {
        self.usage.total()
    }

    /// Append an event, keeping the tool / permission indexes in sync.
    fn push(&mut self, event: AgentEvent) {
        match &event {
            AgentEvent::ToolCall(tc) | AgentEvent::ToolCallUpdate(tc) => {
                match self.tools.get_mut(&tc.id) {
                    Some(m) => merge_tool(m, tc),
                    None => {
                        self.tools.insert(tc.id.clone(), tc.clone());
                    }
                }
                // The card text changed: rebuild it on next render.
                self.tool_details.insert(tc.id.clone(), OnceCell::new());
            }
            AgentEvent::Error { .. } => self.has_error_event = true,
            AgentEvent::PermissionRequested(r) if r.risk != RiskClass::Safe && !self.resolved.contains(&r.id) => {
                self.pending.insert(r.id.clone());
            }
            AgentEvent::PermissionResolved { request, .. } => {
                self.resolved.insert(request.clone());
                self.pending.remove(request);
            }
            _ => {}
        }
        self.events.push(event);
        self.labels.push(OnceCell::new());
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

/// Avatar letters, color (0xRRGGBB) and display name of an agent.
#[derive(Clone, Debug)]
pub struct Badge {
    pub letters: SharedString,
    pub color: u32,
    pub name: SharedString,
}

const DEFAULT_BADGE_COLOR: u32 = 0x868A91;

impl Badge {
    /// From a descriptor's `badge` / `display_name`; agents without a badge get their initial
    /// on gray.
    pub fn of(d: &AgentDescriptor) -> Self {
        let name: SharedString = if d.display_name.is_empty() { d.id.to_string().into() } else { d.display_name.clone().into() };
        let letters: SharedString = if d.badge.0.is_empty() {
            name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into()).into()
        } else {
            d.badge.0.clone().into()
        };
        Self { letters, color: u32::from_str_radix(&d.badge.1, 16).unwrap_or(DEFAULT_BADGE_COLOR), name }
    }

    /// Built-in agents from the registry; anything else gets a default badge.
    pub fn for_agent(agent: &AgentId) -> Self {
        static BUILTIN: OnceLock<HashMap<String, Badge>> = OnceLock::new();
        let builtin = BUILTIN.get_or_init(|| tandem_acp::builtin().iter().map(|d| (d.id.to_string(), Badge::of(d))).collect());
        builtin.get(agent.as_str()).cloned().unwrap_or_else(|| Self::of(&AgentDescriptor { id: agent.clone(), ..Default::default() }))
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
                    if let Some(l) = t.labels.last_mut() {
                        *l = OnceCell::new();
                    }
                    return;
                }
            }
            _ => {}
        }
        if t.status == TaskStatus::Starting && !matches!(ev.event, AgentEvent::Done { .. } | AgentEvent::Error { .. }) {
            t.status = TaskStatus::Running;
        }
        t.push(ev.event);
        // A resolved request may leave others open.
        if t.status == TaskStatus::Running && t.open_permission_count() > 0 {
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
    use tandem_core::{PermissionOption, PermissionOptionKind, PermissionRequest, ToolKind, ToolStatus};

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
        assert_eq!(m.tasks[0].open_permission_count(), 1);
        m.apply(ev(AgentEvent::PermissionResolved { request: RequestId::from("r1"), option: "allow".into(), by: tandem_core::Decider::User }));
        assert_eq!(m.tasks[0].status, TaskStatus::Running);
        m.apply(ev(AgentEvent::ToolCallUpdate(ToolCall { title: String::new(), ..tool("t", ToolStatus::Completed) })));
        let merged = m.tasks[0].merged_tool(&ToolCallId::from("t")).unwrap();
        assert_eq!((merged.title.as_str(), merged.status), ("Run", ToolStatus::Completed));
        m.apply(ev(AgentEvent::Done { reason: StopReason::EndTurn }));
        assert_eq!(m.tasks[0].status, TaskStatus::Done);
        assert_eq!(m.running_count(), 0);
    }

    fn task() -> (AgentsModel, TaskId) {
        let mut m = AgentsModel::default();
        let id = TaskId::from("t1");
        m.tasks.push(AgentTask::new(id.clone(), "claude".into(), "do it".into(), TaskPhase::Implement));
        (m, id)
    }

    #[test]
    fn label_cache_resets_when_a_delta_grows() {
        let (mut m, id) = task();
        let ev = |event| TaskEvent { task: id.clone(), event };
        m.apply(ev(AgentEvent::TextDelta { role: Role::Thought, text: " plan ".into() }));
        assert_eq!(m.tasks[0].label(0).as_deref(), Some("Thinking: plan"));
        m.apply(ev(AgentEvent::TextDelta { role: Role::Thought, text: "more".into() }));
        assert_eq!(m.tasks[0].label(0).as_deref(), Some("Thinking: plan more"));
        m.apply(ev(AgentEvent::TextDelta { role: Role::Assistant, text: "Hi".into() }));
        assert_eq!(m.tasks[0].label(1).as_deref(), Some("Hi"));
        m.apply(ev(AgentEvent::TextDelta { role: Role::Assistant, text: " there".into() }));
        assert_eq!(m.tasks[0].label(1).as_deref(), Some("Hi there"));
        assert_eq!(m.tasks[0].label(0).as_deref(), Some("Thinking: plan more"), "earlier labels stay cached");
        assert_eq!(m.tasks[0].label(5), None);
    }

    #[test]
    fn pending_tracking_across_resolves() {
        let (mut m, id) = task();
        let ev = |event| TaskEvent { task: id.clone(), event };
        let req = |rid: &str, risk| PermissionRequest {
            id: RequestId::from(rid),
            tool_call: tool("t", ToolStatus::Pending),
            options: vec![PermissionOption { id: "allow".into(), label: "Allow".into(), kind: PermissionOptionKind::AllowOnce }],
            risk,
            reason: String::new(),
        };
        let resolve = |rid: &str| AgentEvent::PermissionResolved { request: RequestId::from(rid), option: "allow".into(), by: tandem_core::Decider::User };
        m.apply(ev(AgentEvent::PermissionRequested(req("a", RiskClass::Review))));
        m.apply(ev(AgentEvent::PermissionRequested(req("b", RiskClass::Destructive))));
        m.apply(ev(AgentEvent::PermissionRequested(req("safe", RiskClass::Safe))));
        assert_eq!(m.tasks[0].open_permission_count(), 2, "safe requests never wait");
        m.apply(ev(resolve("a")));
        assert_eq!(m.tasks[0].open_permission_count(), 1);
        assert_eq!(m.tasks[0].status, TaskStatus::NeedsApproval, "b still open");
        assert!(m.tasks[0].is_resolved(&RequestId::from("a")));
        m.apply(ev(resolve("cancelled-unknown")));
        m.apply(ev(resolve("b")));
        assert_eq!(m.tasks[0].open_permission_count(), 0);
        assert_eq!(m.tasks[0].status, TaskStatus::Running);
        // A resolution that arrives before its request (replay) leaves nothing pending.
        m.apply(ev(resolve("c")));
        m.apply(ev(AgentEvent::PermissionRequested(req("c", RiskClass::Review))));
        assert_eq!(m.tasks[0].open_permission_count(), 0);
    }

    #[test]
    fn tool_detail_cache_invalidates_on_update_and_error_flag() {
        let (mut m, id) = task();
        let ev = |event| TaskEvent { task: id.clone(), event };
        let tid = ToolCallId::from("t");
        m.apply(ev(AgentEvent::ToolCall(tool("t", ToolStatus::InProgress))));
        let d = m.tasks[0].tool_detail(&tid).unwrap().clone();
        assert_eq!(d.raw_input.as_deref(), Some("rm -rf build"));
        assert!(d.content.is_none());
        m.apply(ev(AgentEvent::ToolCallUpdate(ToolCall { content: Some("x".repeat(TOOL_CONTENT_MAX + 10)), ..tool("t", ToolStatus::Completed) })));
        let d = m.tasks[0].tool_detail(&tid).unwrap();
        assert_eq!(d.content.as_ref().unwrap().chars().count(), TOOL_CONTENT_MAX + 1);
        assert!(m.tasks[0].tool_detail(&ToolCallId::from("nope")).is_none());
        assert!(!m.tasks[0].has_error_event);
        m.apply(ev(AgentEvent::Error { message: "boom".into() }));
        assert!(m.tasks[0].has_error_event);
    }
}
