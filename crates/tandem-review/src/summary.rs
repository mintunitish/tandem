//! One-glance summary of a DiffSet: counts, touched symbols, risk flags.

use crate::symbols;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tandem_core::{DiffSet, FileChange, FileDiff, LineKind};

/// Deletions above this (per file, or in total) are flagged.
pub const LARGE_DELETION: usize = 200;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub files: usize,
    pub additions: usize,
    pub deletions: usize,
    /// `path: Container.name` for every symbol a hunk touched, deduplicated.
    pub touched_symbols: Vec<String>,
    /// Human-readable risk flags.
    pub risky: Vec<String>,
}

struct Rules { lock: Regex, ci: Regex, migration: Regex, secret: Regex, test_file: Regex, test_decl: Regex }

fn rules() -> &'static Rules {
    static R: OnceLock<Rules> = OnceLock::new();
    R.get_or_init(|| {
        let re = |s: &str| Regex::new(s).expect("valid regex");
        Rules {
            lock: re(r"(^|/)(cargo\.lock|package-lock\.json|npm-shrinkwrap\.json|yarn\.lock|pnpm-lock\.yaml|bun\.lockb?|poetry\.lock|pipfile\.lock|uv\.lock|go\.sum|gemfile\.lock|composer\.lock|flake\.lock|[^/]+\.lock)$"),
            ci: re(r"(^|/)(\.github/workflows/|\.gitlab-ci\.ya?ml$|\.circleci/|jenkinsfile$|azure-pipelines\.ya?ml$|\.buildkite/|\.travis\.ya?ml$|bitbucket-pipelines\.ya?ml$|\.drone\.ya?ml$)"),
            migration: re(r"(^|/)(migrations?|migrate|alembic|db/schema)(/|\.)|(^|/)\d{3,}[_-][^/]*\.sql$|(^|/)schema\.(sql|prisma|rb)$"),
            secret: re(r"(^|/)(\.env(\.[^/]*)?|[^/]*\.(pem|key|p12|pfx|keystore|jks)|id_(rsa|ed25519|ecdsa)[^/]*|\.npmrc|\.pypirc|\.netrc|\.ssh/|\.aws/)$|(^|[/_.-])(auth|oauth|secrets?|credentials?|passwords?|tokens?|keychain|permissions?|security)([/_.-]|$)"),
            test_file: re(r"(^|/)(tests?|__tests__|spec)/|(^|/)test_[^/]+\.py$|_test\.(py|go)$|\.(test|spec)\.[jt]sx?$"),
            test_decl: re(r"#\[(tokio::)?test\]|^\s*(async\s+)?def\s+test_|^\s*func\s+Test[A-Z_]|^\s*(it|test|describe)(\.only|\.skip)?\s*\("),
        }
    })
}

fn counts(f: &FileDiff) -> (usize, usize) {
    let lines = f.hunks.iter().flat_map(|h| &h.lines);
    let (mut a, mut d) = (0, 0);
    for l in lines {
        match l.kind { LineKind::Added => a += 1, LineKind::Removed => d += 1, LineKind::Context => {} }
    }
    (a, d)
}

/// Summarize a DiffSet.
pub fn describe(diff: &DiffSet) -> ReviewSummary {
    let r = rules();
    let mut s = ReviewSummary { files: diff.files.len(), ..Default::default() };
    let mut large_file_flagged = false;
    for f in &diff.files {
        let (a, d) = counts(f);
        s.additions += a;
        s.deletions += d;
        let shown = f.path.display().to_string();
        let paths: Vec<String> = std::iter::once(f.path.as_path()).chain(f.old_path.as_deref())
            .map(|p| p.to_string_lossy().replace('\\', "/").to_ascii_lowercase()).collect();
        let any = |re: &Regex| paths.iter().any(|p| re.is_match(p));

        if any(&r.lock) { s.risky.push(format!("lockfile changed: {shown}")) }
        if any(&r.ci) { s.risky.push(format!("CI config changed: {shown}")) }
        if any(&r.migration) { s.risky.push(format!("database migration changed: {shown}")) }
        if any(&r.secret) { s.risky.push(format!("auth/secret-looking file changed: {shown}")) }
        if any(&r.test_file) && f.change == FileChange::Deleted {
            s.risky.push(format!("test file deleted: {shown}"));
        } else {
            let decl = |k: LineKind| f.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == k && r.test_decl.is_match(&l.text)).count();
            let (removed, added) = (decl(LineKind::Removed), decl(LineKind::Added));
            if removed > added { s.risky.push(format!("{} test(s) removed: {shown}", removed - added)) }
        }
        if d > LARGE_DELETION {
            large_file_flagged = true;
            s.risky.push(format!("large deletion: {d} lines in {shown}"));
        }
        if f.change == FileChange::Deleted && d == 0 && !f.binary {
            // Deletion without hunks (producer omitted them): count from old text.
            let n = f.old_text.as_deref().map(|t| t.lines().count()).unwrap_or(0);
            if n > LARGE_DELETION {
                large_file_flagged = true;
                s.risky.push(format!("large deletion: {n} lines in {shown}"));
            }
        }

        for syms in symbols::hunk_symbols(f) {
            for sym in syms {
                let q = format!("{}: {}", shown, sym.qualified());
                if !s.touched_symbols.contains(&q) { s.touched_symbols.push(q) }
            }
        }
    }
    if !large_file_flagged && s.deletions > LARGE_DELETION {
        s.risky.push(format!("large deletion: {} lines across {} files", s.deletions, s.files));
    }
    s
}
