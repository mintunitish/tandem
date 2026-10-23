//! Search-Everywhere style fuzzy matching: camel humps, path segments, recent-file boost.
//!
//! Ranking tiers (score bands are 2000 apart so a higher tier always wins):
//! exact name > exact stem > name prefix > camel-hump > name substring > scattered in name > path-only.

use crate::watcher::{FsEvent, FsEventKind};
use crate::{is_excluded_name, is_excluded_path};
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const EXACT: i32 = 10_000;
const STEM: i32 = 9_500;
const PREFIX: i32 = 8_000;
const HUMP: i32 = 6_000;
const SUBSTR: i32 = 4_000;
const SCATTER: i32 = 2_000;
const PATH_ONLY: i32 = 1_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tier {
    Exact,
    Stem,
    Prefix,
    Hump,
    Substr,
    Scatter,
}

/// Reusable buffers so scoring allocates nothing in the hot loop.
#[derive(Default)]
pub(crate) struct Scratch {
    table: Vec<bool>,
}

/// A parsed query: lowercase bytes plus its `/`-separated parts.
pub(crate) struct Query {
    pub lower: Vec<u8>,
    parts: Vec<Vec<u8>>,
}

impl Query {
    pub fn new(raw: &str) -> Self {
        let cleaned: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
        let cleaned = cleaned.trim_start_matches("./");
        let lower: Vec<u8> = cleaned.bytes().map(|b| b.to_ascii_lowercase()).collect();
        let parts = if lower.contains(&b'/') { lower.split(|b| *b == b'/').map(<[u8]>::to_vec).collect() } else { vec![] };
        Self { lower, parts }
    }
    pub fn is_empty(&self) -> bool {
        self.lower.is_empty()
    }
}

#[inline]
fn lc(b: u8) -> u8 {
    b.to_ascii_lowercase()
}

#[inline]
fn is_sep(b: u8) -> bool {
    matches!(b, b'_' | b'-' | b'.' | b' ' | b'/' | b'$' | b':')
}

#[inline]
fn is_hump(name: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let (c, p) = (name[i], name[i - 1]);
    if is_sep(p) {
        return true;
    }
    if c.is_ascii_uppercase() {
        return !p.is_ascii_uppercase() || name.get(i + 1).is_some_and(u8::is_ascii_lowercase);
    }
    c.is_ascii_digit() && !p.is_ascii_digit()
}

fn greedy(q: &[u8], name: &[u8]) -> Option<(usize, usize)> {
    let (mut qi, mut first) = (0, 0);
    for (i, &b) in name.iter().enumerate() {
        if lc(b) == q[qi] {
            if qi == 0 {
                first = i;
            }
            qi += 1;
            if qi == q.len() {
                return Some((first, i));
            }
        }
    }
    None
}

/// Fill `s.table` (q x n) with "q[..=qi] can match ending exactly at name[ni]" under hump rules:
/// each matched char is either a hump start or directly continues the previous match.
fn hump_table(q: &[u8], name: &[u8], s: &mut Scratch) {
    let n = name.len();
    s.table.clear();
    s.table.resize(q.len() * n, false);
    for (qi, &qc) in q.iter().enumerate() {
        let mut any_prev = false;
        for ni in 0..n {
            let r = lc(name[ni]) == qc
                && if qi == 0 {
                    is_hump(name, ni)
                } else {
                    (ni > 0 && s.table[(qi - 1) * n + ni - 1]) || (any_prev && is_hump(name, ni))
                };
            s.table[qi * n + ni] = r;
            if qi > 0 && s.table[(qi - 1) * n + ni] {
                any_prev = true;
            }
        }
    }
}

fn hump_end(q: &[u8], n: usize, s: &Scratch) -> Option<usize> {
    let last = (q.len() - 1) * n;
    (0..n).find(|&ni| s.table[last + ni])
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()].iter().zip(needle).all(|(a, b)| lc(*a) == *b))
}

fn eq_lower(name: &[u8], q: &[u8]) -> bool {
    name.len() == q.len() && name.iter().zip(q).all(|(a, b)| lc(*a) == *b)
}

