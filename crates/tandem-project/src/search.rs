//! Find in Files / Replace in Files, powered by the ripgrep crates over `ignore::WalkParallel`.

use anyhow::{Context, Result, bail};
use grep::matcher::Matcher;
use grep::regex::{RegexMatcher, RegexMatcherBuilder};
use grep::searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::overrides::OverrideBuilder;
use ignore::{WalkBuilder, WalkState};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tandem_core::{Receiver, unbounded};

#[derive(Clone, Debug)]
pub struct SearchOptions {
    /// Treat the query as a regular expression (otherwise literal).
    pub regex: bool,
    /// Case-sensitive matching (default false).
    pub case: bool,
    pub whole_word: bool,
    /// Gitignore-style globs; when non-empty only matching files are searched (e.g. `*.ts`).
    pub include_glob: Vec<String>,
    /// Gitignore-style globs to skip (e.g. `dist/**`, `*.min.js`).
    pub exclude_glob: Vec<String>,
    /// Stop after this many matching lines.
    pub max_results: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self { regex: false, case: false, whole_word: false, include_glob: vec![], exclude_glob: vec![], max_results: 10_000 }
    }
}

/// Cooperative cancellation shared with the background search.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// One matching line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Absolute path (`root` joined with the relative path).
    pub path: PathBuf,
    /// 1-based line number.
    pub line_no: u64,
    /// 1-based character column of the first match on the line.
    pub col: usize,
    /// The line without its terminator.
    pub line_text: String,
    /// Byte ranges of every match within `line_text`.
    pub ranges: Vec<Range<usize>>,
}

fn pattern(query: &str, opts: &SearchOptions) -> String {
    if opts.regex { query.to_string() } else { regex::escape(query) }
}

fn build_matcher(query: &str, opts: &SearchOptions) -> Result<RegexMatcher> {
    if query.is_empty() {
        bail!("empty search query");
    }
    RegexMatcherBuilder::new()
        .case_insensitive(!opts.case)
        .word(opts.whole_word)
        .build(&pattern(query, opts))
        .with_context(|| format!("invalid pattern {query:?}"))
}

fn build_walker(root: &Path, opts: &SearchOptions) -> Result<WalkBuilder> {
    let mut wb = crate::walker(root);
    if !opts.include_glob.is_empty() || !opts.exclude_glob.is_empty() {
        let mut ob = OverrideBuilder::new(root);
        for g in &opts.include_glob {
            ob.add(g).with_context(|| format!("bad include glob {g:?}"))?;
        }
        for g in &opts.exclude_glob {
            ob.add(&format!("!{g}")).with_context(|| format!("bad exclude glob {g:?}"))?;
        }
        wb.overrides(ob.build()?);
    }
    Ok(wb)
}

struct Collector<'a> {
    matcher: &'a RegexMatcher,
    path: &'a Path,
    out: &'a mut Vec<Match>,
    budget: &'a AtomicUsize,
    limit: usize,
    cancel: &'a CancelToken,
}

impl Sink for Collector<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _s: &Searcher, m: &SinkMatch<'_>) -> std::result::Result<bool, Self::Error> {
        if self.cancel.is_cancelled() {
            return Ok(false);
        }
        // Reserve a slot in the global budget.
        let prev = self.budget.fetch_add(1, Ordering::Relaxed);
        if prev >= self.limit {
            return Ok(false);
        }
        let raw = m.bytes();
        let line = String::from_utf8_lossy(raw);
        let line_text = line.trim_end_matches(['\n', '\r']).to_string();
        let mut ranges = Vec::new();
        let hay = line_text.as_bytes();
        let _ = self.matcher.find_iter(hay, |r| {
            ranges.push(r.start()..r.end());
            true
        });
        let col = ranges.first().map(|r| line_text[..r.start.min(line_text.len())].chars().count() + 1).unwrap_or(1);
        self.out.push(Match {
            path: self.path.to_path_buf(),
            line_no: m.line_number().unwrap_or(0),
            col,
            line_text,
            ranges,
        });
        Ok(true)
    }
}

