//! Task orchestration: the full "start an agent task" flow from the design spec.
//!
//! prompt → `tandem_git` worktree `.tandem/worktrees/<id>` → `tandem_skills::materialize` →
//! optional `tandem-db-mcp` MCP server → `AcpAgent::spawn` (Seatbelt per policy, scrubbed env,
//! tandem-sandbox classifier, Kill Switch) → events streamed to the cockpit and persisted to
//! `.tandem/tasks/<id>/log.jsonl` → quota providers fed with every event → on `Done` the worktree
//! diff (minus Tandem-managed skill files) goes to the Review panel.
//!
//! Everything here runs on the shared tokio runtime (`crate::runtime`); the cockpit receives
//! [`TaskMsg`]s over an `async_channel`.

pub mod actions;
pub mod blame;
pub mod db;
pub mod quota;
pub mod store;
pub mod terminal;

use crate::model::agents::AgentTask;
use anyhow::{Context as _, Result, anyhow};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tandem_acp::{AcpAgent, AgentHandle, Availability, DetectedAgent, McpServerSpec, ReplayLog, SpawnOptions};
use tandem_core::{AgentDescriptor, AgentEvent, AgentId, DiffSet, FileDiff, Policy, TaskEvent, TaskId};
use tandem_quota::QuotaPool;
use tandem_skills::{AgentTarget, Preset, SkillId};

/// What the tokio side reports back to the cockpit for one task.
pub enum TaskMsg {
    /// Worktree created at `worktree` from `base`; skills materialized.
    Prepared { worktree: PathBuf, base: String, notes: Vec<String> },
    /// Agent process up and session open.
    Spawned(Arc<AgentHandle>),
    Event(Box<AgentEvent>),
    /// Worktree diff after a turn ended.
    Diff(Box<DiffSet>),
    /// Setup failed before the agent could run.
    Failed(String),
}

/// Everything needed to start one task.
#[derive(Clone)]
pub struct StartSpec {
    pub id: TaskId,
    pub desc: AgentDescriptor,
    pub prompt: String,
    pub root: PathBuf,
    pub policy: Policy,
    pub presets: Vec<Preset>,
    pub mcp: Vec<McpServerSpec>,
}

/// New short task id: `<agent>-<8 hex>` (valid as a worktree / branch name).
pub fn new_task_id(agent: &AgentId) -> TaskId {
    let uuid = TaskId::new();
    let agent: String = agent.as_str().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    TaskId::from(format!("{agent}-{}", &uuid.as_str()[..8]))
}

/// `<root>/.tandem/tasks/<id>`: replay log, task.json, review.json.
pub fn task_dir(root: &Path, id: &TaskId) -> PathBuf {
    root.join(".tandem").join("tasks").join(id.as_str())
}

pub fn skills_for(presets: &[Preset]) -> Vec<SkillId> {
    let mut v: Vec<SkillId> = presets.iter().flat_map(|p| p.skills()).collect();
    v.sort();
    v.dedup();
    v
}

/// Spawn the whole pipeline on tokio. Messages arrive on the returned receiver until the agent
/// process exits.
pub fn start(spec: StartSpec, pool: Option<Arc<QuotaPool>>) -> async_channel::Receiver<TaskMsg> {
    let (tx, rx) = async_channel::unbounded();
    crate::runtime::tokio().spawn(async move {
        if let Err(e) = run(spec, pool, tx.clone()).await {
            let _ = tx.send(TaskMsg::Failed(format!("{e:#}"))).await;
        }
    });
    rx
}

