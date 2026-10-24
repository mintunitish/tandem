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
    ui.send(TaskMsg::Prepared { worktree: worktree.clone(), base: base.clone(), notes }).await.ok();

    // 2. Replay log. The prompt reaches it (and the UI) through the event stream:
    //    `AgentHandle::prompt` emits it as `TextDelta { role: User }`.
    let log = ReplayLog::open(task_dir(&root, &id))?;

    // 3. Spawn the agent: cwd = worktree, sandbox + classifier from policy.
    let mut opts = SpawnOptions::new(&worktree, &root, policy);
    opts.mcp_servers = mcp;
    let (tx, rx) = tandem_core::unbounded::<TaskEvent>();
    let handle = Arc::new(AcpAgent::spawn(&desc, opts, tx, id.clone()).await.with_context(|| format!("start {}", desc.display_name))?);
    ui.send(TaskMsg::Spawned(handle.clone())).await.ok();

    // 4. Forward events: log, quota, UI; diff after every finished turn.
    let agent = desc.id.clone();
    tokio::spawn(forward(rx, ui, log, pool, agent, root, id, base));
    // 5. First turn. Events stream through the forwarder meanwhile; follow-up prompts reuse `handle`.
    let _ = handle.prompt(prompt).await;
    Ok(())
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

#[allow(clippy::too_many_arguments)]
async fn forward(
    rx: tandem_core::EventRx,
    ui: async_channel::Sender<TaskMsg>,
    log: ReplayLog,
    pool: Option<Arc<QuotaPool>>,
    agent: AgentId,
    root: PathBuf,
    id: TaskId,
    base: String,
) {
    // File writes run on the blocking pool; `log` travels there and back with each record.
    let mut log = Some(log);
    let write = async |log: &mut Option<ReplayLog>, id: &TaskId, event: Option<AgentEvent>| {
        let (Some(mut l), id) = (log.take(), id.clone()) else { return };
        let back = tokio::task::spawn_blocking(move || {
            let _ = match &event {
                Some(e) => l.record(&id, e),
                None => l.flush(),
            };
            l
        });
        *log = back.await.ok();
    };
    let mut dirty = false;
    loop {
        // Idle stream with buffered records: flush them so the log is never far behind.
        let next = if dirty { tokio::time::timeout(tandem_acp::log::FLUSH_INTERVAL, rx.recv()).await } else { Ok(rx.recv().await) };
        let event = match next {
            Ok(Ok(TaskEvent { event, .. })) => event,
            Ok(Err(_)) => break,
            Err(_) => {
                write(&mut log, &id, None).await;
                dirty = false;
                continue;
            }
        };
        write(&mut log, &id, Some(event.clone())).await;
        dirty = true;
        if let Some(p) = &pool {
            p.ingest(&agent, &event);
        }
        let done = matches!(event, AgentEvent::Done { .. });
        if ui.send(TaskMsg::Event(Box::new(event))).await.is_err() {
            // Nobody listens (task deleted); keep logging until the agent exits.
            continue;
        }
        if done {
            if let Some(p) = pool.clone() {
                tokio::spawn(async move { p.reconcile_all().await });
            }
            let (root, id, agent, base) = (root.clone(), id.clone(), agent.clone(), base.clone());
            match tokio::task::spawn_blocking(move || compute_diff(&root, &id, &agent, &base)).await {
                Ok(Ok(ds)) => {
                    let _ = ui.send(TaskMsg::Diff(Box::new(ds))).await;
                }
                Ok(Err(e)) => {
                    let _ = ui.send(TaskMsg::Event(Box::new(AgentEvent::Error { message: format!("diff failed: {e:#}") }))).await;
                }
                Err(_) => {}
            }
        }
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
/// declared in `<root>/.tandem/agents.json` (an array of `AgentDescriptor`).
pub fn detect_agents(root: &Path) -> Vec<DetectedAgent> {
    let mut v = tandem_acp::detect_installed();
    if let Ok(s) = std::fs::read_to_string(root.join(".tandem/agents.json"))
        && let Ok(custom) = serde_json::from_str::<Vec<AgentDescriptor>>(&s)
    {
        for d in custom {
            v.retain(|x| x.descriptor.id != d.id);
            let path = PathBuf::from(&d.command);
            v.push(DetectedAgent { descriptor: d, availability: Availability::Installed(path) });
        }
    }
    v
}

/// The vendor login command for "Sign in" (opens in a Terminal tab). Claude Code signs in with
/// its `/login` slash command; other agents use the descriptor's `login_command`, else the
/// command quoted in `auth_note`.
pub fn login_command(desc: &AgentDescriptor) -> Option<(String, Vec<String>)> {
    if desc.id.as_str() == "claude" {
        return Some(("claude".into(), vec!["/login".into()]));
    }
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
        let kiro = AgentDescriptor { id: "kiro".into(), auth_note: "Kiro login (`kiro-cli login`)".into(), ..Default::default() };
        assert!(login_command(&kiro).is_some_and(|(p, a)| p == "kiro-cli" && a == ["login"]));
    }
}
