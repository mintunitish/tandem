mod common;
use common::*;
use tandem_review::symbols::{Lang, LineRange, ParsedSource, SymbolKind, enclosing_symbols, hunk_symbols};

fn names(lang: Lang, src: &str, a: u32, b: u32) -> Vec<(String, SymbolKind)> {
    enclosing_symbols(lang, src, LineRange::new(a, b)).into_iter().map(|s| (s.qualified(), s.kind)).collect()
}

#[test]
fn language_detection() {
    assert_eq!(Lang::from_path("a/b.rs"), Some(Lang::Rust));
    assert_eq!(Lang::from_path("x.ts"), Some(Lang::TypeScript));
    assert_eq!(Lang::from_path("x.TSX"), Some(Lang::Tsx));
    assert_eq!(Lang::from_path("x.mjs"), Some(Lang::JavaScript));
    assert_eq!(Lang::from_path("x.jsx"), Some(Lang::JavaScript));
    assert_eq!(Lang::from_path("x.py"), Some(Lang::Python));
    assert_eq!(Lang::from_path("x.go"), Some(Lang::Go));
    assert_eq!(Lang::from_path("Makefile"), None);
    assert_eq!(Lang::from_path("x.md"), None);
}

#[test]
fn rust_symbols() {
    use SymbolKind::*;
    assert_eq!(names(Lang::Rust, RS, 3, 3), vec![("MAX_RETRIES".into(), Const)]);
    assert_eq!(names(Lang::Rust, RS, 18, 19), vec![("Client.fetch".into(), Method)]);
    assert_eq!(names(Lang::Rust, RS, 31, 31), vec![("helper".into(), Fn)]);
    assert_eq!(names(Lang::Rust, RS, 6, 6), vec![("Client".into(), Class)]);
    // Spanning two fns: both, no outer noise.
    assert_eq!(names(Lang::Rust, RS, 27, 31), vec![("parse_config".into(), Fn), ("helper".into(), Fn)]);
    // Blank line between items inside impl → the impl itself.
    assert_eq!(names(Lang::Rust, RS, 13, 13), vec![("Client".into(), Class)]);
    assert!(names(Lang::Rust, RS, 1, 2).is_empty());
}

#[test]
fn typescript_and_tsx_symbols() {
    use SymbolKind::*;
    for lang in [Lang::TypeScript, Lang::Tsx] {
        assert_eq!(names(lang, TS, 3, 3), vec![("DEFAULT_LIMIT".into(), Const)]);
        assert_eq!(names(lang, TS, 6, 6), vec![("User".into(), Class)]);
        assert_eq!(names(lang, TS, 14, 14), vec![("UserService.load".into(), Method)]);
        assert_eq!(names(lang, TS, 19, 19), vec![("UserService.save".into(), Method)]);
        assert_eq!(names(lang, TS, 24, 24), vec![("formatUser".into(), Fn)]);
        assert_eq!(names(lang, TS, 28, 28), vec![("internal".into(), Fn)]);
    }
    let tsx = "export function App() {\n  return <div>hi</div>;\n}\n";
    assert_eq!(names(Lang::Tsx, tsx, 2, 2), vec![("App".into(), Fn)]);
}

#[test]
fn javascript_symbols() {
    use SymbolKind::*;
    let js = "const LIMIT = 3;\nclass A {\n  run() {\n    return 1;\n  }\n}\nfunction b() {\n  return 2;\n}\n";
    assert_eq!(names(Lang::JavaScript, js, 1, 1), vec![("LIMIT".into(), Const)]);
    assert_eq!(names(Lang::JavaScript, js, 4, 4), vec![("A.run".into(), Method)]);
    assert_eq!(names(Lang::JavaScript, js, 8, 8), vec![("b".into(), Fn)]);
}

#[test]
fn python_symbols() {
    use SymbolKind::*;
    assert_eq!(names(Lang::Python, PY, 3, 3), vec![("TIMEOUT".into(), Const)]);
    assert_eq!(names(Lang::Python, PY, 12, 13), vec![("Repo.files".into(), Method)]);
    assert_eq!(names(Lang::Python, PY, 8, 8), vec![("Repo.__init__".into(), Method)]);
    assert_eq!(names(Lang::Python, PY, 18, 18), vec![("load_settings".into(), Fn)]);
}

#[test]
fn go_symbols() {
    use SymbolKind::*;
    assert_eq!(names(Lang::Go, GO, 5, 5), vec![("Version".into(), Const)]);
    assert_eq!(names(Lang::Go, GO, 8, 8), vec![("Server".into(), Class)]);
    assert_eq!(names(Lang::Go, GO, 12, 12), vec![("Start".into(), Method)]);
    assert_eq!(names(Lang::Go, GO, 17, 17), vec![("Greet".into(), Fn)]);
}

#[test]
fn hunk_symbols_use_both_sides() {
    // New function added at end + edit inside fetch.
    let new = replace_line(RS, 20, "        out.push_str(\"&mode=slow\");") + "\npub fn extra() {}\n";
    let f = modify("c.rs", RS, &new);
    let per_hunk = hunk_symbols(&f);
    let all: Vec<String> = per_hunk.iter().flatten().map(|s| s.qualified()).collect();
    assert!(all.contains(&"Client.fetch".to_string()), "{all:?}");
    assert!(all.contains(&"extra".to_string()), "{all:?}");
    // Deletion resolves against the old text.
    let del = modify("c.rs", RS, &delete_lines(RS, 30, 32));
    let all: Vec<String> = hunk_symbols(&del).into_iter().flatten().map(|s| s.qualified()).collect();
    assert!(all.contains(&"helper".to_string()), "{all:?}");
    // Unknown language → empty.
    assert!(hunk_symbols(&modify("x.txt", "a\n", "b\n"))[0].is_empty());
    assert!(ParsedSource::parse(Lang::Rust, RS).unwrap().symbols().len() >= 7);
}
