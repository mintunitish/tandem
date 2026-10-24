//! Review model helpers: per-file line counts of a `tandem_core::FileDiff`.

use tandem_core::{FileDiff, LineKind};

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
        let p = f.to_patch();
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
        let p = git_style.to_patch();
        assert!(p.ends_with(" a\n+b\n\\ No newline at end of file\n"), "{p:?}");
    }
}