/// Classify `q` (already lowercase) against `name`; returns tier, score and an aux value
/// (substring offset for `Tier::Substr`).
fn classify(q: &[u8], name: &[u8], s: &mut Scratch) -> Option<(Tier, i32, usize)> {
    if q.is_empty() || q.len() > name.len() {
        return None;
    }
    let len_pen = (name.len() as i32).min(200);
    if eq_lower(name, q) {
        return Some((Tier::Exact, EXACT, 0));
    }
    if let Some(dot) = name.iter().rposition(|b| *b == b'.')
        && dot > 0
        && eq_lower(&name[..dot], q)
    {
        return Some((Tier::Stem, STEM - len_pen, 0));
    }
    if name.iter().zip(q).all(|(a, b)| lc(*a) == *b) {
        return Some((Tier::Prefix, PREFIX - len_pen, 0));
    }
    let (first, last) = greedy(q, name)?;
    hump_table(q, name, s);
    if let Some(end) = hump_end(q, name.len(), s) {
        let bonus = if first == 0 { 200 } else { 0 };
        return Some((Tier::Hump, HUMP + bonus - len_pen - (end as i32), 0));
    }
    if let Some(pos) = find_sub(name, q) {
        return Some((Tier::Substr, SUBSTR - (pos as i32) * 10 - len_pen, pos));
    }
    let span = (last - first + 1 - q.len()) as i32;
    Some((Tier::Scatter, SCATTER - span * 10 - (first as i32) * 2 - len_pen, 0))
}

pub(crate) fn score_name(q: &[u8], name: &[u8], s: &mut Scratch) -> Option<i32> {
    classify(q, name, s).map(|t| t.1)
}

fn name_positions(tier: Tier, q: &[u8], name: &[u8], aux: usize, s: &mut Scratch, offset: usize, out: &mut Vec<usize>) {
    match tier {
        Tier::Exact | Tier::Prefix => out.extend((0..q.len()).map(|i| offset + i)),
        Tier::Stem => out.extend((0..q.len()).map(|i| offset + i)),
        Tier::Substr => out.extend((0..q.len()).map(|i| offset + aux + i)),
        Tier::Hump => {
            hump_table(q, name, s);
            let n = name.len();
            let mut ni = hump_end(q, n, s).unwrap_or(0);
            let mut rev = vec![ni];
            for qi in (1..q.len()).rev() {
                let prev = if ni > 0 && s.table[(qi - 1) * n + ni - 1] {
                    ni - 1
                } else {
                    (0..ni).rev().find(|&nj| s.table[(qi - 1) * n + nj]).unwrap_or(0)
                };
                rev.push(prev);
                ni = prev;
            }
            out.extend(rev.into_iter().rev().map(|p| offset + p));
        }
        Tier::Scatter => {
            let mut qi = 0;
            for (i, &b) in name.iter().enumerate() {
                if qi < q.len() && lc(b) == q[qi] {
                    out.push(offset + i);
                    qi += 1;
                }
            }
        }
    }
}

/// Score one relative path (`/`-separated). Returns the score and optionally highlight positions
/// (byte offsets into `path`).
pub(crate) fn score_path(q: &Query, path: &[u8], s: &mut Scratch, mut pos: Option<&mut Vec<usize>>) -> Option<i32> {
    let name_start = path.iter().rposition(|b| *b == b'/').map_or(0, |i| i + 1);
    let name = &path[name_start..];
    let depth_pen = path[..name_start].iter().filter(|b| **b == b'/').count() as i32 * 3;

    if q.parts.len() > 1 {
        return score_slash(q, path, name_start, s, pos, depth_pen);
    }

    if let Some((tier, score, aux)) = classify(&q.lower, name, s) {
        if let Some(out) = pos.as_deref_mut() {
            name_positions(tier, &q.lower, name, aux, s, name_start, out);
        }
        return Some(score - depth_pen);
    }

    // Path-only: the query hits a directory segment, or is scattered over the whole path.
    type SegHit = (usize, Tier, usize, usize); // offset, tier, aux, len
    let mut best: Option<(i32, Option<SegHit>)> = None;
    let mut off = 0;
    for seg in path[..name_start.saturating_sub(1).min(path.len())].split(|b| *b == b'/') {
        if let Some((tier, sc, aux)) = classify(&q.lower, seg, s) {
            let v = PATH_ONLY + sc / 10;
            if best.as_ref().is_none_or(|b| v > b.0) {
                best = Some((v, Some((off, tier, aux, seg.len()))));
            }
        }
        off += seg.len() + 1;
    }
    if best.is_none()
        && let Some((first, last)) = greedy(&q.lower, path)
    {
        let span = (last - first + 1 - q.lower.len()) as i32;
        best = Some((PATH_ONLY - span * 3 - path.len() as i32 / 4, None));
    }
    let (score, seg) = best?;
    if let Some(out) = pos {
        match seg {
            Some((off, tier, aux, len)) => name_positions(tier, &q.lower, &path[off..off + len], aux, s, off, out),
            None => {
                let mut qi = 0;
                for (i, &b) in path.iter().enumerate() {
                    if qi < q.lower.len() && lc(b) == q.lower[qi] {
                        out.push(i);
                        qi += 1;
                    }
                }
            }
        }
    }
    Some(score - depth_pen)
}

