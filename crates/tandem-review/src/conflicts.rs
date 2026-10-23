//! Intent-conflict detection across concurrent agent tasks.
//!
//! All tasks are assumed to branch from the same base; hunks are compared in
//! base (old-side) coordinates. Rules, per unordered task pair:
//!
//! * **Hard** – same base file and changed lines overlap or sit within
//!   [`ADJACENT_LINES`] lines of each other (both adding the same new file counts).
//! * **Hard** – one task deletes or renames a file the other changes.
//! * **Soft** – same file, hunks apart, but both touch the same innermost symbol.
//! * **Info** – one task edits a symbol's definition, the other adds lines in a
//!   different file mentioning that identifier (likely a call site).

use crate::TaskMeta;
use crate::hunks::{self, base_path};
use crate::symbols::{self, LineRange, SymbolKind, SymbolRef};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tandem_core::{AgentId, DiffSet, FileChange, FileDiff, LineKind, TaskId};

/// Max untouched base lines between two hunks for them to count as adjacent.
pub const ADJACENT_LINES: usize = 3;
/// Identifiers shorter than this never produce call-site (Info) conflicts.
const MIN_IDENT: usize = 4;
const IDENT_STOPLIST: &[&str] = &[
    "main", "init", "__init__", "new", "default", "from", "into", "self", "this", "test", "setup",
    "run", "get", "set", "update", "render", "constructor", "toString", "clone", "drop", "next",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity { Info, Soft, Hard }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// Overlapping or adjacent hunks in the same file.
    HunkOverlap,
    /// Non-overlapping hunks inside the same symbol.
    SameSymbol,
    /// One side deletes the file the other edits.
    DeleteVsEdit,
    /// One side renames the file the other edits (or both rename it differently).
    RenameVsEdit,
    /// One side edits a definition, the other adds a reference elsewhere.
    DefinitionVsUse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictSide {
    pub task: TaskId,
    pub agent: AgentId,
    /// First line of the task prompt.
    pub intent: String,
    /// File on this side (post-change path).
    pub path: PathBuf,
    /// Changed base-file lines on this side (1-based; insertions anchor on the next line).
    pub ranges: Vec<LineRange>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentConflict {
    pub severity: Severity,
    pub kind: ConflictKind,
    /// Base path the conflict is about (for DefinitionVsUse: the definition file).
    pub file: PathBuf,
    pub symbol: Option<String>,
    pub a: ConflictSide,
    pub b: ConflictSide,
    /// One-line human explanation.
    pub explanation: String,
}

struct HunkInfo {
    span: (usize, usize),
    range: LineRange,
    symbols: Vec<SymbolRef>,
}

struct FileInfo<'a> {
    file: &'a FileDiff,
    base: &'a Path,
    hunks: Vec<HunkInfo>,
    added_idents: HashSet<String>,
}

struct TaskInfo<'a> {
    meta: &'a TaskMeta,
    files: Vec<FileInfo<'a>>,
}

fn ident_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*").expect("valid regex"))
}

fn span_to_range((s, e): (usize, usize)) -> LineRange {
    if e > s { LineRange::new(s as u32 + 1, e as u32) } else { LineRange::new(s as u32 + 1, s as u32 + 1) }
}

fn analyze<'a>(meta: &'a TaskMeta, diff: &'a DiffSet) -> TaskInfo<'a> {
    let files = diff.files.iter().map(|f| {
        let syms = symbols::hunk_symbols(f);
        let hunks = f.hunks.iter().zip(syms).map(|(h, symbols)| {
            let span = hunks::changed_old_span(h);
            HunkInfo { span, range: span_to_range(span), symbols }
        }).collect();
        let mut added_idents = HashSet::new();
        for l in f.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == LineKind::Added) {
            added_idents.extend(ident_re().find_iter(&l.text).map(|m| m.as_str().to_string()));
        }
        FileInfo { file: f, base: base_path(f), hunks, added_idents }
    }).collect();
    TaskInfo { meta, files }
}

fn gap(a: (usize, usize), b: (usize, usize)) -> usize {
    // Untouched base lines strictly between two half-open spans (0 when overlapping/touching).
    b.0.saturating_sub(a.1).max(a.0.saturating_sub(b.1))
}

