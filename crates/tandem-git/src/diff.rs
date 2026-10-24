//! Diffs into `tandem_core` types.
//!
//! The *set of changed files* comes from libgit2; the *hunks* are computed with `similar` from
//! the old and new text (3 lines of context, git-style 1-based starts, `0` start for empty
//! ranges). Text is only filled for UTF-8 files without NUL bytes and <= [`MAX_TEXT_BYTES`];
//! binary files get `binary = true`, oversized ones get no text and no hunks.

use crate::{GitError, Repo, Result};
use git2::{Delta, DiffOptions, ObjectType, Repository, Tree};
use serde::{Deserialize, Serialize};
use similar::{DiffOp, TextDiff};
use std::path::{Path, PathBuf};
use tandem_core::{AgentId, DiffSet, FileChange, FileDiff, Hunk, TaskId};

/// Files larger than this (either side) get no text/hunks.
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const CONTEXT: usize = 3;

/// What happened to a gutter line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GutterKind {
    /// New line.
    Added,
    /// Changed line (part of a replaced block).
    Modified,
    /// Lines deleted just before this line.
    Removed,
}

/// Line-level change mark for the editor gutter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GutterMark {
    /// 1-based line in the current file.
    pub line: u32,
    pub kind: GutterKind,
}

enum Content {
    Absent,
    Text(String),
    Binary,
    TooLarge,
}

fn classify(bytes: &[u8]) -> Content {
    if bytes.len() > MAX_TEXT_BYTES {
        return Content::TooLarge;
    }
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return Content::Binary;
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => Content::Text(s.to_string()),
        Err(_) => Content::Binary,
    }
}

fn read_blob(repo: &Repository, id: git2::Oid) -> Result<Content> {
    if id.is_zero() {
        return Ok(Content::Absent);
    }
    let blob = repo.find_blob(id)?;
    if blob.size() > MAX_TEXT_BYTES {
        return Ok(Content::TooLarge);
    }
    Ok(classify(blob.content()))
}

fn read_workdir(root: &Path, rel: &Path) -> Result<Content> {
    let full = root.join(rel);
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.is_file() => {
            if m.len() > MAX_TEXT_BYTES as u64 {
                return Ok(Content::TooLarge);
            }
            Ok(classify(&std::fs::read(&full)?))
        }
        Ok(_) => Ok(Content::Absent), // symlink/dir/submodule: not diffed as text
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Content::Absent),
        Err(e) => Err(GitError::Io(e)),
    }
}

/// Git-style hunks (3 lines of context) between two texts. Line texts keep their terminator.
pub fn compute_hunks(old: &str, new: &str) -> Vec<Hunk> {
    tandem_core::hunks_from_texts(old, new, CONTEXT, false)
}