/// `dir/seg/name` style queries: leading parts match directory segments in order, last part the file name.
fn score_slash(q: &Query, path: &[u8], name_start: usize, s: &mut Scratch, mut pos: Option<&mut Vec<usize>>, depth_pen: i32) -> Option<i32> {
    let dir = &path[..name_start.saturating_sub(1)];
    let segs: Vec<(usize, &[u8])> = {
        let mut v = Vec::new();
        let mut off = 0;
        if !dir.is_empty() {
            for seg in dir.split(|b| *b == b'/') {
                v.push((off, seg));
                off += seg.len() + 1;
            }
        }
        v
    };
    let (last, dirs) = q.parts.split_last()?;
    let mut cursor = 0;
    let mut total = 0;
    for (k, part) in dirs.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let mut found = None;
        for (i, (off, seg)) in segs.iter().enumerate().skip(cursor) {
            if let Some((tier, sc, aux)) = classify(part, seg, s) {
                found = Some((i, *off, tier, sc, aux, seg));
                break;
            }
        }
        let (i, off, tier, sc, aux, seg) = found?;
        total += sc / 20;
        if k == 0 && i == 0 {
            total += 100; // anchored at the project root
        }
        if let Some(out) = pos.as_deref_mut() {
            name_positions(tier, part, seg, aux, s, off, out);
        }
        cursor = i + 1;
    }
    let name = &path[name_start..];
    if last.is_empty() {
        return Some(PATH_ONLY + total - depth_pen);
    }
    let (tier, sc, aux) = classify(last, name, s)?;
    if let Some(out) = pos {
        name_positions(tier, last, name, aux, s, name_start, out);
    }
    Some(sc + total - depth_pen)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMatch {
    /// Path relative to the index root, `/`-separated.
    pub path: String,
    pub score: i32,
    /// Byte offsets in `path` to highlight.
    pub positions: Vec<usize>,
}

/// Sorted list of project-relative file paths.
#[derive(Default)]
pub struct FileIndex {
    root: PathBuf,
    paths: Vec<String>,
    recent: HashMap<String, i32>,
}

impl FileIndex {
    /// Walk `root` (honoring ignore files and the hidden set) and index every file.
    pub fn build(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut idx = Self { root: root.clone(), ..Default::default() };
        idx.scan(&root);
        idx
    }