fn side(t: &TaskInfo, f: &FileInfo, ranges: Vec<LineRange>) -> ConflictSide {
    ConflictSide {
        task: t.meta.task.clone(),
        agent: t.meta.agent.clone(),
        intent: t.meta.intent_line().to_string(),
        path: f.file.path.clone(),
        ranges,
    }
}

fn all_ranges(f: &FileInfo) -> Vec<LineRange> { f.hunks.iter().map(|h| h.range).collect() }

/// (container, name)
type SymKey = (Option<String>, String);

fn sym_key(s: &SymbolRef) -> SymKey { (s.container.clone(), s.name.clone()) }

/// Detect intent conflicts between every pair of tasks.
pub fn detect(tasks: &[(TaskMeta, DiffSet)]) -> Vec<IntentConflict> {
    let infos: Vec<TaskInfo> = tasks.iter().map(|(m, d)| analyze(m, d)).collect();
    let mut out = Vec::new();
    for i in 0..infos.len() {
        for j in i + 1..infos.len() {
            if infos[i].meta.task == infos[j].meta.task { continue }
            pair(&infos[i], &infos[j], &mut out);
            uses(&infos[i], &infos[j], &mut out);
            uses(&infos[j], &infos[i], &mut out);
        }
    }
    out.sort_by(|x, y| y.severity.cmp(&x.severity).then_with(|| x.file.cmp(&y.file)));
    out
}

fn pair(a: &TaskInfo, b: &TaskInfo, out: &mut Vec<IntentConflict>) {
    for fa in &a.files {
        for fb in b.files.iter().filter(|fb| fb.base == fa.base) {
            if fa.file.change == FileChange::Deleted && fb.file.change == FileChange::Deleted {
                continue; // Same intent; nothing to reconcile.
            }
            if let Some(c) = structural(a, fa, b, fb) {
                out.push(c);
                continue;
            }
            same_file(a, fa, b, fb, out);
        }
    }
}

/// Rule (c): delete/rename vs edit.
fn structural(a: &TaskInfo, fa: &FileInfo, b: &TaskInfo, fb: &FileInfo) -> Option<IntentConflict> {
    use FileChange::*;
    let (ca, cb) = (fa.file.change, fb.file.change);
    let kind = match (ca, cb) {
        (Deleted, Deleted) => return None,
        (Renamed, Renamed) if fa.file.path == fb.file.path => return None,
        (Deleted, _) | (_, Deleted) => ConflictKind::DeleteVsEdit,
        (Renamed, _) | (_, Renamed) => ConflictKind::RenameVsEdit,
        _ => return None,
    };
    let verb = |t: &TaskInfo, f: &FileInfo| match f.file.change {
        Deleted => format!("{} deletes it", t.meta.agent),
        Renamed => format!("{} renames it to {}", t.meta.agent, f.file.path.display()),
        _ => format!("{} edits it", t.meta.agent),
    };
    Some(IntentConflict {
        severity: Severity::Hard,
        kind,
        file: fa.base.to_path_buf(),
        symbol: None,
        a: side(a, fa, all_ranges(fa)),
        b: side(b, fb, all_ranges(fb)),
        explanation: format!("{}: {} while {}", fa.base.display(), verb(a, fa), verb(b, fb)),
    })
}

