//! Per-hunk review decisions over one task's [`DiffSet`].
//!
//! Files without hunks (pure renames, binary changes, empty adds/deletes) get one
//! virtual change at hunk index 0 that stands for the whole file.

use crate::hunks;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tandem_core::{DiffSet, FileChange, FileDiff, TaskId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision { #[default] Pending, Accepted, Rejected }

/// Address of one reviewable change: file index + hunk index within that file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HunkRef { pub file: usize, pub hunk: usize }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCounts { pub total: usize, pub pending: usize, pub accepted: usize, pub rejected: usize }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewState {
    diff: DiffSet,
    /// `decisions[file][hunk]`; each file has `max(hunks.len(), 1)` slots.
    decisions: Vec<Vec<Decision>>,
    #[serde(default)]
    cursor: Option<HunkRef>,
}

/// Where a task's review state is persisted: `<root>/.tandem/tasks/<id>/review.json`.
pub fn review_path(root: impl AsRef<Path>, task: &TaskId) -> PathBuf {
    root.as_ref().join(".tandem").join("tasks").join(task.as_str()).join("review.json")
}

fn slots(f: &FileDiff) -> usize { f.hunks.len().max(1) }

impl ReviewState {
    pub fn new(diff: DiffSet) -> Self {
        let decisions = diff.files.iter().map(|f| vec![Decision::Pending; slots(f)]).collect();
        let cursor = (!diff.files.is_empty()).then_some(HunkRef { file: 0, hunk: 0 });
        Self { diff, decisions, cursor }
    }

    pub fn diff(&self) -> &DiffSet { &self.diff }
    pub fn cursor(&self) -> Option<HunkRef> { self.cursor }

    /// Every change in navigation order.
    pub fn changes(&self) -> Vec<HunkRef> {
        self.decisions.iter().enumerate()
            .flat_map(|(file, d)| (0..d.len()).map(move |hunk| HunkRef { file, hunk }))
            .collect()
    }

    pub fn decision(&self, at: HunkRef) -> Option<Decision> {
        self.decisions.get(at.file)?.get(at.hunk).copied()
    }

    /// Set a decision; returns false when `at` does not exist.
    pub fn set(&mut self, at: HunkRef, d: Decision) -> bool {
        match self.decisions.get_mut(at.file).and_then(|f| f.get_mut(at.hunk)) {
            Some(slot) => { *slot = d; true }
            None => false,
        }
    }

    pub fn accept(&mut self, at: HunkRef) -> bool { self.set(at, Decision::Accepted) }
    pub fn reject(&mut self, at: HunkRef) -> bool { self.set(at, Decision::Rejected) }

    pub fn set_file(&mut self, file: usize, d: Decision) {
        if let Some(f) = self.decisions.get_mut(file) { f.fill(d) }
    }

    pub fn set_all(&mut self, d: Decision) {
        self.decisions.iter_mut().for_each(|f| f.fill(d));
    }

    pub fn counts(&self) -> ReviewCounts {
        let mut c = ReviewCounts::default();
        for d in self.decisions.iter().flatten() {
            c.total += 1;
            match d {
                Decision::Pending => c.pending += 1,
                Decision::Accepted => c.accepted += 1,
                Decision::Rejected => c.rejected += 1,
            }
        }
        c
    }

    pub fn is_complete(&self) -> bool { self.counts().pending == 0 }

    fn step(&mut self, forward: bool, only_pending: bool) -> Option<HunkRef> {
        let all = self.changes();
        if all.is_empty() { return None }
        let cur = self.cursor.and_then(|c| all.iter().position(|x| *x == c));
        let n = all.len();
        for k in 1..=n {
            let i = match (cur, forward) {
                (Some(c), true) => (c + k) % n,
                (Some(c), false) => (c + n - k % n) % n,
                (None, true) => k - 1,
                (None, false) => n - k,
            };
            if !only_pending || self.decision(all[i]) == Some(Decision::Pending) {
                self.cursor = Some(all[i]);
                return self.cursor;
            }
        }
        None
    }

    /// Next change across files (wraps around).
    pub fn next_change(&mut self) -> Option<HunkRef> { self.step(true, false) }
    /// Previous change across files (wraps around).
    pub fn prev_change(&mut self) -> Option<HunkRef> { self.step(false, false) }
    /// Next change still pending (wraps); `None` when everything is decided.
    pub fn next_pending(&mut self) -> Option<HunkRef> { self.step(true, true) }

    /// Files with only accepted hunks applied. `new_text` is materialized from
    /// `old_text` + accepted hunks; `hunks` keeps only the accepted ones.
    /// Files with no accepted change are omitted.
    pub fn accepted_files(&self) -> Vec<FileDiff> {
        let mut out = Vec::new();
        for (fi, f) in self.diff.files.iter().enumerate() {
            let dec = &self.decisions[fi];
            let accepted = |i: usize| dec.get(i) == Some(&Decision::Accepted);
            if !dec.contains(&Decision::Accepted) { continue }
            let all = dec.iter().all(|d| *d == Decision::Accepted);
            if f.hunks.is_empty() || f.binary {
                // Whole-file change: take it verbatim.
                out.push(f.clone());
                continue;
            }
            let mut g = f.clone();
            g.hunks = f.hunks.iter().enumerate().filter(|(i, _)| accepted(*i)).map(|(_, h)| h.clone()).collect();
            match (f.change, all) {
                (FileChange::Deleted, true) => g.new_text = None,
                (FileChange::Deleted, false) => {
                    g.change = FileChange::Modified;
                    g.new_text = Some(hunks::materialize(f, accepted));
                }
                _ => g.new_text = Some(hunks::materialize(f, accepted)),
            }
            out.push(g);
        }
        out
    }

    pub fn to_json(&self) -> Result<String> { Ok(serde_json::to_string_pretty(self)?) }

    pub fn from_json(s: &str) -> Result<Self> {
        let st: Self = serde_json::from_str(s)?;
        if st.decisions.len() != st.diff.files.len()
            || st.decisions.iter().zip(&st.diff.files).any(|(d, f)| d.len() != slots(f))
        {
            anyhow::bail!("review state does not match its diff set");
        }
        Ok(st)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)? }
        std::fs::write(path, self.to_json()?).with_context(|| format!("writing {}", path.display()))
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Self::from_json(&std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?)
    }
}