    /// Build from raw relative paths (tests, benchmarks).
    pub fn from_paths<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut paths: Vec<String> = paths.into_iter().map(Into::into).collect();
        paths.sort();
        paths.dedup();
        Self { root: PathBuf::new(), paths, recent: HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    fn scan(&mut self, dir: &Path) {
        let mut wb = WalkBuilder::new(dir);
        wb.hidden(false).require_git(false).git_global(false);
        wb.filter_entry(|e| e.depth() == 0 || e.file_name().to_str().is_none_or(|n| !is_excluded_name(n)));
        for dent in wb.build().flatten() {
            if dent.file_type().is_some_and(|t| t.is_file()) {
                self.add(dent.path());
            }
        }
    }

    fn rel(&self, p: &Path) -> Option<String> {
        let rel = p.strip_prefix(&self.root).unwrap_or(p);
        if is_excluded_path(&self.root, p) {
            return None;
        }
        let s = rel.to_str()?;
        Some(if std::path::MAIN_SEPARATOR == '/' { s.to_string() } else { s.replace(std::path::MAIN_SEPARATOR, "/") })
    }

    /// Add a file (absolute or root-relative).
    pub fn add(&mut self, path: &Path) {
        let Some(rel) = self.rel(path) else { return };
        if let Err(i) = self.paths.binary_search(&rel) {
            self.paths.insert(i, rel);
        }
    }

    /// Remove a file, or every file below a directory.
    pub fn remove(&mut self, path: &Path) {
        let Some(rel) = self.rel(path) else { return };
        if let Ok(i) = self.paths.binary_search(&rel) {
            self.paths.remove(i);
        }
        let prefix = format!("{rel}/");
        self.paths.retain(|p| !p.starts_with(&prefix));
    }

    /// Keep the index in sync with watcher events.
    pub fn apply_event(&mut self, ev: &FsEvent) {
        let sync = |this: &mut Self, p: &Path| {
            if p.is_dir() {
                this.scan(p);
            } else if p.is_file() {
                this.add(p);
            } else {
                this.remove(p);
            }
        };
        match ev.kind {
            FsEventKind::Modified => {}
            FsEventKind::Removed => ev.paths.iter().for_each(|p| self.remove(p)),
            FsEventKind::Created => ev.paths.iter().for_each(|p| sync(self, p)),
            FsEventKind::Renamed => ev.paths.iter().for_each(|p| sync(self, p)),
        }
    }

    /// Most-recent-first list of files (absolute or relative) used for the recent boost.
    pub fn set_recent(&mut self, recent: &[PathBuf]) {
        self.recent.clear();
        for (rank, p) in recent.iter().enumerate() {
            if let Some(rel) = self.rel(p) {
                self.recent.entry(rel).or_insert((400 - rank as i32 * 20).max(40));
            }
        }
    }

    fn score_chunk(&self, q: &Query, base: usize, chunk: &[String]) -> Vec<(i32, u32)> {
        let ql = &q.lower[..];
        let mut scratch = Scratch::default();
        let mut out = Vec::new();
        for (i, p) in chunk.iter().enumerate() {
            // Cheap necessary condition for every tier: query is a case-insensitive subsequence.
            let mut qi = 0;
            for &b in p.as_bytes() {
                if lc(b) == ql[qi] {
                    qi += 1;
                    if qi == ql.len() {
                        break;
                    }
                }
            }
            if qi < ql.len() {
                continue;
            }
            if let Some(sc) = score_path(q, p.as_bytes(), &mut scratch, None) {
                let boost = self.recent.get(p).copied().unwrap_or(0);
                out.push((sc + boost, (base + i) as u32));
            }
        }
        out
    }

    fn score_all(&self, q: &Query) -> Vec<(i32, u32)> {
        const PAR_THRESHOLD: usize = 8_192;
        if self.paths.len() < PAR_THRESHOLD {
            return self.score_chunk(q, 0, &self.paths);
        }
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        let chunk = self.paths.len().div_ceil(threads);
        std::thread::scope(|s| {
            let handles: Vec<_> = self
                .paths
                .chunks(chunk)
                .enumerate()
                .map(|(k, c)| s.spawn(move || self.score_chunk(q, k * chunk, c)))
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        })
    }

    /// Top `limit` files for `query`. An empty query returns recent files, then the rest alphabetically.
    pub fn fuzzy_files(&self, query: &str, limit: usize) -> Vec<FileMatch> {
        let q = Query::new(query);
        let mut scratch = Scratch::default();
        let mut scored: Vec<(i32, u32)> = Vec::new();
        if q.is_empty() {
            for (i, p) in self.paths.iter().enumerate() {
                let boost = self.recent.get(p).copied().map_or(0, |b| b + 1000);
                scored.push((boost, i as u32));
            }
        } else {
            scored = self.score_all(&q);
        }
        let paths = &self.paths;
        let cmp = |a: &(i32, u32), b: &(i32, u32)| {
            b.0.cmp(&a.0)
                .then_with(|| paths[a.1 as usize].len().cmp(&paths[b.1 as usize].len()))
                .then_with(|| a.1.cmp(&b.1))
        };
        if limit > 0 && scored.len() > limit {
            scored.select_nth_unstable_by(limit - 1, cmp);
            scored.truncate(limit);
        }
        scored.sort_by(cmp);
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(score, i)| {
                let path = self.paths[i as usize].clone();
                let mut positions = Vec::new();
                if !q.is_empty() {
                    score_path(&q, path.as_bytes(), &mut scratch, Some(&mut positions));
                }
                FileMatch { path, score, positions }
            })
            .collect()
    }
}
