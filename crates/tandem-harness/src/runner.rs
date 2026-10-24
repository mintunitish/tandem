//! The seam between the harness and real agents.
//!
//! `tandem-harness` never talks to agents directly. It drives an [`AgentRunner`]
//! (the lead wires an `AcpRunner` adapter over `tandem-acp`), and ships a
//! [`MockRunner`] that replays scripted steps for tests and dry runs.

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tandem_core::{
    AgentEvent, AgentId, Decider, EventTx, FileChange, FileDiff, PermissionDecision, PermissionOption,
    PermissionOptionKind, PermissionRequest, RequestId, RiskClass, SessionId, StopReason, TaskEvent, TaskId,
    ToolCall, ToolCallId, ToolKind, ToolStatus, Usage,
};

/// Result of one agent turn.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunOutcome {
    pub stop: StopReason,
    /// Total usage if the runner knows it. The harness prefers the sum of
    /// `AgentEvent::Usage` events (treated as deltas) and falls back to this.
    pub usage: Usage,
    pub elapsed: Duration,
}

/// Runs one prompt against one agent in `cwd` and streams every event to `tx`.
///
/// Contract:
/// - Send every `AgentEvent` (including `PermissionRequested`/`PermissionResolved`,
///   `ToolCall`/`ToolCallUpdate` with final status, `Usage`, `Done`) on `tx`.
/// - Route every permission request through `decider` and honour its choice.
/// - Return when the turn ends. If the future is dropped (harness timeout or
///   cancel scenario), the agent process must be stopped (e.g. `kill_on_drop`).
#[async_trait]
pub trait AgentRunner: Send + Sync {
    async fn run(
        &self,
        agent: &AgentId,
        cwd: &Path,
        prompt: &str,
        decider: Arc<dyn PermissionDecider>,
        tx: EventTx,
    ) -> anyhow::Result<RunOutcome>;
}

/// Answers permission requests on behalf of the harness (no human in the loop).
pub trait PermissionDecider: Send + Sync {
    fn decide(&self, req: &PermissionRequest) -> PermissionDecision;
}

/// Allow or reject; maps onto the request's concrete option ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Reject,
}

impl Decision {
    /// The option kind this decision prefers ("once"; "always" is the fallback).
    pub fn kind(self) -> PermissionOptionKind {
        match self {
            Decision::Allow => PermissionOptionKind::AllowOnce,
            Decision::Reject => PermissionOptionKind::RejectOnce,
        }
    }
}

/// Picks the option id matching `decision`, preferring "once" over "always" (the same
/// mapping as `AgentHandle::respond_choice`: [`PermissionRequest::option_for`]).
pub fn choose_option(req: &PermissionRequest, decision: Decision) -> PermissionDecision {
    let id = req.option_for(decision.kind()).map(|o| o.id.clone()).unwrap_or_else(|| match decision {
        Decision::Allow => "allow".into(),
        Decision::Reject => "reject".into(),
    });
    PermissionDecision { request: req.id.clone(), option_id: id }
}

pub struct RejectAll;
impl PermissionDecider for RejectAll {
    fn decide(&self, req: &PermissionRequest) -> PermissionDecision {
        choose_option(req, Decision::Reject)
    }
}

pub struct AllowAll;
impl PermissionDecider for AllowAll {
    fn decide(&self, req: &PermissionRequest) -> PermissionDecision {
        choose_option(req, Decision::Allow)
    }
}

/// Allows `RiskClass::Safe`, rejects everything else.
pub struct AllowSafe;
impl PermissionDecider for AllowSafe {
    fn decide(&self, req: &PermissionRequest) -> PermissionDecision {
        let d = if req.risk == RiskClass::Safe { Decision::Allow } else { Decision::Reject };
        choose_option(req, d)
    }
}

/// Allows `Safe` and `Review`, rejects `Destructive`.
pub struct AllowNonDestructive;
impl PermissionDecider for AllowNonDestructive {
    fn decide(&self, req: &PermissionRequest) -> PermissionDecision {
        let d = if req.risk == RiskClass::Destructive { Decision::Reject } else { Decision::Allow };
        choose_option(req, d)
    }
}