/// Stream matches for `query` under `root`. The returned channel closes when the search finishes,
/// hits `max_results`, or `cancel` fires. Matches arrive in no particular cross-file order.
///
/// Fails up front on an invalid pattern/glob.
pub fn find_in_files(root: &Path, query: &str, opts: &SearchOptions, cancel: CancelToken) -> Result<Receiver<Match>> {
    let matcher = build_matcher(query, opts)?;
    let walker = build_walker(root, opts)?.build_parallel();
    let (tx, rx) = unbounded();
    let limit = opts.max_results;
    let root = root.to_path_buf();
    std::thread::Builder::new().name("tandem-search".into()).spawn(move || {
        let count = Arc::new(AtomicUsize::new(0));
        walker.run(|| {
            let matcher = matcher.clone();
            let tx = tx.clone();
            let cancel = cancel.clone();
            let count = count.clone();
            let root = root.clone();
            let mut searcher = SearcherBuilder::new()
                .line_number(true)
                .binary_detection(BinaryDetection::quit(0))
                .build();
            Box::new(move |res| {
                if cancel.is_cancelled() || count.load(Ordering::Relaxed) >= limit {
                    return WalkState::Quit;
                }
                let Ok(dent) = res else { return WalkState::Continue };
                if !dent.file_type().is_some_and(|t| t.is_file()) {
                    return WalkState::Continue;
                }
                let path = root.join(dent.path().strip_prefix(&root).unwrap_or(dent.path()));
                let mut found = Vec::new();
                let mut sink = Collector { matcher: &matcher, path: &path, out: &mut found, budget: &count, limit, cancel: &cancel };
                let _ = searcher.search_path(&matcher, dent.path(), &mut sink);
                for m in found {
                    if tx.send_blocking(m).is_err() {
                        return WalkState::Quit;
                    }
                }
                WalkState::Continue
            })
        });
    })?;
    Ok(rx)
}

/// Blocking convenience: run the search to completion, sorted by (path, line).
pub fn find_in_files_collect(root: &Path, query: &str, opts: &SearchOptions) -> Result<Vec<Match>> {
    let rx = find_in_files(root, query, opts, CancelToken::new())?;
    let mut v = Vec::new();
    while let Ok(m) = rx.recv_blocking() {
        v.push(m);
    }
    v.sort_by(|a, b| a.path.cmp(&b.path).then(a.line_no.cmp(&b.line_no)));
    Ok(v)
}

/// A proposed single-line replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaceItem {
    pub path: PathBuf,
    pub line_no: u64,
    pub old_line: String,
    pub new_line: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplaceReport {
    pub files_changed: usize,
    pub lines_changed: usize,
    /// Items whose line no longer matched `old_line` (file edited since the preview).
    pub stale: Vec<ReplaceItem>,
}

fn replace_regex(query: &str, opts: &SearchOptions) -> Result<regex::Regex> {
    let mut p = pattern(query, opts);
    if opts.whole_word {
        p = format!(r"\b(?:{p})\b");
    }
    regex::RegexBuilder::new(&p).case_insensitive(!opts.case).build().with_context(|| format!("invalid pattern {query:?}"))
}

/// Compute (without touching disk) what replacing `query` by `replacement` would do.
/// In regex mode `replacement` may use `$1` / `${name}` capture references.
/// Callers can filter the returned list (e.g. unchecked rows) before [`apply_replacements`].
pub fn replace_preview(
    root: &Path,
    query: &str,
    replacement: &str,
    opts: &SearchOptions,
    cancel: CancelToken,
) -> Result<Vec<ReplaceItem>> {
    let re = replace_regex(query, opts)?;
    let rx = find_in_files(root, query, opts, cancel)?;
    let mut items = Vec::new();
    while let Ok(m) = rx.recv_blocking() {
        let new_line = if opts.regex {
            re.replace_all(&m.line_text, replacement).into_owned()
        } else {
            re.replace_all(&m.line_text, regex::NoExpand(replacement)).into_owned()
        };
        if new_line != m.line_text {
            items.push(ReplaceItem { path: m.path, line_no: m.line_no, old_line: m.line_text, new_line });
        }
    }
    items.sort_by(|a, b| a.path.cmp(&b.path).then(a.line_no.cmp(&b.line_no)));
    Ok(items)
}

/// Write the replacements to disk. Line terminators are preserved; stale items are skipped and reported.
pub fn apply_replacements(items: &[ReplaceItem]) -> Result<ReplaceReport> {
    let mut report = ReplaceReport::default();
    let mut by_file: std::collections::BTreeMap<&Path, Vec<&ReplaceItem>> = Default::default();
    for it in items {
        by_file.entry(it.path.as_path()).or_default().push(it);
    }
    for (path, its) in by_file {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
        let mut changed = 0;
        for it in its {
            let idx = it.line_no as usize - 1;
            let Some(line) = lines.get_mut(idx) else {
                report.stale.push(it.clone());
                continue;
            };
            let body_len = line.trim_end_matches(['\n', '\r']).len();
            if line[..body_len] != it.old_line {
                report.stale.push(it.clone());
                continue;
            }
            line.replace_range(..body_len, &it.new_line);
            changed += 1;
        }
        if changed > 0 {
            let meta = std::fs::metadata(path)?;
            #[cfg(unix)]
            let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777;
            #[cfg(not(unix))]
            let mode = { let _ = meta; 0o644 };
            tandem_core::fs::atomic_write_mode(path, lines.concat().as_bytes(), mode)?;
            report.files_changed += 1;
            report.lines_changed += changed;
        }
    }
    Ok(report)
}
