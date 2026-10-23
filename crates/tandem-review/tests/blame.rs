mod common;
use common::*;
use tandem_core::Decider;
use tandem_review::blame::{BlameIndex, blame_entries, blame_path, merge_blame};
use tandem_review::diff_texts;

fn base() -> String { (1..=20).map(|i| format!("line {i}\n")).collect() }

/// Task A adds three lines after line 10 → entry at 11..=13.
fn first() -> (String, BlameIndex) {
    let b = base();
    let v1 = insert_after(&insert_after(&insert_after(&b, 10, "a1"), 11, "a2"), 12, "a3");
    let d = set("ta", "claude", vec![modify("f.rs", &b, &v1)]);
    let mut m = meta("ta", "claude", &"x".repeat(200));
    m.permission_by = Some(Decider::User);
    let e = blame_entries(&d, &m);
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].new_start, e[0].new_lines), (11, 3));
    assert_eq!(e[0].prompt_excerpt.chars().count(), 120);
    assert_eq!(e[0].permission_by, Some(Decider::User));
    (v1, BlameIndex::new(e))
}

fn a_entry(idx: &BlameIndex) -> (u32, u32) {
    let e = idx.entries().iter().find(|e| e.task.as_str() == "ta").unwrap();
    (e.new_start, e.new_lines)
}

#[test]
fn insert_above_shifts_down() {
    let (v1, mut idx) = first();
    let v2 = insert_after(&insert_after(&v1, 2, "b1"), 3, "b2");
    idx.apply(&set("tb", "codex", vec![modify("f.rs", &v1, &v2)]), &meta("tb", "codex", "B"));
    assert_eq!(a_entry(&idx), (13, 3));
    assert_eq!(idx.lookup("f.rs", 3).unwrap().agent.as_str(), "codex");
    assert_eq!(idx.lookup("f.rs", 13).unwrap().agent.as_str(), "claude");
    assert_eq!(idx.lookup("f.rs", 15).unwrap().agent.as_str(), "claude");
    assert!(idx.lookup("f.rs", 16).is_none());
    assert!(idx.lookup("f.rs", 12).is_none());
}

#[test]
fn delete_above_shifts_up() {
    let (v1, mut idx) = first();
    let v2 = delete_lines(&v1, 1, 4);
    idx.apply(&set("tb", "codex", vec![modify("f.rs", &v1, &v2)]), &meta("tb", "codex", "B"));
    assert_eq!(a_entry(&idx), (7, 3));
    assert_eq!(idx.entries().len(), 1, "pure deletion adds no entries");
}

#[test]
fn change_below_does_not_shift() {
    let (v1, mut idx) = first();
    let v2 = insert_after(&delete_lines(&v1, 18, 19), 20, "tail");
    idx.apply(&set("tb", "codex", vec![modify("f.rs", &v1, &v2)]), &meta("tb", "codex", "B"));
    assert_eq!(a_entry(&idx), (11, 3));
}

#[test]
fn overwriting_middle_line_splits_entry() {
    let (v1, mut idx) = first();
    let v2 = replace_line(&v1, 12, "b-replaced");
    idx.apply(&set("tb", "codex", vec![modify("f.rs", &v1, &v2)]), &meta("tb", "codex", "B"));
    let who: Vec<&str> = (11..=13).map(|l| idx.lookup("f.rs", l).unwrap().agent.as_str()).collect();
    assert_eq!(who, ["claude", "codex", "claude"]);
}

#[test]
fn delete_rename_and_other_files() {
    let (v1, idx) = first();
    // Deleting the file drops its entries.
    let out = merge_blame(idx.entries().to_vec(), &set("tb", "c", vec![diff_texts("f.rs", Some(&v1), None)]), &meta("tb", "c", "B"));
    assert!(out.is_empty());
    // Renaming moves them.
    let mut ren = diff_texts("g.rs", Some(&v1), Some(&v1));
    ren.change = tandem_core::FileChange::Renamed;
    ren.old_path = Some("f.rs".into());
    let out = merge_blame(idx.entries().to_vec(), &set("tb", "c", vec![ren]), &meta("tb", "c", "B"));
    assert_eq!(out[0].path.to_str(), Some("g.rs"));
    assert_eq!(out[0].new_start, 11);
    // Unrelated file: untouched.
    let out = merge_blame(idx.entries().to_vec(), &set("tb", "c", vec![modify("h.rs", "a\n", "b\n")]), &meta("tb", "c", "B"));
    assert_eq!(out.len(), 2);
    assert_eq!(BlameIndex::new(out).lookup("f.rs", 11).unwrap().task.as_str(), "ta");
}

#[test]
fn persistence_round_trip() {
    let (_, idx) = first();
    let dir = tempfile::tempdir().unwrap();
    let p = blame_path(dir.path());
    assert!(p.ends_with(".tandem/blame.json"));
    assert!(BlameIndex::load_or_default(&p).unwrap().entries().is_empty());
    idx.save(&p).unwrap();
    let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert!(raw.is_array());
    let back = BlameIndex::load(&p).unwrap();
    assert_eq!(serde_json::to_string(&back).unwrap(), serde_json::to_string(&idx).unwrap());
    assert_eq!(back.lookup("f.rs", 12).unwrap().task.as_str(), "ta");
}
