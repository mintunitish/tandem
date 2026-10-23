mod common;
use common::*;
use tandem_core::FileChange;
use tandem_review::{Decision, HunkRef, ReviewState, diff_texts};

fn fixture() -> tandem_core::DiffSet {
    let rs_new = replace_line(&replace_line(RS, 31, "    43"), 15, "        let full = format!(\"{}/v2/{}\", self.url, path);");
    let ts_new = insert_after(&replace_line(TS, 3, "export const DEFAULT_LIMIT = 100;"), 28, "  // trailing");
    let py_new = delete_lines(PY, 18, 20);
    set("t1", "claude", vec![
        modify("src/client.rs", RS, &rs_new),
        modify("web/user.ts", TS, &ts_new),
        modify("py/repo.py", PY, &py_new),
        diff_texts("NEW.md", None, Some("hello\nworld\n")),
        diff_texts("old.txt", Some("bye\n"), None),
    ])
}

#[test]
fn all_accepted_materializes_full_new_text() {
    let d = fixture();
    let mut st = ReviewState::new(d.clone());
    st.set_all(Decision::Accepted);
    let files = st.accepted_files();
    assert_eq!(files.len(), d.files.len());
    for (a, orig) in files.iter().zip(&d.files) {
        assert_eq!(a.new_text, orig.new_text, "{}", orig.path.display());
        assert_eq!(a.change, orig.change);
    }
}

#[test]
fn materialization_handles_missing_trailing_newline_and_crlf() {
    let cases = [
        ("a\nb\nc", "a\nB\nc"),
        ("a\nb\nc\n", "a\nb\nc\nd"),
        ("a\nb\nc", "a\nb\nc\n"),
        ("x\r\ny\r\nz\r\n", "x\r\nY\r\nz\r\nw\r\n"),
        ("", "only\n"),
    ];
    for (old, new) in cases {
        let mut st = ReviewState::new(set("t", "a", vec![modify("f.txt", old, new)]));
        st.set_all(Decision::Accepted);
        assert_eq!(st.accepted_files()[0].new_text.as_deref(), Some(new), "{old:?} -> {new:?}");
    }
}

#[test]
fn partial_accept_applies_only_accepted_hunks() {
    let rs_new = replace_line(&replace_line(RS, 31, "    43"), 3, "pub const MAX_RETRIES: u32 = 5;");
    let d = set("t", "a", vec![modify("c.rs", RS, &rs_new)]);
    assert_eq!(d.files[0].hunks.len(), 2);
    let mut st = ReviewState::new(d);
    st.accept(HunkRef { file: 0, hunk: 1 });
    st.reject(HunkRef { file: 0, hunk: 0 });
    let out = st.accepted_files();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].hunks.len(), 1);
    assert_eq!(out[0].new_text.as_deref(), Some(replace_line(RS, 31, "    43").as_str()));
}

#[test]
fn nothing_accepted_yields_nothing_and_partial_delete_becomes_modify() {
    let mut st = ReviewState::new(fixture());
    assert!(st.accepted_files().is_empty());
    st.set_all(Decision::Rejected);
    assert!(st.accepted_files().is_empty());

    // A deleted file whose deletion is split into two hunks (simulated by
    // a two-hunk deletion fixture) and only one accepted.
    let old: String = (1..=20).map(|i| format!("l{i}\n")).collect();
    let mut f = diff_texts("gone.txt", Some(&old), None);
    // Split the single deletion hunk into two hunks of 10 lines each.
    let h = f.hunks.remove(0);
    let (first, second) = h.lines.split_at(10);
    let mk = |lines: &[tandem_core::DiffLine], start: u32| tandem_core::Hunk {
        old_start: start, old_lines: lines.len() as u32, new_start: 0, new_lines: 0, lines: lines.to_vec(),
    };
    f.hunks = vec![mk(first, 1), mk(second, 11)];
    let mut st = ReviewState::new(set("t", "a", vec![f]));
    st.accept(HunkRef { file: 0, hunk: 0 });
    let out = st.accepted_files();
    assert_eq!(out[0].change, FileChange::Modified);
    let expect: String = (11..=20).map(|i| format!("l{i}\n")).collect();
    assert_eq!(out[0].new_text.as_deref(), Some(expect.as_str()));
}

#[test]
fn navigation_and_counts() {
    let mut st = ReviewState::new(fixture());
    let all = st.changes();
    let c = st.counts();
    assert_eq!(c.total, all.len());
    assert_eq!(c.pending, c.total);
    assert_eq!(st.cursor(), Some(all[0]));
    assert_eq!(st.next_change(), Some(all[1]));
    assert_eq!(st.prev_change(), Some(all[0]));
    assert_eq!(st.prev_change(), Some(*all.last().unwrap()), "prev wraps");
    assert_eq!(st.next_change(), Some(all[0]), "next wraps");
    // Crosses file boundary: file 0 has 2 hunks.
    st.next_change();
    assert_eq!(st.next_change(), Some(HunkRef { file: 1, hunk: 0 }));

    st.set_all(Decision::Accepted);
    st.set(all[3], Decision::Pending);
    st.reject(all[0]);
    let c = st.counts();
    assert_eq!((c.pending, c.rejected, c.accepted), (1, 1, c.total - 2));
    assert_eq!(st.next_pending(), Some(all[3]));
    st.accept(all[3]);
    assert_eq!(st.next_pending(), None);
    assert!(st.is_complete());
    assert!(!st.set(HunkRef { file: 99, hunk: 0 }, Decision::Accepted));
}

#[test]
fn serde_round_trip_and_persistence() {
    let mut st = ReviewState::new(fixture());
    st.accept(HunkRef { file: 0, hunk: 1 });
    st.reject(HunkRef { file: 2, hunk: 0 });
    st.next_change();
    let back = ReviewState::from_json(&st.to_json().unwrap()).unwrap();
    assert_eq!(back.counts(), st.counts());
    assert_eq!(back.cursor(), st.cursor());
    assert_eq!(back.decision(HunkRef { file: 0, hunk: 1 }), Some(Decision::Accepted));
    assert_eq!(back.decision(HunkRef { file: 2, hunk: 0 }), Some(Decision::Rejected));

    let dir = tempfile::tempdir().unwrap();
    let path = tandem_review::state::review_path(dir.path(), &"t1".into());
    assert!(path.ends_with(".tandem/tasks/t1/review.json"));
    st.save(&path).unwrap();
    let loaded = ReviewState::load(&path).unwrap();
    assert_eq!(loaded.to_json().unwrap(), st.to_json().unwrap());

    // Mismatched decisions are rejected.
    let mut v: serde_json::Value = serde_json::from_str(&st.to_json().unwrap()).unwrap();
    v["decisions"][0] = serde_json::json!([]);
    assert!(ReviewState::from_json(&v.to_string()).is_err());
}

#[test]
fn materialization_falls_back_to_hunk_text_without_new_text() {
    let new = replace_line(PY, 12, "        for name in sorted(os.listdir(self.path)):");
    let mut f = modify("r.py", PY, &new);
    f.new_text = None;
    let mut st = ReviewState::new(set("t", "a", vec![f]));
    st.set_all(Decision::Accepted);
    assert_eq!(st.accepted_files()[0].new_text.as_deref(), Some(new.as_str()));
}
