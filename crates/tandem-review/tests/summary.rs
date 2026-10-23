mod common;
use common::*;
use tandem_review::{describe, diff_texts};

fn has(risky: &[String], needle: &str) -> bool { risky.iter().any(|r| r.contains(needle)) }

#[test]
fn counts_and_symbols() {
    let d = set("t", "a", vec![
        modify("src/client.rs", RS, &replace_line(RS, 31, "    43")),
        modify("py/repo.py", PY, &insert_after(PY, 12, "            # x")),
    ]);
    let s = describe(&d);
    assert_eq!((s.files, s.additions, s.deletions), (2, 2, 1));
    assert!(s.touched_symbols.contains(&"src/client.rs: helper".to_string()), "{:?}", s.touched_symbols);
    assert!(s.touched_symbols.contains(&"py/repo.py: Repo.files".to_string()), "{:?}", s.touched_symbols);
    assert!(s.risky.is_empty(), "{:?}", s.risky);
}

#[test]
fn risky_flags() {
    let big: String = (0..250).map(|i| format!("l{i}\n")).collect();
    let d = set("t", "a", vec![
        modify("Cargo.lock", "a\n", "b\n"),
        modify("web/pnpm-lock.yaml", "a\n", "b\n"),
        modify(".github/workflows/ci.yml", "a\n", "b\n"),
        diff_texts("db/migrations/0003_add_users.sql", None, Some("create table x();\n")),
        modify("src/auth/session.rs", "a\n", "b\n"),
        modify(".env.local", "A=1\n", "A=2\n"),
        diff_texts("tests/api_test.py", Some("def test_a():\n    pass\n"), None),
        modify("src/lib.rs", "#[test]\nfn t() {}\nfn x() {}\n", "fn x() {}\n"),
        diff_texts("src/huge.rs", Some(&big), None),
    ]);
    let r = describe(&d).risky;
    for needle in [
        "lockfile changed: Cargo.lock",
        "lockfile changed: web/pnpm-lock.yaml",
        "CI config changed: .github/workflows/ci.yml",
        "migration changed: db/migrations/0003_add_users.sql",
        "secret-looking file changed: src/auth/session.rs",
        "secret-looking file changed: .env.local",
        "test file deleted: tests/api_test.py",
        "1 test(s) removed: src/lib.rs",
        "large deletion: 250 lines in src/huge.rs",
    ] {
        assert!(has(&r, needle), "missing {needle:?} in {r:#?}");
    }
}

#[test]
fn benign_paths_are_not_flagged() {
    let d = set("t", "a", vec![
        modify("src/tokenizer.rs", "a\n", "b\n"),
        modify("src/author_list.ts", "a\n", "b\n"),
        modify("README.md", "a\n", "b\n"),
    ]);
    let r = describe(&d).risky;
    assert!(r.is_empty(), "{r:?}");
}

#[test]
fn summary_serde_round_trip() {
    let s = describe(&set("t", "a", vec![modify("Cargo.lock", "a\n", "b\n")]));
    let back: tandem_review::ReviewSummary = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back, s);
}
