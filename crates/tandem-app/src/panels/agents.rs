//! Agents cockpit: quota pool header, task list, streaming transcript with tool-call and
//! permission cards, prompt composer with routing / skill presets / estimate, and the global
//! kill switch. Orchestration lives in `crate::tasks`; this panel owns the live agent handles.

use crate::assets::icon;
use crate::keymap::StopAllAgents;
use crate::model::agents::{AgentTask, AgentsModel, TaskStatus, agent_badge};
use crate::panels::chip;
use crate::panels::review::ReviewInput;
use crate::runtime::spawn_bridge;
use crate::tasks::actions::OpenTerminalWith;
use crate::tasks::{self, StartSpec, TaskMsg, db, store};
use crate::theme::{MONO_FONT, Palette, hex, pal};
use chrono::Utc;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tandem_acp::{AgentHandle, Availability, DetectedAgent};
use tandem_core::{
    AgentEvent, AgentId, Decider, PermissionDecision, PermissionOptionKind, PermissionRequest, Policy, RiskClass, Role, TaskEvent,
    TaskId, TaskPhase, ToolCall, ToolCallId, ToolKind, ToolStatus,
};
use tandem_quota::{QuotaPool, RouteRequest, TurnRecord, Unit, WindowKind};
use tandem_skills::Preset;

pub enum AgentsEvent {
    Toast(SharedString),
    /// Task list / statuses changed (status bar + title bar pill).
    Changed,
    /// A turn ended with changes in the worktree: refresh the Review panel.
    ReviewReady(TaskId),
    /// User asked to review a task now.
    OpenReview(TaskId),
}

/// The user's answer on a permission card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    AllowOnce,
    AllowAlways,
    Reject,
}

/// Skill preset chips, in display order.
pub const PRESETS: &[(Preset, &str)] =
    &[(Preset::Lazy, "Lazy"), (Preset::Terse, "Terse"), (Preset::Disciplined, "Disciplined"), (Preset::Ship, "Ship"), (Preset::TandemDefault, "Tandem")];

pub struct AgentsPanel {
    pub root: PathBuf,
    pub model: AgentsModel,
    pub detected: Vec<DetectedAgent>,
    pub policy: Policy,
    pool: Option<Arc<QuotaPool>>,
    handles: HashMap<TaskId, Arc<AgentHandle>>,
    composer: Entity<TextareaState>,
    /// `None` = "Route for me".
    pub pinned: Option<AgentId>,
    pub phase: TaskPhase,
    pub presets: HashSet<Preset>,
    pub kill_tripped: bool,
    expanded: HashSet<(TaskId, ToolCallId)>,
    _tick: Task<()>,
}

impl EventEmitter<AgentsEvent> for AgentsPanel {}

