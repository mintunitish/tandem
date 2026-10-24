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
    let target = root.join(&fd.path);
    check_no_escape(root, &fd.path)?;
    Ok(target)
}

/// Refuses `root/rel` when an existing component of it (the target included) is a symlink
/// that resolves outside `root`, or a dangling one: writing through it would land outside
/// the checkout. Lexical checks alone do not catch this.
fn check_no_escape(root: &Path, rel: &Path) -> Result<()> {
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut p = root.to_path_buf();
    for c in rel.components() {
        p.push(c);
        let Ok(meta) = std::fs::symlink_metadata(&p) else { break };
        if meta.file_type().is_symlink() {
            match p.canonicalize() {
                Ok(real) if real.starts_with(&canon_root) => {}
                _ => return Err(GitError::Invalid(format!("{} is a symlink leading outside {}", rel.display(), root.display()))),
            }
        }
    }
    Ok(())
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
    if fd.change != FileChange::Deleted && std::fs::symlink_metadata(&src).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(GitError::Invalid(format!("{} is a symlink in the task worktree; refusing to copy it", fd.path.display())));
    }
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn added(path: &str, text: &str) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            change: FileChange::Added,
            old_text: None,
            new_text: Some(text.into()),
            hunks: tandem_core::hunks_from_texts("", text, 3, false),
            binary: false,
        }
    }

    #[test]
    fn symlinked_dir_to_outside_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, outside) = (tmp.path().join("root"), tmp.path().join("outside"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("link")).unwrap();
        let fd = added("link/evil.txt", "x\n");
        assert!(matches!(apply_file(&root, &fd), Err(GitError::Invalid(_))));
        assert!(matches!(apply_hunks(&root, &fd, &[0]), Err(GitError::Invalid(_))));
        assert!(!outside.join("evil.txt").exists());
        // A symlinked file pointing outside, and a dangling one, are refused too.
        symlink(outside.join("target.txt"), root.join("dangling")).unwrap();
        assert!(apply_file(&root, &added("dangling", "x\n")).is_err());
        assert!(!outside.join("target.txt").exists());
        // A symlink that stays inside the root is fine.
        std::fs::create_dir_all(root.join("real")).unwrap();
        symlink(root.join("real"), root.join("inner")).unwrap();
        apply_file(&root, &added("inner/ok.txt", "ok\n")).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("real/ok.txt")).unwrap(), "ok\n");
    }

    #[test]
    fn normal_file_applies_and_symlink_source_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, wt) = (tmp.path().join("root"), tmp.path().join("wt"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        apply_file(&root, &added("dir/a.txt", "a\n")).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("dir/a.txt")).unwrap(), "a\n");
        std::fs::write(wt.join("b.bin"), [0u8, 1, 2]).unwrap();
        copy_file_from_worktree(&wt, &root, &added("b.bin", "")).unwrap();
        assert_eq!(std::fs::read(root.join("b.bin")).unwrap(), [0u8, 1, 2]);
        // The agent planted a symlink to a secret in its worktree.
        let secret = tmp.path().join("secret");
        std::fs::write(&secret, "s3cret").unwrap();
        symlink(&secret, wt.join("leak.txt")).unwrap();
        assert!(copy_file_from_worktree(&wt, &root, &added("leak.txt", "")).is_err());
        assert!(!root.join("leak.txt").exists());
    }
}
