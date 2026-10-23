//! Review model: a `tandem_core::DiffSet` plus a unified-patch renderer for gpui-kit's `Diff`.
//! Integration point: `tandem_review::diffset_for_task(task) -> DiffSet`.

use std::fmt::Write as _;
use tandem_core::{DiffLine, DiffSet, FileChange, FileDiff, Hunk, LineKind};

/// Render one `FileDiff` as a git-style unified patch that `DiffFile::parse` accepts.
pub fn to_unified_patch(f: &FileDiff) -> String {
    let path = f.path.to_string_lossy();
    let old = f.old_path.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let mut s = String::new();
    let _ = writeln!(s, "diff --git a/{old} b/{path}");
    match f.change {
        FileChange::Added => {
            let _ = writeln!(s, "new file mode 100644\n--- /dev/null\n+++ b/{path}");
        }
        FileChange::Deleted => {
            let _ = writeln!(s, "deleted file mode 100644\n--- a/{old}\n+++ /dev/null");
        }
        _ => {
            let _ = writeln!(s, "--- a/{old}\n+++ b/{path}");
        }
    }
    for h in &f.hunks {
        let _ = writeln!(s, "@@ -{},{} +{},{} @@", h.old_start, h.old_lines, h.new_start, h.new_lines);
        for l in &h.lines {
            let sign = match l.kind {
                LineKind::Context => ' ',
                LineKind::Added => '+',
                LineKind::Removed => '-',
            };
            let _ = writeln!(s, "{sign}{}", l.text);
        }
    }
    s
}

fn hunk(old_start: u32, new_start: u32, lines: &[(&str, &str)]) -> Hunk {
    let (mut o, mut n) = (old_start, new_start);
    let mut out = Vec::new();
    for (k, t) in lines {
        let kind = match *k {
            "+" => LineKind::Added,
            "-" => LineKind::Removed,
            _ => LineKind::Context,
        };
        let (old_no, new_no) = match kind {
            LineKind::Context => {
                o += 1;
                n += 1;
                (Some(o - 1), Some(n - 1))
            }
            LineKind::Added => {
                n += 1;
                (None, Some(n - 1))
            }
            LineKind::Removed => {
                o += 1;
                (Some(o - 1), None)
            }
        };
        out.push(DiffLine { kind, old_no, new_no, text: (*t).to_string() });
    }
    Hunk { old_start, old_lines: o - old_start, new_start, new_lines: n - new_start, lines: out }
}

pub fn fake_diffset() -> DiffSet {
    let client = FileDiff {
        path: "src/http/client.ts".into(),
        old_path: None,
        change: FileChange::Modified,
        old_text: None,
        new_text: None,
        binary: false,
        hunks: vec![
            hunk(1, 1, &[
                (" ", "import { fetchWithTimeout } from './timeout';"),
                ("+", "import { RetryPolicy, backoff } from './retry';"),
                (" ", ""),
                (" ", "export interface ClientOptions {"),
                (" ", "  baseUrl: string;"),
                ("+", "  retry?: RetryPolicy;"),
                (" ", "}"),
            ]),
            hunk(24, 26, &[
                (" ", "  async request<T>(path: string, init?: RequestInit): Promise<T> {"),
                ("-", "    const res = await fetchWithTimeout(this.url(path), init);"),
                ("-", "    if (!res.ok) throw new HttpError(res.status);"),
                ("+", "    const policy = this.options.retry ?? RetryPolicy.default();"),
                ("+", "    for (let attempt = 0; ; attempt++) {"),
                ("+", "      const res = await fetchWithTimeout(this.url(path), init);"),
                ("+", "      if (res.ok) return res.json() as Promise<T>;"),
                ("+", "      if (!policy.shouldRetry(res.status, attempt)) throw new HttpError(res.status);"),
                ("+", "      await backoff(attempt, policy);"),
                ("+", "    }"),
                (" ", "  }"),
            ]),
        ],
    };
    let retry = FileDiff {
        path: "src/http/retry.ts".into(),
        old_path: None,
        change: FileChange::Added,
        old_text: None,
        new_text: None,
        binary: false,
        hunks: vec![hunk(0, 1, &[
            ("+", "export class RetryPolicy {"),
            ("+", "  constructor(readonly maxAttempts = 3, readonly baseMs = 200) {}"),
            ("+", "  static default() { return new RetryPolicy(); }"),
            ("+", "  shouldRetry(status: number, attempt: number) {"),
            ("+", "    return attempt + 1 < this.maxAttempts && (status >= 500 || status === 429);"),
            ("+", "  }"),
            ("+", "}"),
            ("+", ""),
            ("+", "export const backoff = (attempt: number, p: RetryPolicy) =>"),
            ("+", "  new Promise((r) => setTimeout(r, p.baseMs * 2 ** attempt * (0.5 + Math.random())));"),
        ])],
    };
    DiffSet { task: "task-1".into(), agent: "claude".into(), base_commit: "4f2a9c1".into(), files: vec![client, retry] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_has_headers_and_hunks() {
        let ds = fake_diffset();
        let p = to_unified_patch(&ds.files[0]);
        assert!(p.starts_with("diff --git a/src/http/client.ts b/src/http/client.ts"));
        assert!(p.contains("@@ -1,5 +1,7 @@"));
    }
}
