//! Apply reviewed changes (from a task worktree `FileDiff`) onto a target checkout.

use crate::{GitError, Result};
use std::path::{Component, Path, PathBuf};
use tandem_core::{FileChange, FileDiff, Hunk, LineKind};

fn conflict<T>(path: &Path, reason: impl Into<String>) -> Result<T> {
    Err(GitError::Conflict { path: path.to_path_buf(), reason: reason.into() })
}

fn target_path(root: &Path, fd: &FileDiff) -> Result<PathBuf> {
    let ok = !fd.path.as_os_str().is_empty()
        && fd.path.components().all(|c| matches!(c, Component::Normal(_)));
    if !ok {
        return Err(GitError::Invalid(format!("unsafe path in diff: {}", fd.path.display())));
    }
    Ok(root.join(&fd.path))
}

fn read_opt(p: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(p) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            Err(GitError::Invalid(format!("{} is not valid UTF-8", p.display())))
        }
        Err(e) => Err(e.into()),
    }
}

fn write(p: &Path, s: &str) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, s)?;
    Ok(())
}

/// Apply one hunk of `fd` (by index) to `repo_root/fd.path`. See [`apply_hunks`].
pub fn apply_hunk_to_file(repo_root: &Path, fd: &FileDiff, hunk_index: usize) -> Result<()> {
    apply_hunks(repo_root, fd, &[hunk_index])
}

/// Apply the selected hunks of `fd` to `repo_root/fd.path`.
///
/// Each hunk's old side (context + removed lines) must match the target exactly; it is searched
/// outward from the hunk's nominal position so earlier edits that shifted lines are tolerated.
/// On any mismatch nothing is written and `GitError::Conflict` is returned. A `Deleted` file
/// whose content becomes empty is removed.
pub fn apply_hunks(repo_root: &Path, fd: &FileDiff, hunk_indexes: &[usize]) -> Result<()> {
    let target = target_path(repo_root, fd)?;
    if fd.binary {
        return Err(GitError::Invalid("binary files cannot be applied per hunk; use copy_file_from_worktree".into()));
    }
    let mut idx = hunk_indexes.to_vec();
    idx.sort_unstable();
    idx.dedup();
    if let Some(&bad) = idx.iter().find(|&&i| i >= fd.hunks.len()) {
        return Err(GitError::Invalid(format!("hunk index {bad} out of range ({} hunks)", fd.hunks.len())));
    }
    let current = match read_opt(&target)? {
        Some(s) => s,
        None if fd.change == FileChange::Added || fd.hunks.iter().all(|h| h.old_lines == 0) => String::new(),
        None => return conflict(&fd.path, "file does not exist"),
    };
    let mut lines: Vec<&str> = current.split_inclusive('\n').collect();
    // Bottom-up so a hunk's position is unaffected by hunks applied below it.
    for &i in idx.iter().rev() {
        apply_one(&mut lines, &fd.hunks[i], &fd.path, i)?;
    }
    let out: String = lines.concat();
    if fd.change == FileChange::Deleted && out.is_empty() {
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        return Ok(());
    }
    write(&target, &out)
}

fn apply_one<'a>(lines: &mut Vec<&'a str>, hunk: &'a Hunk, path: &Path, i: usize) -> Result<()> {
    let old: Vec<&str> = hunk
        .lines
        .iter()
        .filter(|l| l.kind != LineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    let new: Vec<&'a str> = hunk
        .lines
        .iter()
        .filter(|l| l.kind != LineKind::Removed)
        .map(|l| l.text.as_str())
        .collect();
    let expected = if hunk.old_lines == 0 { hunk.old_start } else { hunk.old_start.saturating_sub(1) } as usize;
    let pos = if old.is_empty() {
        expected.min(lines.len())
    } else {
        let Some(max_pos) = lines.len().checked_sub(old.len()) else {
            return conflict(path, format!("hunk {i}: target is shorter than the hunk"));
        };
        let matches_at = |p: usize| lines[p..p + old.len()] == old[..];
        let expected = expected.min(max_pos);
        let found = (0..=max_pos).find_map(|d| {
            let lo = expected.checked_sub(d).filter(|&p| matches_at(p));
            let hi = Some(expected + d).filter(|&p| p <= max_pos && matches_at(p));
            lo.or(hi)
        });
        match found {
            Some(p) => p,
            None => return conflict(path, format!("hunk {i}: context does not match the current file")),
        }
    };
    lines.splice(pos..pos + old.len(), new);
    Ok(())
}

/// Apply a whole-file change. Strict: the target must still equal `old_text` (or not exist for
/// `Added`); otherwise `GitError::Conflict`. Binary/oversized files (no text) are rejected;
/// use [`copy_file_from_worktree`] for those.
pub fn apply_file(repo_root: &Path, fd: &FileDiff) -> Result<()> {
    let target = target_path(repo_root, fd)?;
    if fd.binary || (fd.change != FileChange::Deleted && fd.new_text.is_none()) {
        return Err(GitError::Invalid("no text available; use copy_file_from_worktree".into()));
    }
    let current = read_opt(&target)?;
    match fd.change {
        FileChange::Added => {
            let new = fd.new_text.as_deref().unwrap_or_default();
            match current {
                None => write(&target, new),
                Some(c) if c == new => Ok(()),
                Some(_) => conflict(&fd.path, "file already exists with different content"),
            }
        }
        FileChange::Deleted => match current {
            None => Ok(()),
            Some(c) if Some(c.as_str()) == fd.old_text.as_deref() => {
                std::fs::remove_file(&target)?;
                Ok(())
            }
            Some(_) => conflict(&fd.path, "file changed since the base"),
        },
        _ => {
            let new = fd.new_text.as_deref().unwrap_or_default();
            match current {
                Some(c) if Some(c.as_str()) == fd.old_text.as_deref() => write(&target, new),
                Some(c) if c == new => Ok(()),
                Some(_) => conflict(&fd.path, "file changed since the base"),
                None => conflict(&fd.path, "file does not exist"),
            }
        }
    }
}

/// Overwrite (or delete) `repo_root/fd.path` with the worktree's version byte-for-byte.
/// Works for binary and oversized files; does not check the target's current content.
pub fn copy_file_from_worktree(worktree_root: &Path, repo_root: &Path, fd: &FileDiff) -> Result<()> {
    let dst = target_path(repo_root, fd)?;
    let src = target_path(worktree_root, fd)?;
    if fd.change == FileChange::Deleted {
        match std::fs::remove_file(&dst) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&src, &dst)?;
    Ok(())
}