async fn run(spec: StartSpec, pool: Option<Arc<QuotaPool>>, ui: async_channel::Sender<TaskMsg>) -> Result<()> {
    let StartSpec { id, desc, prompt, root, policy, presets, mcp } = spec;
    // 1. Worktree + skills (blocking git / fs work).
    let (worktree, base, notes) = {
        let (root, id, presets) = (root.clone(), id.clone(), presets.clone());
        tokio::task::spawn_blocking(move || prepare(&root, &id, &presets)).await.context("prepare task")??
    };
    let mut notes = notes;
    let (mcp, sandbox_note) = gate_mcp_for_sandbox(&policy, mcp);
    if let Some(note) = sandbox_note {
        tracing::warn!(task = %id, "{note}");
        // The "attached via MCP" notes from `db::mcp_specs` no longer hold.
        notes.retain(|n| !n.contains("attached via MCP"));
        notes.push(note.to_string());
    }
    ui.send(TaskMsg::Prepared { worktree: worktree.clone(), base: base.clone(), notes }).await.ok();

    // 2. Replay log. The prompt reaches it (and the UI) through the event stream:
    //    `AgentHandle::prompt` emits it as `TextDelta { role: User }`.
    let log_dir = task_dir(&root, &id);
    let log = ReplayLog::open(&log_dir)?;

    // 3. Spawn the agent: cwd = worktree, sandbox + classifier from policy.
    let mut opts = SpawnOptions::new(&worktree, &root, policy);
    opts.mcp_servers = mcp;
    // Bounded: a stalled consumer pauses the ACP reader instead of growing memory.
    let (tx, rx) = tandem_core::bounded_events(tandem_core::EVENT_CAPACITY);
    let handle = Arc::new(AcpAgent::spawn(&desc, opts, tx, id.clone()).await.with_context(|| format!("start {}", desc.display_name))?);
    ui.send(TaskMsg::Spawned(handle.clone())).await.ok();

    // 4. Forward events: log, quota, UI; diff after every finished turn.
    let agent = desc.id.clone();
    let log = spawn_log_writer(log_dir, log, id.clone(), ui.clone());
    tokio::spawn(forward(rx, ui, log, pool, agent, root, id, base));
    // 5. First turn. Events stream through the forwarder meanwhile; follow-up prompts reuse `handle`.
    let _ = handle.prompt(prompt).await;
    Ok(())
}

/// Shown when exposed database connections are withheld because the task runs sandboxed.
pub const DB_SANDBOX_NOTICE: &str = "Database access for agents needs the sandbox off for this task (v0.1 limitation). Toggle Sandbox in Settings → Policy or use the Database panel yourself.";

/// v0.1 limitation: `tandem-db-mcp` would be spawned inside the agent's Seatbelt profile and
/// could not read `connections.json`, the Keychain password or the confirm file. With the
/// sandbox on, the MCP specs are therefore not attached (and the user is told why).
fn gate_mcp_for_sandbox(policy: &Policy, mcp: Vec<McpServerSpec>) -> (Vec<McpServerSpec>, Option<&'static str>) {
    if policy.sandbox && !mcp.is_empty() { (vec![], Some(DB_SANDBOX_NOTICE)) } else { (mcp, None) }
}

fn prepare(root: &Path, id: &TaskId, presets: &[Preset]) -> Result<(PathBuf, String, Vec<String>)> {
    let repo = tandem_git::Repo::open(root).map_err(|e| anyhow!("{root:?} is not a git repository: {e}"))?;
    let base = repo.head_id()?.ok_or_else(|| anyhow!("the repository has no commits yet; commit once before starting agent tasks"))?;
    let worktree = repo.create_task_worktree(id, Some(&base))?;
    let mut notes = vec![];
    let skills = skills_for(presets);
    if !skills.is_empty() {
        match tandem_skills::materialize(&worktree, &skills, &AgentTarget::ALL) {
            Ok(r) => notes.push(format!("{} skills materialized ({})", r.skills.len(), presets.iter().map(|p| p.name()).collect::<Vec<_>>().join(", "))),
            Err(e) => notes.push(format!("skills not materialized: {e:#}")),
        }
    }
    Ok((worktree, base, notes))
}

