mod common;
use common::*;
use tandem_core::{DiffSet, FileChange, FileDiff};
use tandem_review::{ConflictKind, IntentConflict, Severity, TaskMeta, detect, diff_texts};

fn rs_edit(line: usize, text: &str) -> FileDiff { modify("src/client.rs", RS, &replace_line(RS, line, text)) }

fn run(a: Vec<FileDiff>, b: Vec<FileDiff>) -> Vec<IntentConflict> {
    let tasks: Vec<(TaskMeta, DiffSet)> = vec![
        (meta("ta", "claude", "Add v2 API paths\nmore detail"), set("ta", "claude", a)),
        (meta("tb", "codex", "Switch to slow mode"), set("tb", "codex", b)),
    ];
    detect(&tasks)
}

fn kinds(c: &[IntentConflict]) -> Vec<(Severity, ConflictKind)> { c.iter().map(|c| (c.severity, c.kind)).collect() }

#[test]
fn overlapping_hunks_are_hard() {
    let c = run(vec![rs_edit(19, "        // a")], vec![rs_edit(20, "        // b")]);
    assert_eq!(kinds(&c), vec![(Severity::Hard, ConflictKind::HunkOverlap)]);
    let c = &c[0];
    assert_eq!(c.a.intent, "Add v2 API paths");
    assert_eq!(c.b.intent, "Switch to slow mode");
    assert_eq!(c.a.agent.as_str(), "claude");
    assert_eq!(c.b.task.as_str(), "tb");
    assert_eq!(c.file.to_str(), Some("src/client.rs"));
    assert_eq!(c.symbol.as_deref(), Some("Client.fetch"));
    assert_eq!((c.a.ranges[0].start, c.b.ranges[0].start), (19, 20));
    assert!(c.explanation.contains("claude") && c.explanation.contains("codex"), "{}", c.explanation);
}

#[test]
fn adjacency_threshold_is_three_lines() {
    // Lines 15 and 19: three untouched lines between → adjacent → Hard.
    let c = run(vec![rs_edit(15, "        // a")], vec![rs_edit(19, "        // b")]);
    assert_eq!(kinds(&c), vec![(Severity::Hard, ConflictKind::HunkOverlap)]);
    // Lines 15 and 20: four lines apart → not adjacent, but same method → Soft.
    let c = run(vec![rs_edit(15, "        // a")], vec![rs_edit(20, "        // b")]);
    assert_eq!(kinds(&c), vec![(Severity::Soft, ConflictKind::SameSymbol)]);
    assert_eq!(c[0].symbol.as_deref(), Some("Client.fetch"));
}

#[test]
fn both_adding_same_new_file_is_hard() {
    let c = run(vec![diff_texts("NOTES.md", None, Some("a\n"))], vec![diff_texts("NOTES.md", None, Some("b\n"))]);
    assert_eq!(kinds(&c), vec![(Severity::Hard, ConflictKind::HunkOverlap)]);
}

#[test]
fn far_apart_different_symbols_do_not_conflict() {
    assert!(run(vec![rs_edit(3, "pub const MAX_RETRIES: u32 = 9;")], vec![rs_edit(31, "    7")]).is_empty());
    // TS: different methods of the same class, 4+ lines apart.
    let a = modify("web/user.ts", TS, &replace_line(TS, 14, "    const res = await api.get(`${this.base}/u/${id}`);"));
    let b = modify("web/user.ts", TS, &replace_line(TS, 19, "    api.put(`${this.base}/users`, user);"));
    assert!(run(vec![a], vec![b]).is_empty());
    // Different files entirely.
    assert!(run(vec![rs_edit(15, "// a")], vec![modify("py/repo.py", PY, &replace_line(PY, 3, "TIMEOUT = 5"))]).is_empty());
}

