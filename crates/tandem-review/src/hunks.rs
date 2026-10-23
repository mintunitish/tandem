//! Low-level hunk arithmetic shared by every module: building hunks from texts,
//! walking hunk lines with resolved positions, materializing a subset of hunks
//! and mapping base lines through a diff.
//!
//! Conventions: `Hunk::old_start`/`new_start` follow unified-diff headers
//! (1-based; when the side is empty the start is the line *after which* the
//! change sits). Internally everything is converted to 0-based indices.

use similar::{ChangeTag, TextDiff};
use std::path::{Path, PathBuf};
use tandem_core::{DiffLine, FileChange, FileDiff, Hunk, LineKind};

/// Number of context lines used by [`diff_texts`].
pub const CONTEXT: usize = 3;

/// Build a [`FileDiff`] from two texts (`None` = file absent). Mainly for tests,
/// fixtures and demos; `tandem-git` is the production producer.
pub fn diff_texts(path: impl Into<PathBuf>, old: Option<&str>, new: Option<&str>) -> FileDiff {
    let change = match (old, new) {
        (None, Some(_)) => FileChange::Added,
        (Some(_), None) => FileChange::Deleted,
        _ => FileChange::Modified,
    };
    let diff = TextDiff::from_lines(old.unwrap_or(""), new.unwrap_or(""));
    let mut hunks = Vec::new();
    for group in diff.grouped_ops(CONTEXT) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else { continue };
        let (os, oe) = (first.old_range().start, last.old_range().end);
        let (ns, ne) = (first.new_range().start, last.new_range().end);
        let mut lines = Vec::new();
        for op in &group {
            for c in diff.iter_changes(op) {
                let kind = match c.tag() {
                    ChangeTag::Equal => LineKind::Context,
                    ChangeTag::Delete => LineKind::Removed,
                    ChangeTag::Insert => LineKind::Added,
                };
                lines.push(DiffLine {
                    kind,
                    old_no: c.old_index().map(|i| i as u32 + 1),
                    new_no: c.new_index().map(|i| i as u32 + 1),
                    text: strip_eol(c.value()).to_string(),
                });
            }
        }
        hunks.push(Hunk {
            old_start: header_start(os, oe - os),
            old_lines: (oe - os) as u32,
            new_start: header_start(ns, ne - ns),
            new_lines: (ne - ns) as u32,
            lines,
        });
    }
    FileDiff {
        path: path.into(),
        old_path: None,
        change,
        old_text: old.map(str::to_string),
        new_text: new.map(str::to_string),
        hunks,
        binary: false,
    }
}

fn header_start(start0: usize, len: usize) -> u32 {
    if len == 0 { start0 as u32 } else { start0 as u32 + 1 }
}

pub(crate) fn strip_eol(s: &str) -> &str {
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.strip_suffix('\r').unwrap_or(s)
}

/// The path this file had in the base commit (rename source, else `path`).
pub fn base_path(f: &FileDiff) -> &Path {
    match (f.change, &f.old_path) {
        (FileChange::Renamed, Some(p)) => p,
        _ => &f.path,
    }
}

/// 0-based index of the first base line the hunk covers (or the insertion point).
pub fn old_index(h: &Hunk) -> usize {
    if let Some(n) = h.lines.iter().find_map(|l| l.old_no) {
        return n.saturating_sub(1) as usize;
    }
    if h.old_lines == 0 { h.old_start as usize } else { h.old_start.saturating_sub(1) as usize }
}

/// 0-based index of the first new line the hunk covers (or the insertion point).
pub fn new_index(h: &Hunk) -> usize {
    if let Some(n) = h.lines.iter().find_map(|l| l.new_no) {
        return n.saturating_sub(1) as usize;
    }
    if h.new_lines == 0 { h.new_start as usize } else { h.new_start.saturating_sub(1) as usize }
}

/// A hunk line with resolved 0-based positions: for `Context`/`Removed` `old` is the
/// line's own index; for `Added` it is the insertion point. Symmetric for `new`.
#[derive(Clone, Copy, Debug)]
pub struct Pos<'a> {
    pub line: &'a DiffLine,
    pub old: usize,
    pub new: usize,
}