/// First matching rule wins (regex over raw_input, falling back to title); default reject.
pub struct ScriptedDecider {
    pub rules: Vec<(Regex, Option<ToolKind>, Decision)>,
}
impl PermissionDecider for ScriptedDecider {
    fn decide(&self, req: &PermissionRequest) -> PermissionDecision {
        let tc = &req.tool_call;
        let hay = tc.raw_input.as_deref().unwrap_or(&tc.title);
        let d = self
            .rules
            .iter()
            .find(|(re, kind, _)| kind.is_none_or(|k| k == tc.kind) && re.is_match(hay))
            .map(|r| r.2)
            .unwrap_or(Decision::Reject);
        choose_option(req, d)
    }
}

// ---------------------------------------------------------------------------
// MockRunner
// ---------------------------------------------------------------------------

/// Filesystem side effect of a mock tool call, relative to `cwd`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsEffect {
    Write { path: String, content: String },
    Remove(String),
    /// Delete everything in cwd except `.git` (a destructive `rm -rf *`).
    RemoveAll,
}

/// One scripted step.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MockStep {
    /// Emit an arbitrary event verbatim.
    Event(Box<AgentEvent>),
    /// Assistant text.
    Text(String),
    SleepMs(u64),
    /// A tool call: optionally asks permission first, then applies `effect`.
    Tool {
        kind: ToolKind,
        raw_input: String,
        #[serde(default)]
        effect: Option<FsEffect>,
        #[serde(default)]
        ask_permission: bool,
        #[serde(default = "default_risk")]
        risk: RiskClass,
        /// Output text reported in the final update.
        #[serde(default)]
        output: Option<String>,
    },
    Usage(Usage),
    /// End the turn with this reason.
    Stop(StopReason),
}

fn default_risk() -> RiskClass {
    RiskClass::Review
}

/// A script for one agent, optionally only for prompts containing `prompt_contains`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MockScript {
    pub agent: AgentId,
    #[serde(default)]
    pub prompt_contains: Option<String>,
    pub steps: Vec<MockStep>,
}

/// Replays scripted steps. Unknown agents get a single "ok" text and `EndTurn`.
#[derive(Clone, Debug, Default)]
pub struct MockRunner {
    pub scripts: Vec<MockScript>,
}

impl MockRunner {
    pub fn new() -> Self {
        Self::default()
    }
    /// Script used for every prompt sent to `agent` (unless a prompt-specific one matches).
    pub fn script(mut self, agent: &str, steps: Vec<MockStep>) -> Self {
        self.scripts.push(MockScript { agent: agent.into(), prompt_contains: None, steps });
        self
    }
    pub fn from_yaml(text: &str) -> anyhow::Result<Self> {
        Ok(Self { scripts: crate::scenario::from_yaml(text)? })
    }

    fn steps_for(&self, agent: &AgentId, prompt: &str) -> Vec<MockStep> {
        let mine = self.scripts.iter().filter(|s| &s.agent == agent);
        mine.clone()
            .find(|s| s.prompt_contains.as_deref().is_some_and(|n| prompt.contains(n)))
            .or_else(|| mine.clone().find(|s| s.prompt_contains.is_none()))
            .map(|s| s.steps.clone())
            .unwrap_or_else(|| vec![MockStep::Text("ok".into())])
    }
}

fn apply_effect(cwd: &Path, effect: &FsEffect) -> std::io::Result<Option<FileDiff>> {
    match effect {
        FsEffect::Write { path, content } => {
            let p = cwd.join(path);
            let old = std::fs::read_to_string(&p).ok();
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, content)?;
            Ok(Some(FileDiff {
                path: PathBuf::from(path),
                old_path: None,
                change: if old.is_some() { FileChange::Modified } else { FileChange::Added },
                old_text: old,
                new_text: Some(content.clone()),
                hunks: vec![],
                binary: false,
            }))
        }
        FsEffect::Remove(path) => {
            let p = cwd.join(path);
            if p.is_dir() {
                std::fs::remove_dir_all(&p)?;
            } else if p.exists() {
                std::fs::remove_file(&p)?;
            }
            Ok(None)
        }
        FsEffect::RemoveAll => {
            for entry in std::fs::read_dir(cwd)? {
                let entry = entry?;
                if entry.file_name() == ".git" {
                    continue;
                }
                let p = entry.path();
                if p.is_dir() {
                    std::fs::remove_dir_all(&p)?;
                } else {
                    std::fs::remove_file(&p)?;
                }
            }
            Ok(None)
        }
    }
}