impl AgentsPanel {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| TextareaState::new(window, cx).rows(3).placeholder("Describe a task for an agent…  (⌘⏎ to start)"));
        let detected = tasks::detect_agents(&root);
        let mut model = AgentsModel { tasks: store::restore(&root), ..Default::default() };
        model.selected = model.tasks.len().saturating_sub(1);
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |this, cx| {
                    if this.poll_quota() || this.model.running_count() > 0 {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            policy: crate::state::load_policy(&root),
            root,
            model,
            detected,
            pool: None,
            handles: HashMap::new(),
            composer,
            pinned: None,
            phase: TaskPhase::Implement,
            presets: [Preset::Disciplined, Preset::TandemDefault].into_iter().collect(),
            kill_tripped: tandem_sandbox::KillSwitch::global().is_tripped(),
            expanded: HashSet::new(),
            _tick: tick,
        };
        this.load_restored_diffs(cx);
        #[cfg(feature = "test-support")]
        this.demo_hooks(cx);
        this
    }

    /// Recompute diffs of restored tasks whose worktree still exists (Review badges and
    /// intent-conflict detection across sessions).
    fn load_restored_diffs(&mut self, cx: &mut Context<Self>) {
        let jobs: Vec<(TaskId, AgentId, String)> = self
            .model
            .tasks
            .iter()
            .filter(|t| t.restored && t.worktree.is_some())
            .filter_map(|t| Some((t.id.clone(), t.agent.clone(), t.base_commit.clone()?)))
            .collect();
        if jobs.is_empty() {
            return;
        }
        let root = self.root.clone();
        let rx = spawn_bridge(async move {
            tokio::task::spawn_blocking(move || jobs.into_iter().filter_map(|(id, agent, base)| tasks::compute_diff(&root, &id, &agent, &base).ok().map(|d| (id, d))).collect::<Vec<_>>())
                .await
                .unwrap_or_default()
        });
        cx.spawn(async move |this, cx| {
            if let Ok(diffs) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    for (id, d) in diffs {
                        if let Some(t) = this.model.task_mut(&id) {
                            t.diff = Some(d);
                        }
                    }
                    #[cfg(feature = "test-support")]
                    if let Ok(task) = std::env::var("TANDEM_DEMO_REVIEW") {
                        cx.emit(AgentsEvent::OpenReview(TaskId::from(task)));
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// `--screenshot` support: `TANDEM_DEMO_AGENT=<ACP binary>` registers it as a scripted demo
    /// agent and starts `TANDEM_DEMO_PROMPT` on it. (`TANDEM_DEMO_REVIEW=<task id>` opens that
    /// restored task in Review once restored diffs are loaded.)
    #[cfg(feature = "test-support")]
    fn demo_hooks(&mut self, cx: &mut Context<Self>) {
        if let Some(bin) = std::env::var_os("TANDEM_DEMO_AGENT") {
            self.add_agent(
                tandem_core::AgentDescriptor {
                    id: "fake".into(),
                    display_name: "Scripted demo agent".into(),
                    vendor: "Tandem".into(),
                    command: bin.to_string_lossy().into_owned(),
                    args: vec![],
                    env: Default::default(),
                    install_hint: String::new(),
                    acp: true,
                    auth_note: String::new(),
                },
                cx,
            );
            self.pinned = Some("fake".into());
            let prompt = std::env::var("TANDEM_DEMO_PROMPT").unwrap_or_else(|_| "Clean the build output and regenerate it".into());
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.start_task(prompt, cx));
            })
            .detach();
        }
    }

    /// Start the quota pool on first use (first render), so opening a project does not spawn
    /// provider processes until the cockpit is visible.
    fn ensure_quota(&mut self) {
        if self.pool.is_none() {
            let ids = self.detected.iter().map(|d| d.descriptor.id.clone()).collect();
            let pool = tasks::quota::start_pool(ids);
            self.model.pool = pool.state();
            self.pool = Some(pool);
        }
    }

    /// Pull the latest pool state (called from the 1 s tick).
    fn poll_quota(&mut self) -> bool {
        let Some(pool) = &self.pool else { return false };
        let st = pool.state();
        let changed = st != self.model.pool;
        if changed {
            self.model.pool = st;
        }
        changed
    }

    /// Register an extra ACP agent (custom agents, tests).
    pub fn add_agent(&mut self, desc: tandem_core::AgentDescriptor, cx: &mut Context<Self>) {
        self.detected.retain(|d| d.descriptor.id != desc.id);
        let path = PathBuf::from(&desc.command);
        self.detected.push(DetectedAgent { descriptor: desc, availability: Availability::Installed(path) });
        cx.notify();
    }

    pub fn composer_text(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    pub fn set_composer_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.set_value(text.to_string(), window, cx));
    }

    fn installed_ids(&self) -> Vec<AgentId> {
        self.detected.iter().map(|d| d.descriptor.id.clone()).collect()
    }

    /// Router's pick for the current phase (best first), from live quota headroom.
    pub fn route(&self) -> Option<tandem_quota::RouteChoice> {
        let ids = self.installed_ids();
        if ids.is_empty() {
            return None;
        }
        let req = RouteRequest { phase: self.phase, pins: None, exclude: vec![], prefs: BTreeMap::new(), require_installed: &ids };
        tandem_quota::route(&self.model.pool, &req).into_iter().next()
    }

    fn chosen_agent(&self) -> Option<(AgentId, Option<String>)> {
        match &self.pinned {
            Some(a) => Some((a.clone(), None)),
            None => self.route().map(|c| (c.agent, Some(c.reason))),
        }
    }

    /// Pre-prompt estimate from the agent's recent finished tasks (replay history).
    pub fn estimate_label(&self, agent: &AgentId) -> String {
        let history: Vec<TurnRecord> = self
            .model
            .tasks
            .iter()
            .filter(|t| t.finished_at.is_some() && t.tokens() > 0)
            .map(|t| TurnRecord { agent: t.agent.clone(), usage: t.usage.clone(), elapsed: t.elapsed().to_std().unwrap_or_default() })
            .collect();
        let est = tandem_quota::estimate(&history, agent);
        let name = agent_badge(agent).2;
        if est.samples == 0 {
            return format!("no {name} history yet: estimate after the first task");
        }
        let tokens = est.median_tokens as f64;
        // Convert to a share of the tightest quota window when its size is known.
        if let Some(snap) = self.model.pool.snapshots.get(agent) {
            for w in snap.windows.iter().filter(|w| w.kind == WindowKind::Quota) {
                let per_token = match (w.unit, w.used, w.limit, w.used_fraction) {
                    (Unit::Tokens, _, Some(l), _) if l > 0.0 => Some(1.0 / l),
                    (Unit::Tokens, Some(u), _, Some(f)) if u > 0.0 => Some(f / u),
                    _ => None,
                };
                if let Some(pt) = per_token {
                    let pct = (tokens * pt * 100.0).max(0.1);
                    return format!("est. ~{pct:.0}% of {name} {} · {} turns", w.label, est.samples);
                }
            }
        }
        format!("est. ~{} tokens · ~{} · median of {} turns", fmt_tokens(est.median_tokens), fmt_secs(est.median_secs), est.samples)
    }

    // ------------------------------------------------------------------ task lifecycle

    /// Start a task with the composer's settings. Returns its id.
    pub fn start_task(&mut self, prompt: String, cx: &mut Context<Self>) -> Option<TaskId> {
        if prompt.trim().is_empty() {
            cx.emit(AgentsEvent::Toast("Write a prompt first".into()));
            return None;
        }
        if self.kill_tripped {
            cx.emit(AgentsEvent::Toast("Kill switch is engaged: re-arm agents before starting new tasks".into()));
            return None;
        }
        let Some((agent, route_reason)) = self.chosen_agent() else {
            cx.emit(AgentsEvent::Toast("No ACP agent found on this machine. Install one (see the agent menu).".into()));
            return None;
        };
        let Some(desc) = self.detected.iter().find(|d| d.descriptor.id == agent).map(|d| d.descriptor.clone()) else {
            cx.emit(AgentsEvent::Toast(format!("{agent} is not installed").into()));
            return None;
        };
        self.ensure_quota();
        let id = tasks::new_task_id(&agent);
        let presets: Vec<Preset> = PRESETS.iter().map(|(p, _)| *p).filter(|p| self.presets.contains(p)).collect();
        let (mcp, notes) = db::mcp_specs(&self.root);
        let mut t = AgentTask::new(id.clone(), agent.clone(), prompt.clone(), self.phase);
        t.presets = presets.iter().map(|p| p.name().to_string()).collect();
        t.route_reason = route_reason;
        t.notes = notes;
        store::save(&self.root, &t);
        self.model.tasks.push(t);
        self.model.selected = self.model.tasks.len() - 1;
        let spec = StartSpec { id: id.clone(), desc, prompt, root: self.root.clone(), policy: self.policy.clone(), presets, mcp };
        let rx = tasks::start(spec, self.pool.clone());
        self.listen(id.clone(), rx, cx);
        cx.emit(AgentsEvent::Changed);
        cx.notify();
        Some(id)
    }

    fn listen(&mut self, id: TaskId, rx: async_channel::Receiver<TaskMsg>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                let id = id.clone();
                if this.update(cx, |this, cx| this.on_msg(id, msg, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_msg(&mut self, id: TaskId, msg: TaskMsg, cx: &mut Context<Self>) {
        let before = self.model.task(&id).map(|t| t.status);
        match msg {
            TaskMsg::Prepared { worktree, base, notes } => {
                if let Some(t) = self.model.task_mut(&id) {
                    t.worktree = Some(worktree);
                    t.base_commit = Some(base);
                    t.notes.extend(notes);
                }
            }
            TaskMsg::Spawned(h) => {
                if self.model.task(&id).is_some() {
                    self.handles.insert(id.clone(), h);
                }
            }
            TaskMsg::Event(ev) => self.model.apply(TaskEvent { task: id.clone(), event: *ev }),
            TaskMsg::Diff(ds) => {
                let n = ds.files.len();
                if let Some(t) = self.model.task_mut(&id) {
                    t.diff = Some(*ds);
                }
                if n > 0 {
                    cx.emit(AgentsEvent::ReviewReady(id.clone()));
                    cx.emit(AgentsEvent::Toast(format!("Task finished with {n} changed file{}. Open Review to accept hunks.", if n == 1 { "" } else { "s" }).into()));
                }
            }
            TaskMsg::Failed(err) => {
                if let Some(t) = self.model.task_mut(&id) {
                    t.status = TaskStatus::Failed;
                    t.error = Some(err.clone());
                    t.finished_at = Some(Utc::now());
                }
                cx.emit(AgentsEvent::Toast(format!("Task failed: {err}").into()));
            }
        }
        if let Some(t) = self.model.task(&id)
            && Some(t.status) != before
        {
            store::save(&self.root, t);
            cx.emit(AgentsEvent::Changed);
        }
        cx.notify();
    }

    /// Answer a permission card. Allow always on a shell command also trusts it for future tasks
    /// (never for Destructive: trust cannot downgrade those).
    pub fn decide(&mut self, task: &TaskId, req: &PermissionRequest, choice: Choice, cx: &mut Context<Self>) {
        let Some(handle) = self.handles.get(task).cloned() else {
            cx.emit(AgentsEvent::Toast("The agent process for this task has ended; nothing to answer.".into()));
            return;
        };
        let Some(option_id) = pick_option(req, choice) else {
            cx.emit(AgentsEvent::Toast("The agent offered no matching option".into()));
            return;
        };
        if choice == Choice::AllowAlways && req.risk != RiskClass::Destructive && req.tool_call.kind == ToolKind::Execute {
            let cmd = command_of(&req.tool_call);
            if !cmd.is_empty() && !self.policy.trusted_commands.contains(&cmd) {
                self.policy.trusted_commands.push(cmd.clone());
                crate::state::save_policy(&self.root, &self.policy);
                cx.emit(AgentsEvent::Toast(format!("Trusted for new tasks: {cmd}").into()));
            }
        }
        if let Some(t) = self.model.task_mut(task) {
            t.deciding.push(req.id.clone());
        }
        let decision = PermissionDecision { request: req.id.clone(), option_id };
        let rx = spawn_bridge(async move { handle.respond_permission(decision, Decider::User).await });
        let (task, rid) = (task.clone(), req.id.clone());
        cx.spawn(async move |this, cx| {
            if let Ok(Err(e)) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(t) = this.model.task_mut(&task) {
                        t.deciding.retain(|d| d != &rid);
                    }
                    cx.emit(AgentsEvent::Toast(format!("Could not answer: {e}").into()));
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    pub fn cancel_task(&mut self, task: &TaskId, cx: &mut Context<Self>) {
        if let Some(h) = self.handles.get(task).cloned() {
            let _ = spawn_bridge(async move { h.cancel().await });
            cx.emit(AgentsEvent::Toast("Cancel sent".into()));
        }
    }

    /// Remove the task: its agent, worktree, branch and `.tandem/tasks/<id>`.
    pub fn delete_task(&mut self, task: &TaskId, cx: &mut Context<Self>) {
        self.handles.remove(task);
        let Some(ix) = self.model.tasks.iter().position(|t| &t.id == task) else { return };
        self.model.tasks.remove(ix);
        self.model.selected = self.model.selected.min(self.model.tasks.len().saturating_sub(1));
        let (root, id) = (self.root.clone(), task.clone());
        let rx = spawn_bridge(async move {
            tokio::task::spawn_blocking(move || {
                let r = tandem_git::Repo::open(&root).and_then(|repo| repo.remove_task_worktree(&id, true));
                let _ = std::fs::remove_dir_all(tasks::task_dir(&root, &id));
                r.map_err(|e| e.to_string())
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Err(e))) = rx.recv().await {
                let _ = this.update(cx, |_, cx| cx.emit(AgentsEvent::Toast(format!("Worktree not removed: {e}").into())));
            }
        })
        .detach();
        cx.emit(AgentsEvent::Changed);
        cx.notify();
    }

    /// Send a follow-up prompt to an idle task whose agent is still alive.
    pub fn reply(&mut self, task: &TaskId, prompt: String, cx: &mut Context<Self>) {
        let Some(h) = self.handles.get(task).cloned() else { return };
        let ev = AgentEvent::TextDelta { role: Role::User, text: prompt.clone() };
        if let Ok(mut log) = tandem_acp::ReplayLog::open(tasks::task_dir(&self.root, task)) {
            let _ = log.record(task, &ev);
        }
        if let Some(t) = self.model.task_mut(task) {
            t.status = TaskStatus::Running;
            t.finished_at = None;
            t.events.push(ev);
        }
        let _ = spawn_bridge(async move { h.prompt(prompt).await });
        cx.emit(AgentsEvent::Changed);
        cx.notify();
    }

    /// Global STOP: SIGTERM every agent process group (SIGKILL after 2 s) and refuse new
    /// spawns until re-armed.
    pub fn stop_all(&mut self, cx: &mut Context<Self>) {
        let rx = spawn_bridge(async { tandem_acp::stop_all() });
        self.kill_tripped = true;
        self.handles.clear();
        let now = Utc::now();
        for t in &mut self.model.tasks {
            if t.status.is_active() {
                t.status = TaskStatus::Cancelled;
                t.error = Some("Stopped by the kill switch".into());
                t.finished_at = Some(now);
                store::save(&self.root, t);
            }
        }
        cx.spawn(async move |this, cx| {
            if let Ok(report) = rx.recv().await {
                let n = report.terminated.len();
                let k = report.killed.len();
                let _ = this.update(cx, |_, cx| {
                    cx.emit(AgentsEvent::Toast(format!("Kill switch: {n} agent process group(s) stopped{}", if k > 0 { format!(", {k} force-killed") } else { String::new() }).into()))
                });
            }
        })
        .detach();
        cx.emit(AgentsEvent::Changed);
        cx.notify();
    }

    pub fn rearm(&mut self, cx: &mut Context<Self>) {
        tandem_acp::reset_stop();
        self.kill_tripped = false;
        cx.notify();
    }

    pub fn select_task(&mut self, task: &TaskId, cx: &mut Context<Self>) {
        if let Some(ix) = self.model.tasks.iter().position(|t| &t.id == task) {
            self.model.selected = ix;
            cx.notify();
        }
    }

    /// Review input for one task (meta, diff, worktree).
    pub fn review_input(&self, task: &TaskId) -> Option<ReviewInput> {
        let t = self.model.task(task)?;
        Some(ReviewInput { meta: tasks::task_meta(t), diff: t.diff.clone()?, worktree: t.worktree.clone() })
    }

    /// Every task with a diff (for intent-conflict detection).
    pub fn review_inputs(&self) -> Vec<ReviewInput> {
        self.model.tasks.iter().filter_map(|t| self.review_input(&t.id)).collect()
    }

    /// Recompute a task's diff from its worktree (restored tasks have none in memory).
    pub fn refresh_diff(&mut self, task: &TaskId, cx: &mut Context<Self>) {
        let Some(t) = self.model.task(task) else { return };
        let (Some(base), Some(_)) = (t.base_commit.clone(), t.worktree.clone()) else { return };
        let (root, id, agent) = (self.root.clone(), t.id.clone(), t.agent.clone());
        let rx = spawn_bridge(async move { tokio::task::spawn_blocking(move || tasks::compute_diff(&root, &id, &agent, &base)).await });
        let id = task.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Ok(ds))) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(t) = this.model.task_mut(&id) {
                        t.diff = Some(ds);
                    }
                    cx.emit(AgentsEvent::OpenReview(id.clone()));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.composer_text(cx);
        if self.start_task(prompt, cx).is_some() {
            self.set_composer_text("", window, cx);
        }
    }

    fn submit_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.composer_text(cx);
        if prompt.trim().is_empty() {
            return;
        }
        if let Some(id) = self.model.selected_task().map(|t| t.id.clone()) {
            self.reply(&id, prompt, cx);
            self.set_composer_text("", window, cx);
        }
    }

    fn can_reply(&self) -> bool {
        self.model.selected_task().is_some_and(|t| !t.status.is_active() && self.handles.contains_key(&t.id))
    }

    // ------------------------------------------------------------------ rendering

    fn render_quota(&self, p: &Palette, _cx: &mut Context<Self>) -> impl IntoElement {
        let now = Utc::now();
        let mut cards: Vec<AnyElement> = Vec::new();
        for (ix, d) in self.detected.iter().enumerate() {
            let id = &d.descriptor.id;
            let (letter, color, name) = agent_badge(id);
            let name = if name == "Agent" { d.descriptor.display_name.clone() } else { name.to_string() };
            let snap = self.model.pool.snapshots.get(id);
            let mut rows: Vec<AnyElement> = Vec::new();
            if let Some(s) = snap {
                for w in s.windows.iter().filter(|w| w.kind == WindowKind::Quota && w.used_fraction.is_some()).take(2) {
                    let f = w.used_fraction.unwrap_or(0.0);
                    let color = if f > 0.85 { p.error } else if f > 0.6 { p.warning } else { p.ok };
                    let reset = w.reset_at.map(|r| format!("resets {}", countdown(r - now)));
                    rows.push(
                        v_flex()
                            .flex_none()
                            .gap(px(2.))
                            .w(px(112.))
                            .child(div().text_size(px(11.)).text_color(p.text).whitespace_nowrap().child(format!("{} {:.0}%", w.label, f * 100.0)))
                            .child(div().h(px(4.)).w_full().rounded(px(2.)).bg(p.border).child(div().h_full().rounded(px(2.)).bg(color).w(relative(f as f32))))
                            .children(reset.map(|r| div().text_size(px(10.)).text_color(p.muted).whitespace_nowrap().child(r)))
                            .into_any_element(),
                    );
                }
                for w in s.windows.iter().filter(|w| w.kind == WindowKind::Usage).take(1) {
                    let v = w.used.map(|u| if w.unit == Unit::Usd { format!("${u:.2}") } else { fmt_tokens(u as u64) }).unwrap_or_else(|| "—".into());
                    rows.push(div().text_size(px(11.)).text_color(p.muted).whitespace_nowrap().child(format!("{} {v}", w.label)).into_any_element());
                }
            }
            if rows.is_empty() {
                rows.push(div().text_size(px(11.)).text_color(p.muted).child("quota —").into_any_element());
            }
            let plan = snap.and_then(|s| s.plan.clone());
            let installable = matches!(d.availability, Availability::Installable);
            let login = tasks::login_command(id, &d.descriptor.auth_note);
            let title = format!("Sign in to {name}");
            cards.push(
                h_flex()
                    .id(("quota-agent", ix))
                    .flex_none()
                    .gap(px(8.))
                    .items_start()
                    .child(avatar_owned(letter, color, 20.))
                    .child(
                        v_flex()
                            .gap(px(3.))
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(name.clone()))
                                    .children(plan.map(|pl| div().text_size(px(11.)).text_color(p.muted).child(pl)))
                                    .when(installable, |d| d.child(chip("via npx", p.warning, p.warning.opacity(0.15))))
                                    .children(login.map(|(program, args)| {
                                        div()
                                            .id(("sign-in", ix))
                                            .text_size(px(11.))
                                            .text_color(p.accent)
                                            .cursor_pointer()
                                            .hover(|s| s.underline())
                                            .child("Sign in")
                                            .on_click(move |_, window, cx| {
                                                window.dispatch_action(
                                                    Box::new(OpenTerminalWith { title: title.clone(), program: program.clone(), args: args.clone(), cwd: None }),
                                                    cx,
                                                )
                                            })
                                    })),
                            )
                            .child(h_flex().gap(px(10.)).children(rows)),
                    )
                    .into_any_element(),
            );
        }
        if cards.is_empty() {
            cards.push(
                div()
                    .text_size(px(12.))
                    .text_color(p.muted)
                    .child("No ACP agents found on PATH. Install one, e.g. `npm i -g @agentclientprotocol/claude-agent-acp`.")
                    .into_any_element(),
            );
        }
        h_flex()
            .id("quota-pool")
            .flex_none()
            .px(px(12.))
            .py(px(8.))
            .gap(px(20.))
            .overflow_x_scroll()
            .border_b_1()
            .border_color(p.border)
            .child(
                v_flex()
                    .flex_none()
                    .child(div().text_size(px(11.)).text_color(p.muted).child("QUOTA POOL"))
                    .child(div().text_size(px(12.)).text_color(p.text).whitespace_nowrap().child(format!("{} running", self.model.running_count()))),
            )
            .children(cards)
    }

    fn render_task_row(&self, ix: usize, t: &AgentTask, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let (letter, color, _) = agent_badge(&t.agent);
        let selected = self.model.selected == ix;
        let (fg, bg) = status_colors(t.status, p);
        v_flex()
            .id(("task", ix))
            .px(px(10.))
            .py(px(7.))
            .gap(px(4.))
            .rounded(px(6.))
            .mx(px(6.))
            .when(selected, |d| d.bg(p.row_selected))
            .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.model.selected = ix;
                cx.notify();
            }))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(avatar(letter, color, 18.))
                    .child(div().flex_1().text_size(px(13.)).text_color(p.text).whitespace_nowrap().overflow_hidden().text_ellipsis().child(t.title.clone())),
            )
            .child(
                h_flex()
                    .gap(px(6.))
                    .pl(px(26.))
                    .child(chip(t.status.label(), fg, bg))
                    .child(div().text_size(px(11.)).text_color(p.muted).child(t.elapsed_label()))
                    .when(t.diff.as_ref().is_some_and(|d| !d.files.is_empty()), |d| {
                        d.child(div().text_size(px(11.)).text_color(p.git_modified).child(format!("{} files", t.diff.as_ref().map_or(0, |d| d.files.len()))))
                    }),
            )
    }

    fn render_transcript(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(t) = self.model.selected_task() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_color(p.muted)
                .child(icon(IconName::Bot, 28., p.muted))
                .child("No agent tasks yet")
                .child(div().text_size(px(12.)).child("Each task runs in its own git worktree under .tandem/worktrees/"))
                .into_any_element();
        };
        let (letter, color, agent_name) = agent_badge(&t.agent);
        let mut items: Vec<AnyElement> = Vec::new();
        for n in &t.notes {
            items.push(h_flex().gap(px(6.)).text_size(px(12.)).text_color(p.muted).child(icon(IconName::Database, 12., p.muted)).child(n.clone()).into_any_element());
        }
        let mut seen_tools: HashSet<ToolCallId> = HashSet::new();
        for (i, ev) in t.events.iter().enumerate() {
            match ev {
                AgentEvent::TextDelta { role: Role::User, text } => {
                    items.push(div().p(px(10.)).rounded(px(8.)).bg(p.stripe_selected).text_color(p.text).child(text.clone()).into_any_element())
                }
                AgentEvent::TextDelta { role: Role::Thought, text } => {
                    items.push(div().italic().text_size(px(12.)).text_color(p.muted).child(format!("Thinking: {}", text.trim())).into_any_element())
                }
                AgentEvent::TextDelta { role: Role::Assistant, text } => items.push(div().text_color(p.text).child(text.clone()).into_any_element()),
                AgentEvent::ToolCall(tc) | AgentEvent::ToolCallUpdate(tc) => {
                    if seen_tools.insert(tc.id.clone()) {
                        let merged = t.merged_tool(&tc.id).unwrap_or_else(|| tc.clone());
                        let open = self.expanded.contains(&(t.id.clone(), tc.id.clone()));
                        items.push(self.tool_card(i, &t.id, &merged, open, p, cx).into_any_element());
                    }
                }
                AgentEvent::Plan { entries } => items.push(
                    v_flex()
                        .gap(px(2.))
                        .p(px(8.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(p.border)
                        .child(div().text_size(px(11.)).text_color(p.muted).child("PLAN"))
                        .children(entries.iter().map(|e| {
                            let (g, c) = match e.status {
                                ToolStatus::Completed => (IconName::CircleCheck, p.ok),
                                ToolStatus::InProgress => (IconName::CircleDot, p.accent),
                                _ => (IconName::Square, p.muted),
                            };
                            h_flex().gap(px(6.)).text_size(px(12.)).child(icon(g, 12., c)).child(e.content.clone())
                        }))
                        .into_any_element(),
                ),
                AgentEvent::PermissionRequested(req) => items.push(self.permission_card(i, t, req, p, cx)),
                AgentEvent::PermissionResolved { option, by, .. } => {
                    let (g, c) = if option.contains("reject") || option.contains("cancel") { (IconName::OctagonX, p.error) } else { (IconName::ShieldCheck, p.ok) };
                    let who = match by {
                        Decider::User => "You chose",
                        Decider::Policy => "Policy chose",
                        Decider::Harness => "Harness chose",
                    };
                    items.push(h_flex().gap(px(6.)).text_size(px(12.)).text_color(p.muted).child(icon(g, 14., c)).child(format!("{who} “{option}”")).into_any_element())
                }
                AgentEvent::Error { message } => items.push(
                    h_flex().gap(px(6.)).text_size(px(12.)).text_color(p.error).child(icon(IconName::CircleAlert, 14., p.error)).child(message.clone()).into_any_element(),
                ),
                AgentEvent::Done { reason } => items.push(
                    h_flex()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(p.muted)
                        .child(icon(IconName::CircleStop, 12., p.muted))
                        .child(format!("Turn ended: {}", format!("{reason:?}").to_lowercase()))
                        .into_any_element(),
                ),
                _ => {}
            }
        }
        if let Some(err) = &t.error
            && !t.events.iter().any(|e| matches!(e, AgentEvent::Error { message } if message == err))
        {
            items.push(h_flex().gap(px(6.)).text_size(px(12.)).text_color(p.error).child(icon(IconName::CircleAlert, 14., p.error)).child(err.clone()).into_any_element());
        }
        if matches!(t.status, TaskStatus::Running | TaskStatus::Starting) {
            let label = if t.status == TaskStatus::Starting { "Creating worktree and starting the agent…" } else { "Working…" };
            items.push(h_flex().gap(px(6.)).text_color(p.muted).child(div().size(px(6.)).rounded_full().bg(p.accent)).child(label).into_any_element());
        }
        let id = t.id.clone();
        let diff_files = t.diff.as_ref().map_or(0, |d| d.files.len());
        let active = t.status.is_active();
        let usage = usage_line(t);
        let worktree = t.worktree.as_ref().and_then(|w| w.strip_prefix(&self.root).ok().map(|p| p.display().to_string())).unwrap_or_else(|| format!(".tandem/worktrees/{}", t.id));
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_none()
                    .px(px(12.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(avatar(letter, color, 18.))
                    .child(div().flex_none().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(agent_name))
                    .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(p.muted).whitespace_nowrap().overflow_hidden().text_ellipsis().child(format!("· {}", t.title)))
                    .child(h_flex().flex_none().gap(px(4.)).text_size(px(11.)).text_color(p.muted).child(icon(IconName::GitBranch, 12., p.muted)).child(worktree))
                    .when(diff_files > 0 || (t.restored && t.worktree.is_some()), |d| {
                        let id = id.clone();
                        d.child(Button::new("task-review").xsmall().label(if diff_files > 0 { format!("Review {diff_files}") } else { "Review".into() }).on_click(cx.listener(
                            move |this, _, _, cx| {
                                if this.model.task(&id).is_some_and(|t| t.diff.is_some()) {
                                    cx.emit(AgentsEvent::OpenReview(id.clone()));
                                } else {
                                    this.refresh_diff(&id, cx);
                                }
                            },
                        )))
                    })
                    .when(active, |d| {
                        let id = id.clone();
                        d.child(Button::new("task-cancel").xsmall().ghost().label("Cancel").on_click(cx.listener(move |this, _, _, cx| this.cancel_task(&id, cx))))
                    })
                    .when(!active, |d| {
                        let id = id.clone();
                        d.child(Button::new("task-delete").xsmall().ghost().label("Delete").tooltip("Remove the worktree, its branch and the task log").on_click(
                            cx.listener(move |this, _, _, cx| this.delete_task(&id, cx)),
                        ))
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .px(px(12.))
                    .h(px(24.))
                    .gap(px(10.))
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .border_b_1()
                    .border_color(p.border)
                    .children(usage.into_iter().map(|s| div().whitespace_nowrap().child(s))),
            )
            .child(div().id("transcript").flex_1().min_h_0().overflow_y_scroll().p(px(12.)).child(v_flex().gap(px(10.)).text_size(px(13.)).children(items)))
            .into_any_element()
    }

    fn tool_card(&self, i: usize, task: &TaskId, tc: &ToolCall, open: bool, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let glyph = match tc.kind {
            ToolKind::Read => IconName::FileText,
            ToolKind::Edit => IconName::FileCode,
            ToolKind::Delete => IconName::X,
            ToolKind::Move => IconName::Folder,
            ToolKind::Search => IconName::Search,
            ToolKind::Execute => IconName::Terminal,
            ToolKind::Fetch => IconName::Zap,
            ToolKind::Think => IconName::Sparkles,
            ToolKind::Other => IconName::Hammer,
        };
        let (status_icon, status_color) = match tc.status {
            ToolStatus::Completed => (IconName::Check, p.ok),
            ToolStatus::Failed => (IconName::X, p.error),
            ToolStatus::InProgress => (IconName::CircleDot, p.accent),
            ToolStatus::Pending => (IconName::CircleDot, p.muted),
        };
        let key = (task.clone(), tc.id.clone());
        let mut detail: Vec<AnyElement> = Vec::new();
        if open {
            let mono = |s: String| {
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(4.))
                    .bg(p.editor_bg)
                    .font_family(MONO_FONT)
                    .text_size(px(11.))
                    .text_color(p.text)
                    .child(s)
                    .into_any_element()
            };
            if let Some(r) = &tc.raw_input {
                detail.push(mono(r.clone()));
            }
            for l in &tc.locations {
                detail.push(div().text_size(px(11.)).text_color(p.muted).child(l.display().to_string()).into_any_element());
            }
            if let Some(d) = &tc.diff {
                detail.push(mono(crate::model::review::to_unified_patch(d)));
            }
            if let Some(c) = &tc.content {
                detail.push(mono(truncate(c, 4000)));
            }
            if detail.is_empty() {
                detail.push(div().text_size(px(11.)).text_color(p.muted).child("No input or output reported").into_any_element());
            }
        }
        v_flex()
            .rounded(px(6.))
            .border_1()
            .border_color(p.border)
            .bg(p.card_bg)
            .child(
                h_flex()
                    .id(("tool", i))
                    .px(px(10.))
                    .h(px(30.))
                    .gap(px(8.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded.remove(&key) {
                            this.expanded.insert(key.clone());
                        }
                        cx.notify();
                    }))
                    .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12., p.muted))
                    .child(icon(glyph, 14., p.muted))
                    .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(p.text).whitespace_nowrap().overflow_hidden().text_ellipsis().child(if tc.title.is_empty() { "Tool call".to_string() } else { tc.title.clone() }))
                    .child(icon(status_icon, 14., status_color)),
            )
            .when(open, |d| d.child(v_flex().gap(px(6.)).px(px(10.)).pb(px(8.)).children(detail)))
    }

    fn permission_card(&self, i: usize, t: &AgentTask, req: &PermissionRequest, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let resolved = t.is_resolved(&req.id);
        let cmd = command_of(&req.tool_call);
        if req.risk == RiskClass::Safe {
            // Safe: answered by policy inside tandem-acp, shown for the record.
            return h_flex()
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(p.muted)
                .child(icon(IconName::ShieldCheck, 14., p.muted))
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(format!("{} · {}", req.tool_call.title, req.reason)))
                .child(chip("auto", p.muted, p.muted.opacity(0.18)))
                .into_any_element();
        }
        if resolved {
            return h_flex()
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(p.muted)
                .child(icon(IconName::Lock, 14., p.muted))
                .child(div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(format!("{} permission: {}", req.risk, if cmd.is_empty() { req.tool_call.title.clone() } else { cmd })))
                .into_any_element();
        }
        let destructive = req.risk == RiskClass::Destructive;
        let edge = if destructive { p.error } else { p.accent };
        let deciding = t.deciding.contains(&req.id);
        let alive = self.handles.contains_key(&t.id);
        let mut buttons = h_flex().gap(px(8.));
        if deciding {
            buttons = buttons.child(div().text_size(px(12.)).text_color(p.muted).child("Sending decision…"));
        } else if !alive {
            buttons = buttons.child(div().text_size(px(12.)).text_color(p.muted).child("Agent process ended; this request can no longer be answered."));
        } else {
            let mk = |label: &'static str, id: &'static str, choice: Choice, cx: &mut Context<Self>| {
                let (task, req) = (t.id.clone(), req.clone());
                let b = Button::new((id, i)).small().label(label);
                let b = match choice {
                    Choice::AllowOnce if destructive => b.danger(),
                    Choice::AllowOnce => b.primary(),
                    Choice::Reject if destructive => b.primary(),
                    _ => b,
                };
                b.on_click(cx.listener(move |this, _, _, cx| this.decide(&task, &req, choice, cx)))
            };
            buttons = buttons.child(mk("Allow once", "perm-allow", Choice::AllowOnce, cx));
            if !destructive {
                buttons = buttons.child(mk("Allow always", "perm-always", Choice::AllowAlways, cx));
            }
            buttons = buttons.child(mk("Reject", "perm-reject", Choice::Reject, cx));
        }
        v_flex()
            .id(("perm-card", i))
            .rounded(px(8.))
            .border_1()
            .border_color(edge)
            .bg(if destructive { p.danger_bg } else { p.accent.opacity(0.08) })
            .overflow_hidden()
            .child(
                h_flex()
                    .px(px(10.))
                    .py(px(8.))
                    .gap(px(8.))
                    .bg(edge.opacity(0.18))
                    .child(icon(IconName::ShieldAlert, 16., edge))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(edge).child(if destructive { "Destructive action blocked" } else { "Permission required" }))
                    .child(div().flex_1())
                    .child(chip(if destructive { "DESTRUCTIVE" } else { "REVIEW" }, hex(0xFFFFFF), edge)),
            )
            .child(
                v_flex()
                    .p(px(10.))
                    .gap(px(8.))
                    .child(div().text_color(p.text).child(req.tool_call.title.clone()))
                    .when(!cmd.is_empty(), |d| {
                        d.child(
                            div()
                                .px(px(8.))
                                .py(px(6.))
                                .rounded(px(4.))
                                .bg(p.editor_bg)
                                .font_family(MONO_FONT)
                                .text_size(px(12.))
                                .text_color(p.text_bright)
                                .child(format!("$ {cmd}")),
                        )
                    })
                    .children(req.tool_call.locations.first().map(|l| {
                        let shown = t.worktree.as_ref().and_then(|w| l.strip_prefix(w).ok()).map(|r| format!("worktree/{}", r.display())).unwrap_or_else(|| l.display().to_string());
                        div().text_size(px(11.)).text_color(p.muted).child(shown)
                    }))
                    .child(h_flex().gap(px(6.)).text_size(px(12.)).child(div().text_color(edge).font_weight(FontWeight::MEDIUM).child("Why:")).child(div().text_color(p.text).child(req.reason.clone())))
                    .child(buttons),
            )
            .into_any_element()
    }

    fn render_composer(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let phase = phase_label(self.phase);
        let route = self.route();
        let (picker_label, picker_avatar): (String, Option<(&'static str, u32)>) = match &self.pinned {
            Some(a) => {
                let (l, c, n) = agent_badge(a);
                (n.to_string(), Some((l, c)))
            }
            None => ("Route for me".into(), None),
        };
        let target = self.chosen_agent().map(|(a, _)| a);
        let chips = PRESETS.iter().enumerate().map(|(i, (preset, label))| {
            let on = self.presets.contains(preset);
            let preset = *preset;
            div()
                .id(("skill", i))
                .px(px(8.))
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(px(11.))
                .border_1()
                .border_color(if on { p.accent } else { p.border })
                .bg(if on { p.accent.opacity(0.18) } else { p.panel_bg })
                .text_size(px(12.))
                .text_color(if on { p.text_bright } else { p.muted })
                .cursor_pointer()
                .child(*label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.presets.remove(&preset) {
                        this.presets.insert(preset);
                    }
                    cx.notify();
                }))
        });
        let can_reply = self.can_reply();
        v_flex()
            .flex_none()
            .border_t_1()
            .border_color(p.border)
            .p(px(10.))
            .gap(px(8.))
            .child(
                div()
                    .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                        if ev.keystroke.key == "enter" && ev.keystroke.modifiers.platform {
                            cx.stop_propagation();
                            this.submit(window, cx);
                        }
                    }))
                    .child(Textarea::new(&self.composer).h(px(64.))),
            )
            .child(
                h_flex()
                    .gap(px(6.))
                    .flex_wrap()
                    .child(
                        Button::new("agent-picker")
                            .small()
                            .ghost()
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .children(picker_avatar.map(|(l, c)| avatar(l, c, 16.)))
                                    .when(picker_avatar.is_none(), |d| d.child(icon(IconName::Sparkles, 14., p.accent)))
                                    .child(picker_label)
                                    .child(icon(IconName::ChevronDown, 12., p.muted)),
                            )
                            .dropdown_menu({
                                let view = cx.entity();
                                let agents: Vec<(AgentId, String, Option<String>)> = self
                                    .detected
                                    .iter()
                                    .map(|d| {
                                        let hint = matches!(d.availability, Availability::Installable).then(|| d.descriptor.install_hint.clone());
                                        (d.descriptor.id.clone(), d.descriptor.display_name.clone(), hint)
                                    })
                                    .collect();
                                let missing: Vec<(String, String)> = tandem_acp::builtin()
                                    .into_iter()
                                    .filter(|b| !agents.iter().any(|(id, _, _)| *id == b.id))
                                    .map(|b| (b.display_name, b.install_hint))
                                    .collect();
                                move |mut menu, _, _| {
                                    let v = view.clone();
                                    menu = menu.item(PopupMenuItem::new("Route for me (quota headroom)").on_click(move |_, _, cx| {
                                        v.update(cx, |this, cx| {
                                            this.pinned = None;
                                            cx.notify();
                                        })
                                    }));
                                    menu = menu.separator();
                                    for (id, name, hint) in &agents {
                                        let v = view.clone();
                                        let id = id.clone();
                                        let label = match hint {
                                            Some(h) => format!("{name}  (install: {h})"),
                                            None => name.clone(),
                                        };
                                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                            v.update(cx, |this, cx| {
                                                this.pinned = Some(id.clone());
                                                cx.notify();
                                            })
                                        }));
                                    }
                                    if !missing.is_empty() {
                                        menu = menu.separator();
                                        for (name, hint) in &missing {
                                            menu = menu.item(PopupMenuItem::new(format!("{name} not installed: {hint}")).disabled(true));
                                        }
                                    }
                                    menu
                                }
                            }),
                    )
                    .child(
                        Button::new("phase-picker")
                            .small()
                            .ghost()
                            .child(h_flex().gap(px(6.)).child(div().text_color(p.muted).child("Phase:")).child(phase).child(icon(IconName::ChevronDown, 12., p.muted)))
                            .dropdown_menu({
                                let view = cx.entity();
                                move |mut menu, _, _| {
                                    for ph in [TaskPhase::Plan, TaskPhase::Implement, TaskPhase::Test, TaskPhase::Review, TaskPhase::Any] {
                                        let view = view.clone();
                                        menu = menu.item(PopupMenuItem::new(phase_label(ph)).on_click(move |_, _, cx| {
                                            view.update(cx, |this, cx| {
                                                this.phase = ph;
                                                cx.notify();
                                            });
                                        }));
                                    }
                                    menu
                                }
                            }),
                    )
                    .children(chips),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .when_some(route.filter(|_| self.pinned.is_none()), |d, r| {
                        let (l, c, n) = agent_badge(&r.agent);
                        d.child(
                            h_flex()
                                .id("route-chip")
                                .min_w_0()
                                .gap(px(6.))
                                .px(px(8.))
                                .h(px(22.))
                                .rounded(px(11.))
                                .bg(p.accent.opacity(0.15))
                                .text_size(px(11.))
                                .text_color(p.text)
                                .child(icon(IconName::Sparkles, 12., p.accent))
                                .child(avatar(l, c, 14.))
                                .child(div().whitespace_nowrap().child(format!("→ {n}")))
                                .child(div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(p.muted).child(r.reason)),
                        )
                    })
                    .children(target.map(|a| div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(11.)).text_color(p.muted).child(self.estimate_label(&a))))
                    .child(div().flex_1())
                    .when(can_reply, |d| {
                        d.child(Button::new("reply").small().label("Reply in task").on_click(cx.listener(|this, _, window, cx| this.submit_reply(window, cx))))
                    })
                    .child(Button::new("start-task").small().primary().label("Start task").on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))))
                    .child(
                        div()
                            .id("stop-all")
                            .flex_none()
                            .h(px(28.))
                            .px(px(14.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(5.))
                            .bg(hex(0xDB3B4B))
                            .hover(|s| s.bg(hex(0xE5505F)))
                            .text_color(hex(0xFFFFFF))
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(12.))
                            .cursor_pointer()
                            .child(icon(IconName::OctagonX, 14., hex(0xFFFFFF)))
                            .child("STOP ALL")
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(StopAllAgents), cx)),
                    ),
            )
    }
}

