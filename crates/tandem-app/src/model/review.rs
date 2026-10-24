//! Review model helpers: render a `tandem_core::FileDiff` as a unified patch for gpui-kit's
//! `Diff`, and per-file line counts.

use std::fmt::Write as _;
use tandem_core::{FileChange, FileDiff, LineKind};

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
    // tandem-git keeps each line's newline (a line without one ends the file); tandem-review's
    // `diff_texts` strips them all.
    let keeps_eol = f.hunks.iter().flat_map(|h| &h.lines).any(|l| l.text.ends_with('\n'));
    for h in &f.hunks {
        let _ = writeln!(s, "@@ -{},{} +{},{} @@", h.old_start, h.old_lines, h.new_start, h.new_lines);
        for l in &h.lines {
            let sign = match l.kind {
                LineKind::Context => ' ',
                LineKind::Added => '+',
                LineKind::Removed => '-',
            };
            match l.text.strip_suffix('\n') {
                Some(t) => {
                    let _ = writeln!(s, "{sign}{}", t.strip_suffix('\r').unwrap_or(t));
                }
                None if !keeps_eol => {
                    let _ = writeln!(s, "{sign}{}", l.text);
                }
                None => {
                    let _ = writeln!(s, "{sign}{}\n\\ No newline at end of file", l.text);
                }
            }
        }
    }
    s
}

/// (+added, −removed) line counts of one file.
pub fn line_counts(f: &FileDiff) -> (usize, usize) {
    let lines = f.hunks.iter().flat_map(|h| &h.lines);
    lines.fold((0, 0), |(a, d), l| match l.kind {
        LineKind::Added => (a + 1, d),
        LineKind::Removed => (a, d + 1),
        LineKind::Context => (a, d),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_has_headers_and_hunks() {
        let f = tandem_review::diff_texts("src/a.rs", Some("a\nb\n"), Some("a\nB\nc\n"));
        let p = to_unified_patch(&f);
        assert!(p.starts_with("diff --git a/src/a.rs b/src/a.rs"));
        assert!(p.contains("@@ -1,2 +1,3 @@"), "{p}");
        assert_eq!(line_counts(&f), (2, 1));
        let git_style = tandem_core::FileDiff {
            hunks: vec![tandem_core::Hunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 2,
                lines: vec![
                    tandem_core::DiffLine { kind: LineKind::Context, old_no: Some(1), new_no: Some(1), text: "a\n".into() },
                    tandem_core::DiffLine { kind: LineKind::Added, old_no: None, new_no: Some(2), text: "b".into() },
                ],
            }],
            ..f
        };
        let p = to_unified_patch(&git_style);
        assert!(p.ends_with(" a\n+b\n\\ No newline at end of file\n"), "{p:?}");
    }
}
