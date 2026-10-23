//! Task worktrees: `<repo>/.tandem/worktrees/<task>` on branch `tandem/<task>`.

use crate::error::invalid;
use crate::{GitError, Repo, Result};
use git2::{BranchType, Repository, StatusOptions, WorktreeAddOptions, WorktreePruneOptions};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tandem_core::TaskId;

const EXCLUDE_ENTRY: &str = ".tandem/";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskWorktree {
    pub task: TaskId,
    pub path: PathBuf,
    /// Branch checked out in the worktree (`tandem/<task>`), if it can be determined.
    pub branch: Option<String>,
    /// HEAD commit of the worktree, if the directory still exists.
    pub head: Option<String>,
}

fn validate_task(task: &TaskId) -> Result<()> {
    let t = task.as_str();
    let ok = !t.is_empty()
        && t != "."
        && !t.contains("..")
        && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok { Ok(()) } else { invalid(format!("invalid task id for a worktree: {t:?}")) }
}

impl Repo {
    /// Directory holding all task worktrees.
    pub fn task_worktrees_dir(&self) -> PathBuf {
        self.main_root().join(".tandem").join("worktrees")
    }

    /// Create `<repo>/.tandem/worktrees/<task>` on a new branch `tandem/<task>` starting at
    /// `base` (any revspec; default HEAD). Adds `.tandem/` to `.git/info/exclude`.
    pub fn create_task_worktree(&self, task: &TaskId, base: Option<&str>) -> Result<PathBuf> {
        validate_task(task)?;
        let commit = self.resolve_commit(base)?;
        self.ensure_tandem_excluded()?;
        let dir = self.task_worktrees_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(task.as_str());
        let branch_name = format!("tandem/{task}");
        let mut branch = self.repo.branch(&branch_name, &commit, false)?;
        let mut opts = WorktreeAddOptions::new();
        opts.reference(Some(branch.get()));
        if let Err(e) = self.repo.worktree(task.as_str(), &path, Some(&opts)) {
            let _ = branch.delete();
            return Err(e.into());
        }
        tracing::debug!(task = %task, path = %path.display(), "created task worktree");
        Ok(path)
    }

    /// Remove a task worktree. Without `force`, refuses when it has uncommitted changes
    /// (including untracked files) and keeps the branch (it may hold agent commits).
    /// With `force`, locked/dirty worktrees are removed and the branch is deleted.
    pub fn remove_task_worktree(&self, task: &TaskId, force: bool) -> Result<()> {
        validate_task(task)?;
        let wt = self.repo.find_worktree(task.as_str())?;
        let path = wt.path().to_path_buf();
        if !force && path.exists() {
            let wr = Repository::open_from_worktree(&wt)?;
            let mut o = StatusOptions::new();
            o.include_untracked(true).recurse_untracked_dirs(true);
            if !wr.statuses(Some(&mut o))?.is_empty() {
                return invalid(format!("worktree for task {task} has uncommitted changes"));
            }
        }
        let mut po = WorktreePruneOptions::new();
        po.valid(true).working_tree(true).locked(force);
        wt.prune(Some(&mut po))?;
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
        if force
            && let Ok(mut b) = self.repo.find_branch(&format!("tandem/{task}"), BranchType::Local)
        {
            let _ = b.delete();
        }
        Ok(())
    }

    pub fn list_task_worktrees(&self) -> Result<Vec<TaskWorktree>> {
        let names = self.repo.worktrees()?;
        let mut out = Vec::new();
        for name in names.iter().flatten() {
            let Ok(wt) = self.repo.find_worktree(name) else { continue };
            let path = wt.path().to_path_buf();
            if !path.parent().is_some_and(|p| p.ends_with(Path::new(".tandem/worktrees"))) {
                continue;
            }
            let (mut branch, mut head) = (None, None);
            if let Ok(r) = Repository::open_from_worktree(&wt)
                && let Ok(h) = r.head()
            {
                branch = h.shorthand().map(str::to_string);
                head = h.target().map(|o| o.to_string());
            }
            out.push(TaskWorktree { task: TaskId::from(name), path, branch, head });
        }
        out.sort_by(|a, b| a.task.cmp(&b.task));
        Ok(out)
    }

    /// Path of a task's worktree if it exists.
    pub fn task_worktree_path(&self, task: &TaskId) -> Option<PathBuf> {
        let p = self.task_worktrees_dir().join(task.as_str());
        p.is_dir().then_some(p)
    }

    fn ensure_tandem_excluded(&self) -> Result<()> {
        let info = self.repo.commondir().join("info");
        std::fs::create_dir_all(&info)?;
        let file = info.join("exclude");
        let existing = match std::fs::read_to_string(&file) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(GitError::Io(e)),
        };
        if existing.lines().any(|l| matches!(l.trim(), ".tandem" | ".tandem/" | "/.tandem" | "/.tandem/")) {
            return Ok(());
        }
        let mut s = existing;
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(EXCLUDE_ENTRY);
        s.push('\n');
        std::fs::write(&file, s)?;
        Ok(())
    }
}