/// Diff `tree` (None = empty) against the working tree + index of `repo`, untracked included
/// (ignored files excluded).
fn diff_tree_vs_workdir(repo: &Repository, root: &Path, tree: Option<&Tree<'_>>) -> Result<Vec<FileDiff>> {
    let mut o = DiffOptions::new();
    o.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_typechange(true)
        .ignore_submodules(true);
    let diff = repo.diff_tree_to_workdir_with_index(tree, Some(&mut o))?;
    let mut files = Vec::new();
    for delta in diff.deltas() {
        let change = match delta.status() {
            Delta::Added | Delta::Untracked => FileChange::Added,
            Delta::Deleted => FileChange::Deleted,
            Delta::Modified | Delta::Typechange | Delta::Conflicted => FileChange::Modified,
            _ => continue,
        };
        let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) else { continue };
        let path = path.to_path_buf();
        if root.join(&path).is_dir() {
            continue;
        }
        let old = if change == FileChange::Added { Content::Absent } else { read_blob(repo, delta.old_file().id())? };
        let new = if change == FileChange::Deleted { Content::Absent } else { read_workdir(root, &path)? };
        files.push(build_file_diff(path, change, old, new));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn build_file_diff(path: PathBuf, change: FileChange, old: Content, new: Content) -> FileDiff {
    let binary = matches!(old, Content::Binary) || matches!(new, Content::Binary);
    let (old_text, new_text, hunks) = match (old, new) {
        (Content::Text(o), Content::Text(n)) => {
            let h = compute_hunks(&o, &n);
            (Some(o), Some(n), h)
        }
        (Content::Absent, Content::Text(n)) => {
            let h = compute_hunks("", &n);
            (None, Some(n), h)
        }
        (Content::Text(o), Content::Absent) => {
            let h = compute_hunks(&o, "");
            (Some(o), None, h)
        }
        _ => (None, None, Vec::new()),
    };
    FileDiff { path, old_path: None, change, old_text, new_text, hunks, binary }
}

impl Repo {
    /// Staged + unstaged + untracked changes of this checkout vs HEAD (vs empty on unborn HEAD).
    pub fn diff_working_tree(&self) -> Result<Vec<FileDiff>> {
        let tree = match self.repo.head() {
            Ok(h) => Some(h.peel_to_tree()?),
            Err(e) if matches!(e.code(), git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound) => None,
            Err(e) => return Err(e.into()),
        };
        diff_tree_vs_workdir(&self.repo, &self.workdir, tree.as_ref())
    }

    /// Everything the worktree at `worktree_path` changed relative to `base_commit`
    /// (committed + uncommitted + untracked, gitignore respected).
    ///
    /// `DiffSet.task` is taken from the worktree directory name and `agent` is `"unknown"`;
    /// use [`diff_task`](Self::diff_task) to fill both properly.
    pub fn diff_worktree_vs_base(&self, worktree_path: &Path, base_commit: &str) -> Result<DiffSet> {
        let task = worktree_path
            .file_name()
            .map(|n| TaskId::from(n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        self.diff_worktree_for(task, AgentId::from("unknown"), worktree_path, base_commit)
    }

    /// Like [`diff_worktree_vs_base`](Self::diff_worktree_vs_base) for a known task + agent,
    /// resolving the worktree path from the task id.
    pub fn diff_task(&self, task: &TaskId, agent: &AgentId, base_commit: &str) -> Result<DiffSet> {
        let path = self
            .task_worktree_path(task)
            .ok_or_else(|| GitError::Invalid(format!("no worktree for task {task}")))?;
        self.diff_worktree_for(task.clone(), agent.clone(), &path, base_commit)
    }

    fn diff_worktree_for(&self, task: TaskId, agent: AgentId, worktree_path: &Path, base: &str) -> Result<DiffSet> {
        let wt = Repository::open(worktree_path)?;
        let root = std::fs::canonicalize(worktree_path)?;
        let commit = wt.revparse_single(base)?.peel(ObjectType::Commit)?.into_commit().map_err(|_| {
            GitError::Invalid(format!("{base} is not a commit"))
        })?;
        let tree = commit.tree()?;
        let files = diff_tree_vs_workdir(&wt, &root, Some(&tree))?;
        Ok(DiffSet { task, agent, base_commit: commit.id().to_string(), files })
    }

    /// Changes introduced by a commit vs its first parent (vs empty tree for a root commit).
    pub fn diff_commit(&self, oid: &str) -> Result<Vec<FileDiff>> {
        let commit = self.repo.revparse_single(oid)?.peel_to_commit()?;
        let new_tree = commit.tree()?;
        let old_tree = if commit.parent_count() > 0 { Some(commit.parent(0)?.tree()?) } else { None };
        let mut o = DiffOptions::new();
        o.include_typechange(true).ignore_submodules(true);
        let diff = self.repo.diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), Some(&mut o))?;
        let mut files = Vec::new();
        for delta in diff.deltas() {
            let change = match delta.status() {
                Delta::Added => FileChange::Added,
                Delta::Deleted => FileChange::Deleted,
                Delta::Modified | Delta::Typechange => FileChange::Modified,
                _ => continue,
            };
            let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) else { continue };
            let old = read_blob(&self.repo, delta.old_file().id())?;
            let new = read_blob(&self.repo, delta.new_file().id())?;
            files.push(build_file_diff(path.to_path_buf(), change, old, new));
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }

    /// Gutter marks for `path` (working-tree content vs HEAD). Untracked files (and everything
    /// on an unborn branch) are all `Added`. Binary/oversized/missing files yield no marks.
    /// Sorted by line, at most one mark per line (`Added`/`Modified` win over `Removed`).
    pub fn per_line_gutter_marks(&self, path: impl AsRef<Path>) -> Result<Vec<GutterMark>> {
        let rel = self.rel(path.as_ref())?;
        let old = match self.repo.head() {
            Ok(h) => match h.peel_to_tree()?.get_path(&rel) {
                Ok(entry) => read_blob(&self.repo, entry.id())?,
                Err(e) if e.code() == git2::ErrorCode::NotFound => Content::Absent,
                Err(e) => return Err(e.into()),
            },
            Err(e) if matches!(e.code(), git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound) => Content::Absent,
            Err(e) => return Err(e.into()),
        };
        let new = read_workdir(&self.workdir, &rel)?;
        let (old, new) = match (old, new) {
            (Content::Text(o), Content::Text(n)) => (o, n),
            (Content::Absent, Content::Text(n)) => (String::new(), n),
            (Content::Text(o), Content::Absent) => (o, String::new()),
            _ => return Ok(Vec::new()),
        };
        Ok(gutter_marks(&old, &new))
    }
}

/// Gutter marks between two texts (see [`Repo::per_line_gutter_marks`]).
pub fn gutter_marks(old: &str, new: &str) -> Vec<GutterMark> {
    let diff = TextDiff::from_lines(old, new);
    let mut marks: Vec<GutterMark> = Vec::new();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { new_index, .. } => {
                marks.push(GutterMark { line: new_index as u32 + 1, kind: GutterKind::Removed });
            }
            DiffOp::Insert { new_index, new_len, .. } => {
                for l in new_index..new_index + new_len {
                    marks.push(GutterMark { line: l as u32 + 1, kind: GutterKind::Added });
                }
            }
            DiffOp::Replace { new_index, new_len, .. } => {
                for l in new_index..new_index + new_len {
                    marks.push(GutterMark { line: l as u32 + 1, kind: GutterKind::Modified });
                }
            }
        }
    }
    marks.sort_by_key(|m| (m.line, m.kind == GutterKind::Removed));
    marks.dedup_by_key(|m| m.line);
    marks
}
