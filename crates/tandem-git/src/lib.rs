//! tandem-git: git operations over libgit2. Fully synchronous; callers wrap in `spawn_blocking`.
//!
//! * [`Repo`]: status, branches, log, stage/unstage, commit, checkout.
//! * Task worktrees (`.tandem/worktrees/<task>` on branch `tandem/<task>`): see [`worktree`].
//! * Diffs into `tandem_core` types: see [`diff`]. Hunks are always computed with `similar`
//!   from the two texts so line numbering is consistent everywhere (diffs, gutter, apply).
//!   `DiffLine.text` keeps its trailing line terminator.
//! * Applying hunks / files onto a working tree: see [`apply`].
//! * Blame: see [`blame`].

pub mod apply;
pub mod blame;
pub mod diff;
mod error;
pub mod worktree;

pub use apply::{apply_file, apply_hunk_to_file, apply_hunks, copy_file_from_worktree};
pub use blame::BlameHunk;
pub use diff::{GutterMark, MAX_TEXT_BYTES, compute_hunks};
pub use error::{GitError, Result};
pub use worktree::TaskWorktree;

use git2::{BranchType, ObjectType, Repository, Status, StatusOptions};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Kind of change of a path in the index or in the working tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Unmodified,
    Added,
    Modified,
    Deleted,
    Renamed,
    TypeChange,
    /// Working tree only: file not tracked.
    Untracked,
    Conflicted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusEntry {
    /// Path relative to the repo root.
    pub path: PathBuf,
    /// Set for index renames.
    pub old_path: Option<PathBuf>,
    pub index: Change,
    pub worktree: Change,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
    pub commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    pub message: String,
    pub author: String,
    pub email: String,
    /// Author time, unix seconds.
    pub time: i64,
    pub parents: Vec<String>,
}

pub struct Repo {
    repo: Repository,
    workdir: PathBuf,
}