fn pick_option(req: &PermissionRequest, choice: Choice) -> Option<String> {
    let by_kind = |k: PermissionOptionKind| req.options.iter().find(|o| o.kind == k).map(|o| o.id.clone());
    let by_word = |w: &str| req.options.iter().find(|o| o.id.to_lowercase().contains(w) || o.label.to_lowercase().contains(w)).map(|o| o.id.clone());
    match choice {
        Choice::AllowOnce => by_kind(PermissionOptionKind::AllowOnce).or_else(|| by_kind(PermissionOptionKind::AllowAlways)).or_else(|| by_word("allow")),
        Choice::AllowAlways => by_kind(PermissionOptionKind::AllowAlways).or_else(|| by_kind(PermissionOptionKind::AllowOnce)).or_else(|| by_word("allow")),
        Choice::Reject => by_kind(PermissionOptionKind::RejectOnce).or_else(|| by_kind(PermissionOptionKind::RejectAlways)).or_else(|| by_word("reject")),
    }
}

/// The exact shell command / input behind a tool call.
pub fn command_of(tc: &ToolCall) -> String {
    tc.raw_input.clone().unwrap_or_default().trim().to_string()
}

fn usage_line(t: &AgentTask) -> Vec<String> {
    let mut v = vec![];
    let tokens = t.tokens();
    v.push(if tokens > 0 { format!("{} tokens", fmt_tokens(tokens)) } else { "tokens —".into() });
    if let Some(c) = t.usage.cost_usd {
        v.push(format!("${c:.2}"));
    }
    v.push(t.elapsed_label());
    v.push(format!("phase {}", phase_label(t.phase).to_lowercase()));
    if !t.presets.is_empty() {
        v.push(format!("skills: {}", t.presets.join(", ")));
    }
    if let Some(r) = &t.route_reason {
        v.push(format!("routed: {r}"));
    }
    v
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…\n[{} more characters]", s.chars().take(n).collect::<String>(), s.chars().count() - n) }
}

