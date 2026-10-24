//! `.tandem/tasks/<id>/task.json`: the task's metadata, so the cockpit can restore its task
//! list (status + replay-log tail) on launch.

use super::task_dir;
use crate::model::agents::{AgentTask, AgentsModel, TaskStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tandem_acp::ReplayLog;
use tandem_core::{AgentId, TaskEvent, TaskId, TaskPhase, Usage};

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

pub fn save(root: &Path, t: &AgentTask) {
    let dir = task_dir(root, &t.id);
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string_pretty(&TaskRecord::of(t)) {
        let tmp = dir.join("task.json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, dir.join("task.json"));
        }
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
        t.error = rec.error;
        t.status = rec.status;
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
    use tandem_core::{AgentEvent, Role};

    #[test]
    fn save_and_restore_round_trip() {
        let root = std::env::temp_dir().join(format!("tandem-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut t = AgentTask::new("codex-1234abcd".into(), "codex".into(), "Fix it\nplease".into(), TaskPhase::Test);
        t.status = TaskStatus::Running;
        save(&root, &t);
        let mut log = ReplayLog::open(task_dir(&root, &t.id)).unwrap();
        log.record(&t.id, &AgentEvent::TextDelta { role: Role::User, text: "Fix it".into() }).unwrap();
        log.record(&t.id, &AgentEvent::TextDelta { role: Role::Assistant, text: "Done".into() }).unwrap();
        let back = restore(&root);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].title, "Fix it");
        assert_eq!(back[0].events.len(), 2);
        assert_eq!(back[0].status, TaskStatus::Failed, "a running task cannot survive a restart");
        assert!(back[0].restored);
        let _ = std::fs::remove_dir_all(&root);
    }
}
