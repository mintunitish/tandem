//! Agent blame: which task/agent/prompt produced each line.
//!
//! Entries cover runs of *added* lines in post-change coordinates. When a later
//! DiffSet is applied, existing entries are mapped through it: lines below an
//! insertion/deletion shift, lines the new diff removes drop out (splitting an
//! entry if needed), and the new diff's added lines get fresh entries.

use crate::TaskMeta;
use crate::hunks::{self, base_path};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tandem_core::{BlameEntry, DiffSet, FileChange, LineKind};

/// `<root>/.tandem/blame.json`
pub fn blame_path(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref().join(".tandem").join("blame.json")
}

fn entry(path: &Path, start: u32, len: u32, meta: &TaskMeta) -> BlameEntry {
    BlameEntry {
        path: path.to_path_buf(),
        new_start: start,
        new_lines: len,
        task: meta.task.clone(),
        agent: meta.agent.clone(),
        prompt_excerpt: meta.prompt_excerpt(),
        permission_by: meta.permission_by,
    }
}

/// Blame entries for every run of added lines in `diff`.
pub fn blame_entries(diff: &DiffSet, meta: &TaskMeta) -> Vec<BlameEntry> {
    let mut out = Vec::new();
    for f in diff.files.iter().filter(|f| f.change != FileChange::Deleted && !f.binary) {
        let mut run: Option<(u32, u32)> = None;
        let mut flush = |run: &mut Option<(u32, u32)>| {
            if let Some((s, n)) = run.take() { out.push(entry(&f.path, s, n, meta)) }
        };
        for h in &f.hunks {
            for p in hunks::walk(h).0 {
                if p.line.kind == LineKind::Added {
                    let line = p.new as u32 + 1;
                    match &mut run {
                        Some((s, n)) if *s + *n == line => *n += 1,
                        _ => { flush(&mut run); run = Some((line, 1)) }
                    }
                } else {
                    flush(&mut run);
                }
            }
            flush(&mut run);
        }
    }
    out
}

/// Map `existing` (coordinates of the tree `applied` was diffed against) through
/// `applied`, then add entries for `applied`'s own added lines.
pub fn merge_blame(existing: Vec<BlameEntry>, applied: &DiffSet, meta: &TaskMeta) -> Vec<BlameEntry> {
    let by_base: HashMap<&Path, &tandem_core::FileDiff> = applied.files.iter().map(|f| (base_path(f), f)).collect();
    let mut out = Vec::with_capacity(existing.len());
    for e in existing {
        let Some(f) = by_base.get(e.path.as_path()) else {
            // File untouched (unless something was *added* at this path, which resets it).
            if !applied.files.iter().any(|f| f.change == FileChange::Added && f.path == e.path) { out.push(e) }
            continue;
        };
        if f.change == FileChange::Deleted || f.change == FileChange::Added { continue }
        // Map each line, regroup into contiguous runs.
        let mut run: Option<(u32, u32)> = None;
        let mut emit = |run: &mut Option<(u32, u32)>| {
            if let Some((s, n)) = run.take() {
                out.push(BlameEntry { path: f.path.clone(), new_start: s, new_lines: n, ..e.clone() });
            }
        };
        for line in e.new_start..e.new_start + e.new_lines {
            match hunks::map_old_line(f, line) {
                Some(m) => match &mut run {
                    Some((s, n)) if *s + *n == m => *n += 1,
                    _ => { emit(&mut run); run = Some((m, 1)) }
                },
                None => emit(&mut run),
            }
        }
        emit(&mut run);
    }
    out.extend(blame_entries(applied, meta));
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.new_start.cmp(&b.new_start)));
    out
}

/// Sorted blame entries with per-line lookup. Serialized as a plain JSON array.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlameIndex { entries: Vec<BlameEntry> }

impl BlameIndex {
    pub fn new(mut entries: Vec<BlameEntry>) -> Self {
        entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.new_start.cmp(&b.new_start)));
        Self { entries }
    }

    pub fn entries(&self) -> &[BlameEntry] { &self.entries }

    /// Record an applied DiffSet (e.g. after accepting a task's hunks).
    pub fn apply(&mut self, applied: &DiffSet, meta: &TaskMeta) {
        self.entries = merge_blame(std::mem::take(&mut self.entries), applied, meta);
    }

    /// Entry covering 1-based `line` of `path`.
    pub fn lookup(&self, path: impl AsRef<Path>, line: u32) -> Option<&BlameEntry> {
        let path = path.as_ref();
        let lo = self.entries.partition_point(|e| e.path.as_path() < path);
        let hi = self.entries.partition_point(|e| e.path.as_path() <= path);
        let slice = &self.entries[lo..hi];
        let i = slice.partition_point(|e| e.new_start <= line);
        // Entries don't overlap after merge, but scan back defensively.
        slice[..i].iter().rev().find(|e| line < e.new_start + e.new_lines)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)? }
        std::fs::write(path, serde_json::to_string_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(Self::new(serde_json::from_str(&s)?))
    }

    /// Like [`load`](Self::load) but returns an empty index when the file is missing.
    pub fn load_or_default(path: impl AsRef<Path>) -> Result<Self> {
        if path.as_ref().exists() { Self::load(path) } else { Ok(Self::default()) }
    }
}