/// Walk hunk lines tracking positions; returns the positions plus the indices
/// just past the hunk on each side.
pub fn walk(h: &Hunk) -> (Vec<Pos<'_>>, usize, usize) {
    let (mut o, mut n) = (old_index(h), new_index(h));
    let mut out = Vec::with_capacity(h.lines.len());
    for line in &h.lines {
        out.push(Pos { line, old: o, new: n });
        match line.kind {
            LineKind::Context => { o += 1; n += 1; }
            LineKind::Removed => o += 1,
            LineKind::Added => n += 1,
        }
    }
    (out, o, n)
}

/// Half-open 0-based base span of the *changed* lines (context excluded).
/// A pure insertion yields an empty span at the insertion point.
pub fn changed_old_span(h: &Hunk) -> (usize, usize) {
    let (pos, o_end, _) = walk(h);
    let mut span: Option<(usize, usize)> = None;
    for p in pos.iter().filter(|p| p.line.kind != LineKind::Context) {
        let (s, e) = if p.line.kind == LineKind::Removed { (p.old, p.old + 1) } else { (p.old, p.old) };
        span = Some(match span { None => (s, e), Some((a, b)) => (a.min(s), b.max(e)) });
    }
    span.unwrap_or((old_index(h), o_end))
}

/// 0-based inclusive index range.
pub type IdxRange = (usize, usize);

/// 0-based inclusive index ranges of changed lines: (removed lines in old text, added lines in new text).
pub fn changed_line_ranges(h: &Hunk) -> (Option<IdxRange>, Option<IdxRange>) {
    let (pos, _, _) = walk(h);
    let mut old: Option<(usize, usize)> = None;
    let mut new: Option<(usize, usize)> = None;
    let widen = |r: &mut Option<(usize, usize)>, i: usize| {
        *r = Some(match *r { None => (i, i), Some((a, b)) => (a.min(i), b.max(i)) });
    };
    for p in &pos {
        match p.line.kind {
            LineKind::Removed => widen(&mut old, p.old),
            LineKind::Added => widen(&mut new, p.new),
            LineKind::Context => {}
        }
    }
    (old, new)
}

fn sorted_hunks(f: &FileDiff) -> Vec<(usize, &Hunk)> {
    let mut v: Vec<_> = f.hunks.iter().enumerate().collect();
    v.sort_by_key(|(_, h)| old_index(h));
    v
}

fn newline_of(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Apply the hunks for which `accepted(hunk_index)` is true onto `old_text`.
/// Added lines are copied byte-exact from `new_text` when it is available.
pub fn materialize(f: &FileDiff, accepted: impl Fn(usize) -> bool) -> String {
    let old = f.old_text.as_deref().unwrap_or("");
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = f.new_text.as_deref().map(|t| t.split_inclusive('\n').collect()).unwrap_or_default();
    let nl = newline_of(f.new_text.as_deref().unwrap_or(old));
    let mut out = String::with_capacity(old.len());
    let mut pos = 0usize;
    for (i, h) in sorted_hunks(f) {
        if !accepted(i) {
            continue;
        }
        let start = old_index(h).max(pos).min(old_lines.len());
        old_lines[pos..start].iter().for_each(|l| out.push_str(l));
        pos = start;
        for p in walk(h).0 {
            match p.line.kind {
                LineKind::Context => {
                    if let Some(l) = old_lines.get(pos) { out.push_str(l) }
                    pos += 1;
                }
                LineKind::Removed => pos += 1,
                LineKind::Added => match new_lines.get(p.new) {
                    Some(l) if strip_eol(l) == p.line.text => out.push_str(l),
                    _ => { out.push_str(&p.line.text); out.push_str(nl); }
                },
            }
        }
    }
    if pos < old_lines.len() {
        old_lines[pos..].iter().for_each(|l| out.push_str(l));
    }
    out
}

/// Map a 1-based base line through the file's hunks. `None` when the line was removed.
pub fn map_old_line(f: &FileDiff, line: u32) -> Option<u32> {
    let idx = line.checked_sub(1)? as usize;
    let mut delta: i64 = 0;
    for (_, h) in sorted_hunks(f) {
        if idx < old_index(h) {
            break;
        }
        let (pos, o_end, n_end) = walk(h);
        if let Some(p) = pos.iter().find(|p| p.line.kind != LineKind::Added && p.old == idx) {
            return (p.line.kind == LineKind::Context).then_some(p.new as u32 + 1);
        }
        if idx < o_end {
            // Inside the hunk header range but not listed: treat as unchanged.
            continue;
        }
        delta = n_end as i64 - o_end as i64;
    }
    Some((idx as i64 + delta + 1).max(1) as u32)
}