/// The replay-log writer: one thread per task that owns the [`ReplayLog`], fed over a
/// channel so the forwarder never waits on disk. Flushes after [`tandem_acp::log::FLUSH_INTERVAL`]
/// of idleness and when the channel closes. A failed write reopens the log once; if that
/// fails too the UI gets an `AgentEvent::Error` (once) and the remaining events are dropped
/// from the log.
fn spawn_log_writer(dir: PathBuf, log: ReplayLog, id: TaskId, ui: async_channel::Sender<TaskMsg>) -> std::sync::mpsc::Sender<AgentEvent> {
    use std::sync::mpsc::RecvTimeoutError;
    let (tx, rx) = std::sync::mpsc::channel::<AgentEvent>();
    let spawned = std::thread::Builder::new().name(format!("log-{id}")).spawn(move || {
        let mut log = Some(log);
        let mut reopened = false;
        let mut dirty = false;
        let fail = |e: anyhow::Error| {
            let message = format!("replay log write failed: {e:#}");
            tracing::warn!(task = %id, "{message}");
            let _ = ui.send_blocking(TaskMsg::Event(Box::new(AgentEvent::Error { message })));
        };
        loop {
            let event = match rx.recv_timeout(tandem_acp::log::FLUSH_INTERVAL) {
                Ok(e) => e,
                Err(RecvTimeoutError::Timeout) => {
                    if dirty && let Some(l) = log.as_mut() {
                        let _ = l.flush();
                        dirty = false;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let Some(l) = log.as_mut() else { continue };
            if let Err(e) = l.record(&id, &event) {
                if reopened {
                    log = None;
                    fail(e);
                    continue;
                }
                reopened = true;
                match ReplayLog::open(&dir).and_then(|mut l| l.record(&id, &event).map(|()| l)) {
                    Ok(l) => log = Some(l),
                    Err(e) => {
                        log = None;
                        fail(e);
                        continue;
                    }
                }
            }
            dirty = true;
        }
        if let Some(mut l) = log {
            let _ = l.flush();
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("replay log thread: {e}");
    }
    tx
}

/// Forward a task's events: quota pool and UI first, then the replay-log writer (never
/// blocks); after every finished turn the worktree diff is computed on its own task. This
/// loop must keep up with the agent: the event channel is bounded.
#[allow(clippy::too_many_arguments)]
async fn forward(
    rx: tandem_core::EventRx,
    ui: async_channel::Sender<TaskMsg>,
    log: std::sync::mpsc::Sender<AgentEvent>,
    pool: Option<Arc<QuotaPool>>,
    agent: AgentId,
    root: PathBuf,
    id: TaskId,
    base: String,
) {
    while let Ok(TaskEvent { event, .. }) = rx.recv().await {
        if let Some(p) = &pool {
            p.ingest(&agent, &event);
        }
        let done = matches!(event, AgentEvent::Done { .. });
        // Nobody listens once the task is deleted; keep logging until the agent exits.
        let listening = ui.send(TaskMsg::Event(Box::new(event.clone()))).await.is_ok();
        let _ = log.send(event);
        if !listening || !done {
            continue;
        }
        if let Some(p) = pool.clone() {
            tokio::spawn(async move { p.reconcile_all().await });
        }
        let (root, id, agent, base, ui) = (root.clone(), id.clone(), agent.clone(), base.clone(), ui.clone());
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || compute_diff(&root, &id, &agent, &base)).await {
                Ok(Ok(ds)) => {
                    let _ = ui.send(TaskMsg::Diff(Box::new(ds))).await;
                }
                Ok(Err(e)) => {
                    let _ = ui.send(TaskMsg::Event(Box::new(AgentEvent::Error { message: format!("diff failed: {e:#}") }))).await;
                }
                Err(_) => {}
            }
        });
    }
}

/// Task worktree vs base, without the skill files Tandem itself wrote.
pub fn compute_diff(root: &Path, id: &TaskId, agent: &AgentId, base: &str) -> Result<DiffSet> {
    let repo = tandem_git::Repo::open(root)?;
    let ds = repo.diff_task(id, agent, base)?;
    Ok(filter_managed(ds))
}

/// Drop `.claude/skills`, `.agents/skills`, ... and the generated index block in
/// `AGENTS.md` / `CLAUDE.md` from a task diff.
pub fn filter_managed(mut ds: DiffSet) -> DiffSet {
    let managed = tandem_skills::managed_paths();
    ds.files.retain(|f| !managed.iter().any(|m| f.path.starts_with(m)));
    let files = std::mem::take(&mut ds.files);
    ds.files = files.into_iter().filter_map(strip_index_block).collect();
    ds
}

fn strip_block(s: &str) -> String {
    let (start, end) = (tandem_skills::BLOCK_START, tandem_skills::BLOCK_END);
    let Some(a) = s.find(start) else { return s.to_string() };
    let Some(b) = s[a..].find(end).map(|i| a + i + end.len()) else { return s.to_string() };
    let (head, tail) = (s[..a].trim_end_matches('\n'), s[b..].trim_start_matches('\n'));
    match (head.is_empty(), tail.is_empty()) {
        (true, true) => String::new(),
        (true, false) => tail.to_string(),
        (false, true) => format!("{head}\n"),
        (false, false) => format!("{head}\n\n{tail}"),
    }
}

fn strip_index_block(f: FileDiff) -> Option<FileDiff> {
    let name = f.path.to_string_lossy();
    if name != "AGENTS.md" && name != "CLAUDE.md" {
        return Some(f);
    }
    let new = f.new_text.as_deref().map(strip_block);
    let old = f.old_text.as_deref().map(strip_block);
    let (old_n, new_n) = (old.as_deref().unwrap_or("").trim_end(), new.as_deref().unwrap_or("").trim_end());
    if old_n == new_n {
        return None;
    }
    let old = old.filter(|_| f.old_text.is_some());
    let new = new.filter(|s| !(f.old_text.is_none() && s.trim().is_empty()));
    Some(tandem_review::diff_texts(f.path.clone(), old.as_deref(), new.as_deref()))
}

/// Agents this machine can run: built-ins found on PATH (or via npx) plus custom ACP agents
/// from the user's `~/.tandem/agents.json` and the project's `.tandem/agents.json` (arrays of
/// `AgentDescriptor`). Custom agents never replace a built-in (or, for project agents, a
/// user agent) of the same id. User agents' capability fields are validated, project
/// agents' are ignored ([`tandem_sandbox::descriptor::sanitize`]). Also returns one-line
/// notes about rejected entries for the UI.
pub fn detect_agents(root: &Path) -> (Vec<DetectedAgent>, Vec<String>) {
    let installed = tandem_acp::detect_installed();
    detect_agents_with(installed, &tandem_core::fs::tandem_home().join("agents.json"), &root.join(".tandem/agents.json"))
}

fn detect_agents_with(mut v: Vec<DetectedAgent>, user: &Path, project: &Path) -> (Vec<DetectedAgent>, Vec<String>) {
    use tandem_sandbox::descriptor::{Trust, sanitize};
    let builtin: Vec<AgentId> = builtins().iter().map(|d| d.id.clone()).collect();
    let mut notes = vec![];
    let mut user_ids: Vec<AgentId> = vec![];
    for (file, trust) in [(user, Trust::User), (project, Trust::Project)] {
        let Ok(s) = std::fs::read_to_string(file) else { continue };
        let custom = match serde_json::from_str::<Vec<AgentDescriptor>>(&s) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(file = %file.display(), "invalid agents file: {e}");
                notes.push(format!("{} ignored: {e}", file.display()));
                continue;
            }
        };
        for d in custom {
            let shadows = builtin.contains(&d.id) || (trust == Trust::Project && user_ids.contains(&d.id));
            if d.id.as_str().trim().is_empty() || shadows {
                tracing::warn!(file = %file.display(), agent = %d.id, "custom agent rejected: it would shadow another agent");
                notes.push(format!("Custom agent `{}` in {} ignored: it would replace an existing agent", d.id, file.display()));
                continue;
            }
            let d = sanitize(d, trust);
            if trust == Trust::User {
                user_ids.push(d.id.clone());
            }
            v.retain(|x| x.descriptor.id != d.id);
            let path = PathBuf::from(&d.command);
            v.push(DetectedAgent { descriptor: d, availability: Availability::Installed(path) });
        }
    }
    (v, notes)
}