pub fn fmt_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1e6)
    } else if t >= 1_000 {
        format!("{:.1}k", t as f64 / 1e3)
    } else {
        t.to_string()
    }
}

fn fmt_secs(s: f64) -> String {
    if s >= 60.0 { format!("{:.0}m", s / 60.0) } else { format!("{s:.0}s") }
}

fn countdown(d: chrono::Duration) -> String {
    let m = d.num_minutes().max(0);
    if m >= 24 * 60 {
        format!("in {}d {}h", m / (24 * 60), (m % (24 * 60)) / 60)
    } else if m >= 60 {
        format!("in {}h {:02}m", m / 60, m % 60)
    } else {
        format!("in {m}m")
    }
}

pub fn phase_label(ph: TaskPhase) -> &'static str {
    match ph {
        TaskPhase::Plan => "Plan",
        TaskPhase::Implement => "Implement",
        TaskPhase::Test => "Test",
        TaskPhase::Review => "Review",
        TaskPhase::Any => "Any",
    }
}

fn status_colors(s: TaskStatus, p: &Palette) -> (Hsla, Hsla) {
    match s {
        TaskStatus::Starting | TaskStatus::Running => (p.accent, p.accent.opacity(0.18)),
        TaskStatus::NeedsApproval => (p.error, p.error.opacity(0.18)),
        TaskStatus::Done => (p.ok, p.ok.opacity(0.18)),
        TaskStatus::Failed | TaskStatus::Cancelled => (p.muted, p.muted.opacity(0.2)),
    }
}