#[test]
fn same_symbol_soft_in_python() {
    let a = modify("py/repo.py", PY, &replace_line(PY, 11, "        out = list()"));
    let b = modify("py/repo.py", PY, &insert_after(PY, 14, "        # done"));
    // line 11 vs insertion after 14: lines 12,13,14 untouched → adjacent → Hard.
    assert_eq!(kinds(&run(vec![a.clone()], vec![b])), vec![(Severity::Hard, ConflictKind::HunkOverlap)]);
    // Pad the method so two edits in it are more than three lines apart.
    let long = PY.replace("            out.append(name)\n", "            out.append(name)\n            out.append(name)\n            out.append(name)\n            out.append(name)\n");
    let a = modify("py/repo.py", &long, &replace_line(&long, 11, "        out = list()"));
    let b = modify("py/repo.py", &long, &replace_line(&long, 17, "        return sorted(out)"));
    let c = run(vec![a], vec![b]);
    assert_eq!(kinds(&c), vec![(Severity::Soft, ConflictKind::SameSymbol)]);
    assert_eq!(c[0].symbol.as_deref(), Some("Repo.files"));
}

#[test]
fn delete_and_rename_vs_edit_are_hard() {
    let del = diff_texts("src/client.rs", Some(RS), None);
    let c = run(vec![del.clone()], vec![rs_edit(31, "    7")]);
    assert_eq!(kinds(&c), vec![(Severity::Hard, ConflictKind::DeleteVsEdit)]);
    assert!(c[0].explanation.contains("deletes"), "{}", c[0].explanation);

    let mut ren = diff_texts("src/http.rs", Some(RS), Some(RS));
    ren.change = FileChange::Renamed;
    ren.old_path = Some("src/client.rs".into());
    let c = run(vec![rs_edit(3, "// x")], vec![ren.clone()]);
    assert_eq!(kinds(&c), vec![(Severity::Hard, ConflictKind::RenameVsEdit)]);
    assert_eq!(c[0].file.to_str(), Some("src/client.rs"));

    // Both delete: same intent, no conflict. Both rename to the same target: none.
    assert!(run(vec![del.clone()], vec![del]).is_empty());
    assert!(run(vec![ren.clone()], vec![ren]).is_empty());
}

#[test]
fn definition_vs_new_call_site_is_info() {
    let a = rs_edit(27, "    input.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()");
    let main = "fn main() {\n}\n";
    let b = modify("src/main.rs", main, "fn main() {\n    let cfg = client::parse_config(\"x\");\n}\n");
    let c = run(vec![a.clone()], vec![b.clone()]);
    assert_eq!(kinds(&c), vec![(Severity::Info, ConflictKind::DefinitionVsUse)]);
    assert_eq!(c[0].symbol.as_deref(), Some("parse_config"));
    assert_eq!(c[0].a.task.as_str(), "ta");
    assert_eq!(c[0].b.path.to_str(), Some("src/main.rs"));
    assert_eq!(c[0].b.ranges.len(), 1);
    // Reverse order of tasks: still detected, def side is the editing task.
    let c = run(vec![b], vec![a]);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].a.task.as_str(), "tb");
    // Mention only as a substring (parse_config_v2) does not count.
    let b2 = modify("src/main.rs", main, "fn main() {\n    parse_config_v2();\n}\n");
    assert!(run(vec![rs_edit(27, "    todo!()")], vec![b2]).is_empty());
}

#[test]
fn same_task_and_single_task_produce_nothing() {
    let d = set("ta", "claude", vec![rs_edit(19, "// a")]);
    let m = meta("ta", "claude", "x");
    assert!(detect(&[(m.clone(), d.clone()), (m.clone(), d.clone())]).is_empty());
    assert!(detect(&[(m, d)]).is_empty());
}

#[test]
fn conflicts_serialize() {
    let c = run(vec![rs_edit(19, "// a")], vec![rs_edit(20, "// b")]);
    let s = serde_json::to_string(&c).unwrap();
    let back: Vec<IntentConflict> = serde_json::from_str(&s).unwrap();
    assert_eq!(back, c);
}