/// Rules (a) and (b): overlapping hunks, then shared symbols among the rest.
fn same_file(a: &TaskInfo, fa: &FileInfo, b: &TaskInfo, fb: &FileInfo, out: &mut Vec<IntentConflict>) {
    let mut hard_pairs = HashSet::new();
    let (mut ra, mut rb) = (Vec::new(), Vec::new());
    let mut hard_symbol = None;
    for (ia, ha) in fa.hunks.iter().enumerate() {
        for (ib, hb) in fb.hunks.iter().enumerate() {
            if gap(ha.span, hb.span) <= ADJACENT_LINES {
                hard_pairs.insert((ia, ib));
                if !ra.contains(&ha.range) { ra.push(ha.range) }
                if !rb.contains(&hb.range) { rb.push(hb.range) }
                if hard_symbol.is_none() {
                    hard_symbol = ha.symbols.iter().find(|s| hb.symbols.iter().any(|t| sym_key(s) == sym_key(t))).map(SymbolRef::qualified);
                }
            }
        }
    }
    // Binary or hunk-less modifications of the same file still collide.
    let wholesale = (fa.hunks.is_empty() || fb.hunks.is_empty()) && (fa.file.binary || fb.file.binary);
    if !hard_pairs.is_empty() || wholesale {
        let what = match &hard_symbol { Some(s) => format!(" in `{s}`"), None => String::new() };
        out.push(IntentConflict {
            severity: Severity::Hard,
            kind: ConflictKind::HunkOverlap,
            file: fa.base.to_path_buf(),
            symbol: hard_symbol,
            explanation: format!(
                "{}: {} and {} change the same lines{what} ({} vs {})",
                fa.base.display(), a.meta.agent, b.meta.agent, fmt_ranges(&ra), fmt_ranges(&rb)
            ),
            a: side(a, fa, ra),
            b: side(b, fb, rb),
        });
    }
    // Rule (b): group remaining hunk pairs by shared symbol.
    let mut by_symbol: BTreeMap<SymKey, (SymbolKind, Vec<LineRange>, Vec<LineRange>)> = BTreeMap::new();
    for (ia, ha) in fa.hunks.iter().enumerate() {
        for (ib, hb) in fb.hunks.iter().enumerate() {
            if hard_pairs.contains(&(ia, ib)) { continue }
            for s in ha.symbols.iter().filter(|s| hb.symbols.iter().any(|t| sym_key(s) == sym_key(t))) {
                let e = by_symbol.entry(sym_key(s)).or_insert((s.kind, Vec::new(), Vec::new()));
                if !e.1.contains(&ha.range) { e.1.push(ha.range) }
                if !e.2.contains(&hb.range) { e.2.push(hb.range) }
            }
        }
    }
    for ((container, name), (kind, ra, rb)) in by_symbol {
        let q = match container { Some(c) => format!("{c}.{name}"), None => name };
        out.push(IntentConflict {
            severity: Severity::Soft,
            kind: ConflictKind::SameSymbol,
            file: fa.base.to_path_buf(),
            symbol: Some(q.clone()),
            explanation: format!(
                "{}: {} and {} both edit {} `{q}` in different places",
                fa.base.display(), a.meta.agent, b.meta.agent, kind_word(kind)
            ),
            a: side(a, fa, ra),
            b: side(b, fb, rb),
        });
    }
}

/// Rule (d): `def` edits a definition, `user` adds a mention in another file.
fn uses(def: &TaskInfo, user: &TaskInfo, out: &mut Vec<IntentConflict>) {
    let mut seen = HashSet::new();
    for fd in &def.files {
        for h in &fd.hunks {
            for s in &h.symbols {
                if s.name.len() < MIN_IDENT || IDENT_STOPLIST.contains(&s.name.as_str()) { continue }
                for fu in user.files.iter().filter(|fu| fu.base != fd.base && fu.added_idents.contains(&s.name)) {
                    if !seen.insert((fd.base, s.name.clone(), fu.base)) { continue }
                    let ranges: Vec<LineRange> = fu.hunks.iter().zip(&fu.file.hunks)
                        .filter(|(_, raw)| raw.lines.iter().any(|l| l.kind == LineKind::Added && mentions(&l.text, &s.name)))
                        .map(|(hi, _)| hi.range).collect();
                    out.push(IntentConflict {
                        severity: Severity::Info,
                        kind: ConflictKind::DefinitionVsUse,
                        file: fd.base.to_path_buf(),
                        symbol: Some(s.qualified()),
                        explanation: format!(
                            "{} changes {} `{}` in {} while {} adds a use of it in {}",
                            def.meta.agent, kind_word(s.kind), s.qualified(), fd.base.display(),
                            user.meta.agent, fu.base.display()
                        ),
                        a: side(def, fd, vec![h.range]),
                        b: side(user, fu, ranges),
                    });
                }
            }
        }
    }
}

fn mentions(line: &str, name: &str) -> bool {
    ident_re().find_iter(line).any(|m| m.as_str() == name)
}

fn kind_word(k: SymbolKind) -> &'static str {
    match k { SymbolKind::Fn => "function", SymbolKind::Class => "type", SymbolKind::Method => "method", SymbolKind::Const => "constant" }
}

fn fmt_ranges(r: &[LineRange]) -> String {
    r.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}