pub fn avatar(letter: &'static str, color: u32, size: f32) -> impl IntoElement {
    avatar_owned(letter, color, size)
}

fn avatar_owned(letter: impl Into<SharedString>, color: u32, size: f32) -> Div {
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(hex(color))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.5))
        .font_weight(FontWeight::BOLD)
        .text_color(hex(0xFFFFFF))
        .child(letter.into())
}

impl Render for AgentsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_quota();
        let p = pal(cx).clone();
        let rows: Vec<(usize, AgentTask)> = self.model.tasks.iter().cloned().enumerate().collect();
        let tasks: Vec<AnyElement> = rows.iter().rev().map(|(i, t)| self.render_task_row(*i, t, &p, cx).into_any_element()).collect();
        let waiting = self.model.waiting_count();
        v_flex()
            .size_full()
            .text_size(px(13.))
            .child(self.render_quota(&p, cx))
            .when(self.kill_tripped, |d| {
                d.child(
                    h_flex()
                        .flex_none()
                        .px(px(12.))
                        .py(px(6.))
                        .gap(px(8.))
                        .bg(p.danger_bg)
                        .border_b_1()
                        .border_color(p.error)
                        .child(icon(IconName::OctagonX, 14., p.error))
                        .child(div().flex_1().text_size(px(12.)).text_color(p.text).child("Kill switch engaged: every agent was stopped and new agents are refused."))
                        .child(Button::new("rearm").xsmall().label("Re-arm agents").on_click(cx.listener(|this, _, _, cx| this.rearm(cx)))),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(
                        v_flex()
                            .id("tasks")
                            .w(px(230.))
                            .h_full()
                            .flex_none()
                            .border_r_1()
                            .border_color(p.border)
                            .py(px(6.))
                            .gap(px(2.))
                            .overflow_y_scroll()
                            .child(
                                h_flex()
                                    .px(px(12.))
                                    .h(px(22.))
                                    .text_size(px(11.))
                                    .text_color(p.muted)
                                    .child(format!("TASKS  {}", self.model.tasks.len()))
                                    .child(div().flex_1())
                                    .when(waiting > 0, |d| d.child(chip(format!("{waiting} waiting"), p.error, p.error.opacity(0.18)))),
                            )
                            .children(tasks),
                    )
                    .child(div().flex_1().h_full().min_w_0().bg(p.editor_bg).child(self.render_transcript(&p, cx))),
            )
            .child(self.render_composer(&p, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` exports its own `test` attribute; use the std one.
    use core::prelude::v1::test;
    use tandem_core::{PermissionOption, RequestId};

    #[test]
    fn picks_options_by_kind_with_fallbacks() {
        let req = PermissionRequest {
            id: RequestId::from("r"),
            tool_call: ToolCall { id: "t".into(), title: "x".into(), kind: ToolKind::Execute, status: ToolStatus::Pending, locations: vec![], raw_input: Some(" npm test ".into()), content: None, diff: None },
            options: vec![
                PermissionOption { id: "allow".into(), label: "Allow".into(), kind: PermissionOptionKind::AllowOnce },
                PermissionOption { id: "reject".into(), label: "Reject".into(), kind: PermissionOptionKind::RejectOnce },
            ],
            risk: RiskClass::Review,
            reason: String::new(),
        };
        assert_eq!(pick_option(&req, Choice::AllowAlways).as_deref(), Some("allow"));
        assert_eq!(pick_option(&req, Choice::Reject).as_deref(), Some("reject"));
        assert_eq!(command_of(&req.tool_call), "npm test");
        assert_eq!(countdown(chrono::Duration::minutes(134)), "in 2h 14m");
    }
}