impl Repo {
    /// Open the repository containing `path` (discovers upwards from any subdirectory).
    /// Bare repositories are not supported.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let repo = Repository::discover(path)?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| GitError::Invalid("bare repositories are not supported".into()))?;
        let workdir = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
        Ok(Self { repo, workdir })
    }

    /// Working directory root of this repo (the linked worktree root if opened from one).
    pub fn root(&self) -> &Path {
        &self.workdir
    }

    /// Root of the main checkout (differs from [`root`](Self::root) inside a linked worktree).
    pub fn main_root(&self) -> PathBuf {
        let common = self.repo.commondir();
        let parent = common.parent().unwrap_or(common);
        std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf())
    }

    /// Escape hatch to the underlying libgit2 handle.
    pub fn git2(&self) -> &Repository {
        &self.repo
    }

    pub fn status(&self) -> Result<Vec<StatusEntry>> {
        let mut o = StatusOptions::new();
        o.include_untracked(true)
            .recurse_untracked_dirs(true)
            .renames_head_to_index(true)
            .include_ignored(false);
        let st = self.repo.statuses(Some(&mut o))?;
        let mut out = Vec::with_capacity(st.len());
        for e in st.iter() {
            let f = e.status();
            if f.is_ignored() {
                continue;
            }
            let (index, worktree) = if f.contains(Status::CONFLICTED) {
                (Change::Conflicted, Change::Conflicted)
            } else {
                (index_change(f), worktree_change(f))
            };
            let old_path = e
                .head_to_index()
                .filter(|_| f.contains(Status::INDEX_RENAMED))
                .and_then(|d| d.old_file().path().map(Path::to_path_buf));
            out.push(StatusEntry {
                path: PathBuf::from(String::from_utf8_lossy(e.path_bytes()).into_owned()),
                old_path,
                index,
                worktree,
            });
        }
        Ok(out)
    }

    pub fn branches(&self) -> Result<Vec<BranchInfo>> {
        let mut out = Vec::new();
        for b in self.repo.branches(None)? {
            let (b, ty) = b?;
            let Some(name) = b.name()?.map(str::to_string) else { continue };
            if name.ends_with("/HEAD") && ty == BranchType::Remote {
                continue;
            }
            let upstream = b.upstream().ok().and_then(|u| u.name().ok().flatten().map(str::to_string));
            out.push(BranchInfo {
                is_head: b.is_head(),
                is_remote: ty == BranchType::Remote,
                upstream,
                commit: b.get().target().map(|o| o.to_string()),
                name,
            });
        }
        out.sort_by(|a, b| (a.is_remote, &a.name).cmp(&(b.is_remote, &b.name)));
        Ok(out)
    }

    /// Short name of the current branch; `None` when HEAD is detached.
    /// Works on an unborn branch (fresh `git init`).
    pub fn current_branch(&self) -> Result<Option<String>> {
        let head = self.repo.find_reference("HEAD")?;
        Ok(head
            .symbolic_target()
            .map(|t| t.strip_prefix("refs/heads/").unwrap_or(t).to_string()))
    }

    /// HEAD commit, or `None` for an unborn branch.
    pub fn head_commit(&self) -> Result<Option<CommitInfo>> {
        match self.repo.head() {
            Ok(h) => Ok(Some(commit_info(&h.peel_to_commit()?))),
            Err(e) if is_unborn(&e) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Full hex id of HEAD, or `None` when unborn.
    pub fn head_id(&self) -> Result<Option<String>> {
        Ok(self.head_commit()?.map(|c| c.id))
    }

    /// Newest-first history from HEAD (empty on an unborn branch).
    pub fn log(&self, limit: usize) -> Result<Vec<CommitInfo>> {
        if self.head_commit()?.is_none() {
            return Ok(Vec::new());
        }
        let mut walk = self.repo.revwalk()?;
        walk.push_head()?;
        walk.set_sorting(git2::Sort::TIME)?;
        let mut out = Vec::new();
        for oid in walk.take(limit) {
            out.push(commit_info(&self.repo.find_commit(oid?)?));
        }
        Ok(out)
    }

    /// Stage paths (relative to the repo root). Missing files are staged as deletions.
    pub fn stage<P: AsRef<Path>>(&self, paths: &[P]) -> Result<()> {
        let mut index = self.repo.index()?;
        for p in paths {
            let p = self.rel(p.as_ref())?;
            let full = self.workdir.join(&p);
            if full.is_dir() {
                index.add_all([&p], git2::IndexAddOption::DEFAULT, None)?;
            } else if full.symlink_metadata().is_ok() {
                index.add_path(&p)?;
            } else {
                match index.remove_path(&p) {
                    Ok(()) => {}
                    Err(e) if e.code() == git2::ErrorCode::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        index.write()?;
        Ok(())
    }

    /// Reset paths in the index to HEAD (works on an unborn branch too).
    pub fn unstage<P: AsRef<Path>>(&self, paths: &[P]) -> Result<()> {
        let rels = paths
            .iter()
            .map(|p| self.rel(p.as_ref()))
            .collect::<Result<Vec<_>>>()?;
        match self.repo.head() {
            Ok(h) => {
                let obj = h.peel(ObjectType::Commit)?;
                self.repo.reset_default(Some(&obj), rels.iter())?;
            }
            Err(e) if is_unborn(&e) => {
                let mut index = self.repo.index()?;
                for p in &rels {
                    let _ = index.remove_path(p);
                }
                index.write()?;
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// Commit the index. Author/committer come from git config (`user.name`/`user.email`).
    /// Errors if nothing is staged. Returns the new commit id.
    pub fn commit(&self, message: &str) -> Result<String> {
        let sig = self.repo.signature().map_err(|e| {
            GitError::Invalid(format!("git user.name / user.email not configured: {}", e.message()))
        })?;
        let mut index = self.repo.index()?;
        let tree_id = index.write_tree()?;
        let tree = self.repo.find_tree(tree_id)?;
        let parent = match self.repo.head() {
            Ok(h) => Some(h.peel_to_commit()?),
            Err(e) if is_unborn(&e) => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(p) = &parent {
            if p.tree_id() == tree_id {
                return error::invalid("nothing to commit");
            }
        } else if tree.is_empty() {
            return error::invalid("nothing to commit");
        }
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let id = self.repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
        Ok(id.to_string())
    }

    /// Create a local branch at `base` (any revspec; default HEAD). Does not check it out.
    pub fn create_branch(&self, name: &str, base: Option<&str>) -> Result<()> {
        let commit = self.resolve_commit(base)?;
        self.repo.branch(name, &commit, false)?;
        Ok(())
    }

    /// Safe checkout of a local branch (fails rather than clobbering local changes).
    pub fn checkout_branch(&self, name: &str) -> Result<()> {
        let b = self.repo.find_branch(name, BranchType::Local)?;
        let refname = b
            .get()
            .name()
            .ok_or_else(|| GitError::Invalid("non-utf8 branch name".into()))?
            .to_string();
        let obj = b.get().peel(ObjectType::Commit)?;
        let mut co = git2::build::CheckoutBuilder::new();
        co.safe();
        self.repo.checkout_tree(&obj, Some(&mut co))?;
        self.repo.set_head(&refname)?;
        Ok(())
    }

    pub(crate) fn resolve_commit(&self, rev: Option<&str>) -> Result<git2::Commit<'_>> {
        match rev {
            Some(r) => Ok(self.repo.revparse_single(r)?.peel_to_commit()?),
            None => Ok(self.repo.head()?.peel_to_commit()?),
        }
    }

    /// Normalise to a repo-relative path; rejects escapes.
    pub(crate) fn rel(&self, p: &Path) -> Result<PathBuf> {
        let r = if p.is_absolute() {
            p.strip_prefix(&self.workdir)
                .map_err(|_| GitError::Invalid(format!("{} is outside the repository", p.display())))?
        } else {
            p
        };
        if r.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return error::invalid(format!("{} escapes the repository", p.display()));
        }
        Ok(r.to_path_buf())
    }
}

fn is_unborn(e: &git2::Error) -> bool {
    matches!(e.code(), git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound)
}

fn commit_info(c: &git2::Commit<'_>) -> CommitInfo {
    let a = c.author();
    let id = c.id().to_string();
    CommitInfo {
        short_id: id.chars().take(8).collect(),
        id,
        summary: c.summary().unwrap_or_default().to_string(),
        message: c.message().unwrap_or_default().to_string(),
        author: a.name().unwrap_or_default().to_string(),
        email: a.email().unwrap_or_default().to_string(),
        time: a.when().seconds(),
        parents: c.parent_ids().map(|p| p.to_string()).collect(),
    }
}

fn index_change(f: Status) -> Change {
    if f.contains(Status::INDEX_NEW) {
        Change::Added
    } else if f.contains(Status::INDEX_MODIFIED) {
        Change::Modified
    } else if f.contains(Status::INDEX_DELETED) {
        Change::Deleted
    } else if f.contains(Status::INDEX_RENAMED) {
        Change::Renamed
    } else if f.contains(Status::INDEX_TYPECHANGE) {
        Change::TypeChange
    } else {
        Change::Unmodified
    }
}

fn worktree_change(f: Status) -> Change {
    if f.contains(Status::WT_NEW) {
        Change::Untracked
    } else if f.contains(Status::WT_MODIFIED) {
        Change::Modified
    } else if f.contains(Status::WT_DELETED) {
        Change::Deleted
    } else if f.contains(Status::WT_RENAMED) {
        Change::Renamed
    } else if f.contains(Status::WT_TYPECHANGE) {
        Change::TypeChange
    } else {
        Change::Unmodified
    }
}