#[async_trait]
impl AgentRunner for MockRunner {
    async fn run(
        &self,
        agent: &AgentId,
        cwd: &Path,
        prompt: &str,
        decider: Arc<dyn PermissionDecider>,
        tx: EventTx,
    ) -> anyhow::Result<RunOutcome> {
        let start = Instant::now();
        let task = TaskId::new();
        let send = |event: AgentEvent| {
            let tx = tx.clone();
            let task = task.clone();
            async move { tx.send(TaskEvent { task, event }).await.map_err(|e| anyhow::anyhow!("event channel closed: {e}")) }
        };
        send(AgentEvent::SessionStarted { session: SessionId::new(), agent: agent.clone(), cwd: cwd.to_path_buf() })
            .await?;
        let mut usage = Usage::default();
        let mut stop = StopReason::EndTurn;
        for step in self.steps_for(agent, prompt) {
            match step {
                MockStep::Event(e) => send(*e).await?,
                MockStep::Text(text) => send(AgentEvent::TextDelta { role: tandem_core::Role::Assistant, text }).await?,
                MockStep::SleepMs(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                MockStep::Usage(u) => {
                    usage.add(&u);
                    send(AgentEvent::Usage(u)).await?;
                }
                MockStep::Stop(r) => {
                    stop = r;
                    break;
                }
                MockStep::Tool { kind, raw_input, effect, ask_permission, risk, output } => {
                    let mut call = ToolCall {
                        id: ToolCallId::new(),
                        title: raw_input.clone(),
                        kind,
                        status: ToolStatus::Pending,
                        locations: vec![],
                        raw_input: Some(raw_input),
                        content: None,
                        diff: None,
                    };
                    send(AgentEvent::ToolCall(call.clone())).await?;
                    if ask_permission {
                        let req = PermissionRequest {
                            id: RequestId::new(),
                            tool_call: call.clone(),
                            options: vec![
                                PermissionOption { id: "allow".into(), label: "Allow".into(), kind: PermissionOptionKind::AllowOnce },
                                PermissionOption { id: "reject".into(), label: "Reject".into(), kind: PermissionOptionKind::RejectOnce },
                            ],
                            risk,
                            reason: "mock".into(),
                        };
                        send(AgentEvent::PermissionRequested(req.clone())).await?;
                        let d = decider.decide(&req);
                        send(AgentEvent::PermissionResolved { request: req.id.clone(), option: d.option_id.clone(), by: Decider::Harness })
                            .await?;
                        let allowed = req
                            .options
                            .iter()
                            .find(|o| o.id == d.option_id)
                            .is_some_and(|o| o.kind.is_allow());
                        if !allowed {
                            call.status = ToolStatus::Failed;
                            call.content = Some("permission denied".into());
                            send(AgentEvent::ToolCallUpdate(call)).await?;
                            continue;
                        }
                    }
                    let res = match &effect {
                        Some(e) => apply_effect(cwd, e),
                        None => Ok(None),
                    };
                    match res {
                        Ok(diff) => {
                            call.status = ToolStatus::Completed;
                            call.diff = diff;
                            call.content = output;
                        }
                        Err(e) => {
                            call.status = ToolStatus::Failed;
                            call.content = Some(e.to_string());
                        }
                    }
                    send(AgentEvent::ToolCallUpdate(call)).await?;
                }
            }
        }
        send(AgentEvent::Done { reason: stop }).await?;
        Ok(RunOutcome { stop, usage, elapsed: start.elapsed() })
    }
}