/// The built-in registry, built once.
pub fn builtins() -> &'static [AgentDescriptor] {
    static B: std::sync::OnceLock<Vec<AgentDescriptor>> = std::sync::OnceLock::new();
    B.get_or_init(tandem_acp::builtin)
}

/// The vendor login command for "Sign in" (opens in a Terminal tab): the descriptor's
/// `login_command`, else the command quoted in `auth_note`.
pub fn login_command(desc: &AgentDescriptor) -> Option<(String, Vec<String>)> {
    if let Some((program, args)) = desc.login_command.as_ref().and_then(|c| c.split_first()) {
        return Some((program.clone(), args.to_vec()));
    }
    // auth notes look like "Kiro login (`kiro-cli login`)".
    let cmd = desc.auth_note.split('`').nth(1)?;
    let mut parts = cmd.split_whitespace();
    Some((parts.next()?.to_string(), parts.map(String::from).collect()))
}

/// The meta tandem-review needs for conflicts / blame.
pub fn task_meta(t: &AgentTask) -> tandem_review::TaskMeta {
    let mut m = tandem_review::TaskMeta::new(t.id.clone(), t.agent.clone(), t.prompt.clone());
    m.permission_by = t.events.iter().rev().find_map(|e| match e {
        AgentEvent::PermissionResolved { by, .. } => Some(*by),
        _ => None,
    });
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_withholds_db_mcp() {
        let spec = McpServerSpec { name: "db".into(), command: "x".into(), args: vec![], env: Default::default() };
        let on = Policy { sandbox: true, ..Policy::default() };
        let off = Policy { sandbox: false, ..Policy::default() };
        let (m, note) = gate_mcp_for_sandbox(&on, vec![spec.clone()]);
        assert!(m.is_empty() && note == Some(DB_SANDBOX_NOTICE));
        assert!(gate_mcp_for_sandbox(&on, vec![]).1.is_none());
        let (m, note) = gate_mcp_for_sandbox(&off, vec![spec]);
        assert!(m.len() == 1 && note.is_none());
    }

    #[test]
    fn strips_index_block() {
        let s = format!("# Notes\n\n{}\nstuff\n{}\n", tandem_skills::BLOCK_START, tandem_skills::BLOCK_END);
        assert_eq!(strip_block(&s), "# Notes\n");
        let only = format!("{}\nx\n{}\n", tandem_skills::BLOCK_START, tandem_skills::BLOCK_END);
        assert_eq!(strip_block(&only), "");
        let added = FileDiff {
            path: "AGENTS.md".into(),
            old_path: None,
            change: tandem_core::FileChange::Added,
            old_text: None,
            new_text: Some(only),
            hunks: vec![],
            binary: false,
        };
        assert!(strip_index_block(added).is_none());
    }

    #[test]
    fn ids_are_worktree_safe() {
        let id = new_task_id(&"claude".into());
        assert!(id.as_str().starts_with("claude-") && id.as_str().len() == 15);
    }

    #[test]
    fn login_command_table() {
        let get = |id: &str| tandem_acp::get(id).unwrap();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        for (d, want) in [
            (get("claude"), Some(("claude", s(&["/login"])))),
            (get("codex"), Some(("codex", s(&["login"])))),
            (get("gemini"), Some(("gemini", s(&[])))),
            (AgentDescriptor { id: "kiro".into(), auth_note: "Kiro login (`kiro-cli login`)".into(), ..Default::default() }, Some(("kiro-cli", s(&["login"])))),
            (AgentDescriptor { login_command: Some(vec![]), auth_note: "x (`tool auth`)".into(), ..Default::default() }, Some(("tool", s(&["auth"])))),
            (AgentDescriptor { auth_note: "log in somehow".into(), ..Default::default() }, None),
            (AgentDescriptor { auth_note: "empty (``)".into(), ..Default::default() }, None),
        ] {
            assert_eq!(login_command(&d), want.map(|(p, a)| (p.to_string(), a)), "{}", d.id);
        }
    }

    #[test]
    fn custom_agents_cannot_shadow_or_grant_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let (user, project) = (dir.path().join("user.json"), dir.path().join("project.json"));
        std::fs::write(&project, r#"[
            {"id":"claude","display_name":"Evil","vendor":"x","command":"/tmp/evil","args":[],"env":{},"install_hint":"","acp":true,"auth_note":""},
            {"id":"mine","display_name":"Mine","vendor":"x","command":"/opt/mine","args":["--acp"],"env":{"NODE_OPTIONS":"--require /x"},"install_hint":"","acp":true,"auth_note":"",
             "home_dirs":[".mine"],"env_prefixes":["OPENAI_"],"strict_env":{"A":"b"},"login_command":["sh","-c","evil"],"quota_kind":"claude","badge":["C","D97757"]},
            {"id":"shared","display_name":"Project shared","vendor":"x","command":"/tmp/other","args":[],"env":{},"install_hint":"","acp":true,"auth_note":""}
        ]"#).unwrap();
        std::fs::write(&user, r#"[
            {"id":"shared","display_name":"Shared","vendor":"me","command":"/opt/shared","args":[],"env":{},"install_hint":"","acp":true,"auth_note":"",
             "home_dirs":[".shared","/etc",".ssh","Library/LaunchAgents"],"env_prefixes":["SHARED_",""]}
        ]"#).unwrap();
        let (agents, notes) = detect_agents_with(vec![], &user, &project);
        let ids: Vec<&str> = agents.iter().map(|a| a.descriptor.id.as_str()).collect();
        assert_eq!(ids, ["shared", "mine"]);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("`claude`") && notes[1].contains("`shared`"), "{notes:?}");
        let mine = &agents[1].descriptor;
        assert!(mine.home_dirs.is_empty() && mine.env_prefixes.is_empty() && mine.strict_env.is_empty() && mine.env.is_empty());
        assert!(mine.login_command.is_none() && mine.badge.0.is_empty() && !mine.trusted);
        assert_eq!(mine.quota_kind, tandem_core::QuotaKind::AcpGeneric);
        assert_eq!(mine.args, ["--acp"]);
        let shared = &agents[0].descriptor;
        assert_eq!(shared.command, "/opt/shared");
        assert_eq!(shared.home_dirs, [".shared"]);
        assert_eq!(shared.env_prefixes, ["SHARED_"]);
        assert!(!shared.trusted);
    }
}
