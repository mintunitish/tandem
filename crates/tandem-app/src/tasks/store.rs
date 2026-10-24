//! `.tandem/tasks/<id>/task.json`: the task's metadata, so the cockpit can restore its task
//! list (status + replay-log tail) on launch.

use super::task_dir;
use crate::model::agents::{AgentTask, AgentsModel, TaskStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tandem_acp::ReplayLog;
use tandem_core::{AgentEvent, AgentId, TaskEvent, TaskId, TaskPhase, Usage};

/// Events kept when restoring a task (the log itself stays complete on disk).
pub const RESTORE_TAIL: usize = 2000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: TaskId,
    pub agent: AgentId,
    pub prompt: String,
    pub phase: TaskPhase,
    #[serde(default)]
    pub presets: Vec<String>,
    pub status: TaskStatus,
    pub worktree: Option<PathBuf>,
    pub base_commit: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub route_reason: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl TaskRecord {
    pub fn of(t: &AgentTask) -> Self {
        Self {
            id: t.id.clone(),
            agent: t.agent.clone(),
            prompt: t.prompt.clone(),
            phase: t.phase,
            presets: t.presets.clone(),
            status: t.status,
            worktree: t.worktree.clone(),
            base_commit: t.base_commit.clone(),
            started_at: t.started_at,
            finished_at: t.finished_at,
            usage: t.usage.clone(),
            route_reason: t.route_reason.clone(),
            error: t.error.clone(),
        }
    }
}

/// Serializes `t` here (UI thread) and writes `task.json` on the background writer, without
/// fsync: it is rewritten on every status change and rebuilt from the replay log anyway.
pub fn save(root: &Path, t: &AgentTask) {
    if let Ok(json) = serde_json::to_vec_pretty(&TaskRecord::of(t)) {
        crate::state::persist(task_dir(root, &t.id).join("task.json"), json, false);
    }
}

/// Every task under `<root>/.tandem/tasks`, oldest first. Tasks that were still running when
/// Tandem quit are marked failed: their agent process is gone.
pub fn restore(root: &Path) -> Vec<AgentTask> {
    let Ok(dirs) = std::fs::read_dir(root.join(".tandem/tasks")) else { return vec![] };
    let mut out = Vec::new();
    for d in dirs.flatten() {
        let Ok(s) = std::fs::read_to_string(d.path().join("task.json")) else { continue };
        let Ok(rec) = serde_json::from_str::<TaskRecord>(&s) else { continue };
        let mut model = AgentsModel::default();
        let mut t = AgentTask::new(rec.id.clone(), rec.agent.clone(), rec.prompt.clone(), rec.phase);
        t.started_at = rec.started_at;
        model.tasks.push(t);
        let records = ReplayLog::read(d.path()).unwrap_or_default();
        let skip = records.len().saturating_sub(RESTORE_TAIL);
        // The log is written as events happen; task.json only on status changes, so it can
        // be stale. A log that ends a turn (`Done` / `Error`) decides the status.
        let ended = records
            .iter()
            .rev()
            .map(|r| &r.event)
            .find(|e| !matches!(e, AgentEvent::Raw { .. } | AgentEvent::Usage(_)))
            .is_some_and(|e| matches!(e, AgentEvent::Done { .. } | AgentEvent::Error { .. }));
        for r in records.into_iter().skip(skip) {
            model.apply(TaskEvent { task: rec.id.clone(), event: r.event });
        }
        let mut t = model.tasks.pop().expect("pushed above");
        t.presets = rec.presets;
        t.worktree = rec.worktree.filter(|w| w.is_dir());
        t.base_commit = rec.base_commit;
        t.finished_at = rec.finished_at.or(t.finished_at);
        t.usage = rec.usage;
        t.route_reason = rec.route_reason;
        if !ended {
            t.status = rec.status;
            t.error = rec.error;
        } else if t.error.is_none() {
            t.error = rec.error;
        }
        t.restored = true;
        if t.status.is_active() {
            t.status = TaskStatus::Failed;
            t.error = Some("Tandem was closed while this task ran; the agent process ended.".into());
            t.finished_at.get_or_insert(t.started_at);
        }
        out.push(t);
    }
    out.sort_by_key(|t| t.started_at);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tandem_core::Role;

    #[test]
    fn save_and_restore_round_trip() {
        let root = std::env::temp_dir().join(format!("tandem-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut t = AgentTask::new("codex-1234abcd".into(), "codex".into(), "Fix it\nplease".into(), TaskPhase::Test);
        t.status = TaskStatus::Running;
        save(&root, &t);
        crate::state::flush_writes();
        let mut log = ReplayLog::open(task_dir(&root, &t.id)).unwrap();
        log.record(&t.id, &AgentEvent::TextDelta { role: Role::User, text: "Fix it".into() }).unwrap();
        log.record(&t.id, &AgentEvent::TextDelta { role: Role::Assistant, text: "Done".into() }).unwrap();
        drop(log); // flushes the buffered records
        let back = restore(&root);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].title, "Fix it");
        assert_eq!(back[0].events.len(), 2);
        assert_eq!(back[0].status, TaskStatus::Failed, "a running task cannot survive a restart");
        assert!(back[0].restored);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn terminal_log_event_beats_stale_task_json() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let mk = |id: &str| {
            let mut t = AgentTask::new(id.into(), "codex".into(), "x".into(), TaskPhase::Test);
            t.status = TaskStatus::Running;
            save(root, &t);
            t
        };
        let (done, failed, open) = (mk("codex-00000001"), mk("codex-00000002"), mk("codex-00000003"));
        crate::state::flush_writes();
        let log = |t: &AgentTask, events: &[AgentEvent]| {
            let mut l = ReplayLog::open(task_dir(root, &t.id)).unwrap();
            for e in events {
                l.record(&t.id, e).unwrap();
            }
        };
        log(&done, &[AgentEvent::TextDelta { role: Role::Assistant, text: "ok".into() }, AgentEvent::Done { reason: tandem_core::StopReason::EndTurn }, AgentEvent::Usage(Usage::default())]);
        log(&failed, &[AgentEvent::Error { message: "boom".into() }]);
        log(&open, &[AgentEvent::TextDelta { role: Role::Assistant, text: "still going".into() }]);
        let back = restore(root);
        let status = |id: &TaskId| back.iter().find(|t| &t.id == id).unwrap().status;
        assert_eq!(status(&done.id), TaskStatus::Done, "log says the turn ended");
        assert_eq!(status(&failed.id), TaskStatus::Failed);
        assert_eq!(back.iter().find(|t| t.id == failed.id).unwrap().error.as_deref(), Some("boom"));
        assert_eq!(status(&open.id), TaskStatus::Failed, "no terminal event: task.json (running) applies, then marked failed");
    }
}
