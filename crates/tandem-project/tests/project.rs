mod common;

use common::{fixture, write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tandem_project::*;

fn names(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|e| e.name.as_str()).collect()
}

// ---------------------------------------------------------------- tree

#[test]
fn tree_order_and_ignores() {
    let dir = fixture();
    let mut tree = FileTree::new(dir.path());
    let root = dir.path().to_path_buf();
    let kids = tree.children(&root).to_vec();
    assert_eq!(
        names(&kids),
        // dirs first (case-insensitive), then files (case-insensitive, dotfiles first)
        ["app", "cmd", "crates", "node_modules", "src", ".gitignore", "apple.txt", "Banana.txt", "package.json", "README.md", "Zebra.txt"]
    );
    let nm = kids.iter().find(|e| e.name == "node_modules").unwrap();
    assert!(nm.is_dir && nm.ignored, "node_modules is visible but flagged");
    assert!(!names(&kids).contains(&"dist"), "gitignored dir hidden");
    assert!(!names(&kids).contains(&"debug.log"), "gitignored file hidden");
    assert!(!names(&kids).contains(&".git") && !names(&kids).contains(&"target"));

    // lazy: subdirs not loaded until expanded
    assert!(tree.loaded_children(&root.join("src")).is_none());
    let src = tree.expand(&root.join("src")).to_vec();
    assert_eq!(names(&src), ["components", "index.ts", "util.js"]);
    let rows = tree.visible_rows();
    let src_row = rows.iter().position(|r| r.entry.name == "src").unwrap();
    assert!(rows[src_row].expanded);
    assert_eq!(rows[src_row + 1].entry.name, "components");
    assert_eq!(rows[src_row + 1].depth, 1);
    tree.collapse(&root.join("src"));
    assert!(tree.visible_rows().iter().all(|r| r.entry.name != "index.ts"));
}

#[test]
fn tree_refresh_picks_up_changes() {
    let dir = fixture();
    let mut tree = FileTree::new(dir.path());
    let root = dir.path().to_path_buf();
    tree.expand(&root.join("src"));
    write(dir.path(), "src/Alpha.ts", "");
    assert!(!names(tree.loaded_children(&root.join("src")).unwrap()).contains(&"Alpha.ts"));
    tree.refresh(&root.join("src"));
    assert_eq!(names(tree.loaded_children(&root.join("src")).unwrap()), ["components", "Alpha.ts", "index.ts", "util.js"]);
}

#[test]
fn icons() {
    let cases = [
        ("a.ts", "ts"),
        ("A.tsx", "tsx"),
        ("a.rs", "rs"),
        ("a.py", "py"),
        ("a.json", "json"),
        ("README.md", "md"),
        (".gitignore", "git"),
        ("Cargo.lock", "lock"),
        ("package-lock.json", "lock"),
        ("logo.PNG", "image"),
        ("Cargo.toml", "config"),
        (".env.local", "config"),
        ("vite.config.ts", "config"),
        ("weird.xyz", "file"),
    ];
    for (n, want) in cases {
        assert_eq!(icon_for(Path::new(n)), want, "{n}");
    }
    assert_eq!(icon_for_entry("src", true, false), "folder");
    assert_eq!(icon_for_entry("src", true, true), "folder-open");
    let dir = fixture();
    assert_eq!(icon_for(&dir.path().join("src")), "folder");
}

// ---------------------------------------------------------------- watcher

fn wait_for(rx: &tandem_core::Receiver<FsEvent>, what: &str, pred: impl Fn(&FsEvent) -> bool) -> FsEvent {
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut seen = vec![];
    while Instant::now() < deadline {
        if let Ok(ev) = rx.try_recv() {
            if pred(&ev) {
                return ev;
            }
            seen.push(ev);
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    panic!("no {what} event within 1.5s; saw {seen:?}");
}

#[test]
fn watcher_create_modify_delete_and_tree_follows() {
    let dir = fixture();
    let w = Watcher::new(dir.path()).unwrap();
    let rx = w.events();
    let root = w.root().to_path_buf();
    let mut tree = FileTree::new(&root);
    tree.expand(&root.join("src"));
    std::thread::sleep(Duration::from_millis(400)); // let FSEvents settle past fixture creation
    while rx.try_recv().is_ok() {}

    let f = root.join("src/Fresh.ts");
    std::fs::write(&f, "export {}").unwrap();
    let ev = wait_for(&rx, "create", |e| e.paths.contains(&f) && e.kind == FsEventKind::Created);
    tree.apply_event(&ev);
    assert!(names(tree.loaded_children(&root.join("src")).unwrap()).contains(&"Fresh.ts"));

    std::thread::sleep(Duration::from_millis(250));
    while rx.try_recv().is_ok() {}
    std::fs::write(&f, "export const a = 1;").unwrap();
    wait_for(&rx, "modify", |e| e.paths.contains(&f) && e.kind == FsEventKind::Modified);

    std::thread::sleep(Duration::from_millis(250));
    while rx.try_recv().is_ok() {}
    std::fs::remove_file(&f).unwrap();
    let ev = wait_for(&rx, "remove", |e| e.paths.contains(&f) && e.kind == FsEventKind::Removed);
    tree.apply_event(&ev);
    assert!(!names(tree.loaded_children(&root.join("src")).unwrap()).contains(&"Fresh.ts"));
}

#[test]
fn watcher_ignores_hidden_set_and_pairs_renames() {
    let dir = fixture();
    let w = Watcher::new(dir.path()).unwrap();
    let rx = w.events();
    let root = w.root().to_path_buf();
    std::thread::sleep(Duration::from_millis(400));
    while rx.try_recv().is_ok() {}

    std::fs::write(root.join("node_modules/pkg/new.js"), "x").unwrap();
    std::fs::write(root.join("target/debug/new.rs"), "x").unwrap();
    std::fs::write(root.join(".git/HEAD"), "x").unwrap();
    let visible = root.join("src/visible.ts");
    std::fs::write(&visible, "x").unwrap();
    let mut got = vec![];
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline && !got.iter().any(|e: &FsEvent| e.paths.contains(&visible)) {
        if let Ok(e) = rx.try_recv() {
            got.push(e);
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(got.iter().any(|e| e.paths.contains(&visible)), "{got:?}");
    for e in &got {
        for p in &e.paths {
            let s = p.to_string_lossy();
            assert!(!s.contains("node_modules") && !s.contains("/target/") && !s.contains("/.git/"), "leaked {s}");
        }
    }

    std::thread::sleep(Duration::from_millis(250));
    while rx.try_recv().is_ok() {}
    let to = root.join("src/renamed.ts");
    std::fs::rename(&visible, &to).unwrap();
    let ev = wait_for(&rx, "rename", |e| e.kind == FsEventKind::Renamed && e.paths.contains(&to));
    assert!(ev.paths.contains(&visible) || ev.paths.len() == 1, "{ev:?}");
}

// ---------------------------------------------------------------- search

fn rel(root: &Path, m: &Match) -> String {
    m.path.strip_prefix(root).unwrap().to_string_lossy().into_owned()
}

#[test]
fn search_literal_respects_ignores_and_reports_ranges() {
    let dir = fixture();
    let r = dir.path();
    let hits = find_in_files_collect(r, "TODO", &SearchOptions::default()).unwrap();
    let files: Vec<String> = hits.iter().map(|m| rel(r, m)).collect();
    // case-insensitive by default; ignores: debug.log, dist, node_modules, target
    assert_eq!(files, ["README.md", "app/service.py", "src/index.ts", "src/util.js"]);
    let ts = hits.iter().find(|m| rel(r, m) == "src/index.ts").unwrap();
    assert_eq!(ts.line_no, 2);
    assert_eq!(ts.col, 4);
    assert_eq!(ts.line_text, "// TODO(alice): wire up");
    assert_eq!(ts.ranges, vec![3..7]);

    let cased = find_in_files_collect(r, "TODO", &SearchOptions { case: true, ..Default::default() }).unwrap();
    assert!(cased.iter().all(|m| rel(r, m) != "src/util.js"));
    assert_eq!(cased.len(), 3);
}

#[test]
fn search_regex_word_and_globs() {
    let dir = fixture();
    let r = dir.path();
    let re = SearchOptions { regex: true, case: true, ..Default::default() };
    let hits = find_in_files_collect(r, r"TODO\((\w+)\)", &re).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(rel(r, &hits[0]), "src/index.ts");
    // a literal query containing regex metacharacters is escaped
    let lit = find_in_files_collect(r, "TODO(alice)", &SearchOptions::default()).unwrap();
    assert_eq!(lit.len(), 1);
    assert!(find_in_files_collect(r, "(unclosed", &re).is_err());

    // whole word: `Foo` does not match FooBarController
    let word = SearchOptions { whole_word: true, case: true, ..Default::default() };
    assert!(find_in_files_collect(r, "Foo", &word).unwrap().iter().all(|m| rel(r, m) == "crates/core/src/lib.rs"));
    let sub = find_in_files_collect(r, "Foo", &SearchOptions { case: true, ..Default::default() }).unwrap();
    assert!(sub.iter().any(|m| rel(r, m).ends_with("FooBarController.ts")));

    // include / exclude globs
    let inc = SearchOptions { include_glob: vec!["*.ts".into(), "*.tsx".into()], ..Default::default() };
    let hits = find_in_files_collect(r, "function", &inc).unwrap();
    let files: Vec<String> = hits.iter().map(|m| rel(r, m)).collect();
    assert_eq!(files, ["src/components/Button.tsx", "src/components/FooBarController.ts"]);
    let exc = SearchOptions { exclude_glob: vec!["src/**".into(), "*.md".into()], ..Default::default() };
    let files: Vec<String> = find_in_files_collect(r, "todo", &exc).unwrap().iter().map(|m| rel(r, m)).collect();
    assert_eq!(files, ["app/service.py"]);
}

#[test]
fn search_max_results_and_cancel() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..20 {
        write(dir.path(), &format!("f{i}.txt"), &"needle\n".repeat(50));
    }
    let hits = find_in_files_collect(dir.path(), "needle", &SearchOptions { max_results: 25, ..Default::default() }).unwrap();
    assert_eq!(hits.len(), 25);

    let tok = CancelToken::new();
    tok.cancel();
    let rx = find_in_files(dir.path(), "needle", &SearchOptions::default(), tok).unwrap();
    let mut n = 0;
    while rx.recv_blocking().is_ok() {
        n += 1;
    }
    assert!(n < 1000, "cancel before start yields (almost) nothing, got {n}");
}

#[test]
fn replace_preview_and_apply() {
    let dir = fixture();
    let r = dir.path();
    write(r, "crlf.txt", "keep\r\nold name here\r\nold again\r\n");
    let opts = SearchOptions { case: true, whole_word: true, ..Default::default() };
    let items = replace_preview(r, "old", "new", &opts, CancelToken::new()).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].new_line, "new name here");
    // preview must not touch disk
    assert!(std::fs::read_to_string(r.join("crlf.txt")).unwrap().contains("old name"));
    let report = apply_replacements(&items).unwrap();
    assert_eq!((report.files_changed, report.lines_changed), (1, 2));
    assert_eq!(std::fs::read_to_string(r.join("crlf.txt")).unwrap(), "keep\r\nnew name here\r\nnew again\r\n");

    // regex capture groups
    let re = SearchOptions { regex: true, case: true, ..Default::default() };
    let items = replace_preview(r, r"TODO\((\w+)\)", "FIXME[$1]", &re, CancelToken::new()).unwrap();
    assert_eq!(items[0].new_line, "// FIXME[alice]: wire up");
    // stale items are skipped, not clobbered
    write(r, "src/index.ts", "changed\n");
    let report = apply_replacements(&items).unwrap();
    assert_eq!(report.stale.len(), 1);
    assert_eq!(std::fs::read_to_string(r.join("src/index.ts")).unwrap(), "changed\n");
}

// ---------------------------------------------------------------- fuzzy

fn top(idx: &FileIndex, q: &str) -> Vec<String> {
    idx.fuzzy_files(q, 10).into_iter().map(|m| m.path).collect()
}

#[test]
fn fuzzy_tier_ranking() {
    let idx = FileIndex::from_paths([
        "src/foo_bar_controller.rs", // snake_case humps
        "src/components/FooBarController.ts",
        "src/other/xfxbxcx.ts",            // scattered
        "src/Foo.ts",
        "src/FooBar.ts",
        "lib/foo.ts",
        "docs/afoo.md",                    // substring
        "src/deep/dir/Foo.ts",
    ]);
    // exact name > prefix > hump/substring > scattered
    let r = top(&idx, "foo.ts");
    assert!(["lib/foo.ts", "src/Foo.ts"].contains(&r[0].as_str()));
    let r = top(&idx, "foo");
    // exact stem first (shallowest), then prefix names
    assert!(r[..3].iter().all(|p| p.ends_with("Foo.ts") || p == "lib/foo.ts"), "{r:?}");
    let pos = |p: &str| r.iter().position(|x| x == p).unwrap_or(usize::MAX);
    assert!(pos("src/FooBar.ts") < pos("docs/afoo.md"), "prefix beats substring: {r:?}");

    let r = top(&idx, "fbc");
    assert_eq!(r[0], "src/components/FooBarController.ts", "camel hump: {r:?}");
    assert!(r.contains(&"src/foo_bar_controller.rs".to_string()), "snake humps: {r:?}");
    assert!(pos_of(&r, "src/components/FooBarController.ts") < pos_of(&r, "src/other/xfxbxcx.ts"));

    let r = top(&idx, "FoBaCo");
    assert_eq!(r[0], "src/components/FooBarController.ts");
}

fn pos_of(v: &[String], p: &str) -> usize {
    v.iter().position(|x| x == p).unwrap_or(usize::MAX)
}

#[test]
fn fuzzy_exact_prefix_hump_scattered_order() {
    let idx = FileIndex::from_paths(["a/b/xbxcx_ctl.ts", "a/Card.ts", "a/CardTable.ts", "a/MyCardTable.ts", "a/x/ctab.ts", "a/aCxTxAxBx.ts"]);
    // query "ct"
    let r = top(&idx, "card");
    assert_eq!(r[0], "a/Card.ts");
    assert_eq!(r[1], "a/CardTable.ts");
    let r = top(&idx, "ct");
    // prefix "ctab.ts" > hump "CardTable" / "MyCardTable"? "ct" is prefix of ctab.ts
    assert_eq!(r[0], "a/x/ctab.ts", "{r:?}");
    assert!(pos_of(&r, "a/CardTable.ts") < pos_of(&r, "a/aCxTxAxBx.ts"), "{r:?}");
    assert!(pos_of(&r, "a/CardTable.ts") < pos_of(&r, "a/MyCardTable.ts"), "{r:?}");
}

#[test]
fn fuzzy_path_segments_and_positions() {
    let idx = FileIndex::from_paths([
        "src/components/Button.tsx",
        "src/components/forms/Button.tsx",
        "src/button/index.ts",
        "test/components/ButtonSpec.ts",
        "src/hooks/useThing.ts",
    ]);
    let r = top(&idx, "comp/button");
    assert_eq!(r[0], "src/components/Button.tsx", "{r:?}");
    assert!(!r.contains(&"src/button/index.ts".to_string()) || pos_of(&r, "src/button/index.ts") > 1);
    let r = top(&idx, "src/hooks/");
    assert_eq!(r[0], "src/hooks/useThing.ts");
    // directory-only hit still found
    let r = top(&idx, "button");
    assert!(r.contains(&"src/button/index.ts".to_string()));
    assert!(pos_of(&r, "src/components/Button.tsx") < pos_of(&r, "src/button/index.ts"), "{r:?}");

    let m = idx.fuzzy_files("btn", 1).remove(0);
    assert_eq!(m.path, "src/components/Button.tsx");
    let hl: String = m.positions.iter().map(|&i| m.path.as_bytes()[i] as char).collect();
    assert_eq!(hl.to_lowercase(), "btn");
}

#[test]
fn fuzzy_recent_boost() {
    let mut idx = FileIndex::from_paths(["a/Util.ts", "b/Util.ts", "c/Utility.ts"]);
    let before = top(&idx, "util");
    assert_eq!(before[0], "a/Util.ts");
    idx.set_recent(&[PathBuf::from("b/Util.ts")]);
    assert_eq!(top(&idx, "util")[0], "b/Util.ts");
    // empty query lists recents first
    assert_eq!(top(&idx, "")[0], "b/Util.ts");
}

#[test]
fn fuzzy_index_from_disk_and_events() {
    let dir = fixture();
    let mut idx = FileIndex::build(dir.path());
    assert!(idx.paths().contains(&"src/components/FooBarController.ts".to_string()));
    assert!(!idx.paths().iter().any(|p| p.starts_with("node_modules") || p.starts_with("dist") || p.starts_with(".git/") || p.starts_with("target")));
    assert_eq!(top(&idx, "fbc")[0], "src/components/FooBarController.ts");
    let f = dir.path().join("src/NewThing.ts");
    write(dir.path(), "src/NewThing.ts", "");
    idx.apply_event(&FsEvent { kind: FsEventKind::Created, paths: vec![f.clone()] });
    assert_eq!(top(&idx, "newthing")[0], "src/NewThing.ts");
    std::fs::remove_file(&f).unwrap();
    idx.apply_event(&FsEvent { kind: FsEventKind::Removed, paths: vec![f] });
    assert!(top(&idx, "newthing").is_empty());
}

fn synthetic_paths(n: usize) -> Vec<String> {
    const DIRS: [&str; 12] = ["src", "lib", "app", "components", "utils", "services", "models", "views", "controllers", "hooks", "tests", "config"];
    const WORDS: [&str; 16] =
        ["Foo", "Bar", "Baz", "User", "Account", "Order", "Invoice", "Payment", "Session", "Token", "Cache", "Router", "Store", "Query", "Logger", "Parser"];
    const EXT: [&str; 6] = ["ts", "tsx", "rs", "py", "js", "go"];
    (0..n)
        .map(|i| {
            let d1 = DIRS[i % 12];
            let d2 = DIRS[(i / 12) % 12];
            let d3 = DIRS[(i / 144) % 12];
            let w = format!("{}{}{}", WORDS[i % 16], WORDS[(i / 16) % 16], WORDS[(i / 256) % 16]);
            format!("packages/pkg{}/{d1}/{d2}/{d3}/{w}{}.{}", i / 5000, i / 4096, EXT[i % 6])
        })
        .collect()
}

#[test]
fn fuzzy_perf_50k() {
    let paths = synthetic_paths(50_000);
    assert_eq!(paths.len(), 50_000);
    let idx = FileIndex::from_paths(paths);
    assert!(idx.len() >= 49_000);
    let queries = ["fbc", "usr", "ord", "pay", "xyz"];
    let mut worst = Duration::ZERO;
    for q in queries {
        let _ = idx.fuzzy_files(q, 50); // warm-up
        let mut best = Duration::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            let r = idx.fuzzy_files(q, 50);
            best = best.min(t.elapsed());
            std::hint::black_box(r);
        }
        eprintln!("fuzzy 50k paths, q={q:?}: best of 5 = {best:?}");
        worst = worst.max(best);
    }
    let bound = if cfg!(debug_assertions) { Duration::from_millis(250) } else { Duration::from_millis(5) };
    assert!(worst < bound, "worst {worst:?} >= {bound:?}");
}

// ---------------------------------------------------------------- recent

#[test]
fn recent_files_ring_and_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    let mut rf = RecentFiles::new(r, 3);
    for f in ["a.ts", "b.ts", "c.ts", "a.ts", "d.ts"] {
        rf.touch(f);
    }
    assert_eq!(rf.list(), vec![r.join("d.ts"), r.join("a.ts"), r.join("c.ts")]);
    let json = std::fs::read_to_string(r.join(".tandem/recent.json")).unwrap();
    assert!(json.contains("\"d.ts\""), "stored relative: {json}");
    let loaded = RecentFiles::load_with_capacity(r, 3);
    assert_eq!(loaded.list(), rf.list());
    std::fs::write(r.join(".tandem/recent.json"), "{ not json").unwrap();
    assert!(RecentFiles::load(r).is_empty());
}

// ---------------------------------------------------------------- symbols

fn syms(idx: &SymbolIndex, rel: &str) -> Vec<(String, SymbolKind, Option<String>)> {
    idx.file_structure(Path::new(rel)).iter().map(|s| (s.name.clone(), s.kind, s.container.clone())).collect()
}

fn s(name: &str, kind: SymbolKind, container: Option<&str>) -> (String, SymbolKind, Option<String>) {
    (name.to_string(), kind, container.map(str::to_string))
}

#[test]
fn symbols_typescript() {
    use SymbolKind::*;
    let dir = fixture();
    let idx = SymbolIndex::build(dir.path());
    assert_eq!(
        syms(&idx, "src/components/FooBarController.ts"),
        vec![
            s("Options", Interface, None),
            s("Handler", TypeAlias, None),
            s("Mode", Enum, None),
            s("FooBarController", Class, None),
            s("handle", Method, Some("FooBarController")),
            s("constructor", Method, Some("FooBarController")),
            s("start", Method, Some("FooBarController")),
            s("create", Method, Some("FooBarController")),
            s("helper", Function, None),
            s("MAX", Const, None),
            s("arrow", Function, None),
        ]
    );
    // ordered by position, with sensible spans
    let fs = idx.file_structure(Path::new("src/components/FooBarController.ts"));
    assert!(fs.windows(2).all(|w| w[0].range.start_byte <= w[1].range.start_byte));
    assert_eq!((fs[0].selection.start_line, fs[0].selection.start_col), (0, 17));
}

#[test]
fn symbols_tsx_js_py_rs_go() {
    use SymbolKind::*;
    let dir = fixture();
    let idx = SymbolIndex::build(dir.path());
    assert_eq!(syms(&idx, "src/components/Button.tsx"), vec![s("Button", Function, None), s("Panel", Class, None)]);
    assert_eq!(
        syms(&idx, "src/util.js"),
        vec![s("Widget", Class, None), s("render", Method, Some("Widget")), s("baz", Function, None), s("qux", Function, None), s("counter", Variable, None)]
    );
    assert_eq!(
        syms(&idx, "app/service.py"),
        vec![
            s("Service", Class, None),
            s("run", Method, Some("Service")),
            s("make", Method, Some("Service")),
            s("main", Function, None),
            s("MAX_RETRIES", Const, None),
            s("name", Variable, None),
        ]
    );
    assert_eq!(
        syms(&idx, "crates/core/src/lib.rs"),
        vec![
            s("Foo", Struct, None),
            s("new", Method, Some("Foo")),
            s("Shape", Trait, None),
            s("area", Method, Some("Shape")),
            s("Color", Enum, None),
            s("m", Module, None),
            s("inner", Function, Some("m")),
            s("LIMIT", Const, None),
            s("Alias", TypeAlias, None),
            s("free", Function, None),
            s("mac", Macro, None),
        ]
    );
    assert_eq!(
        syms(&idx, "cmd/main.go"),
        vec![
            s("Server", Struct, None),
            s("Handler", Interface, None),
            s("Start", Method, Some("Server")),
            s("main", Function, None),
            s("Version", Const, None),
            s("Global", Variable, None),
        ]
    );
    // nothing from node_modules / target / dist
    assert!(idx.file_structure(Path::new("node_modules/pkg/index.js")).is_empty());
    assert!(idx.file_structure(Path::new("target/debug/x.rs")).is_empty());
}

#[test]
fn symbols_fuzzy_and_update() {
    let dir = fixture();
    let mut idx = SymbolIndex::build(dir.path());
    let r = idx.fuzzy_symbols("fbc", 5);
    assert_eq!(r[0].symbol.name, "FooBarController");
    assert!(r[0].path.ends_with("FooBarController.ts"));
    let r = idx.fuzzy_symbols("start", 5);
    assert!(r.iter().any(|m| m.symbol.name == "start") && r.iter().any(|m| m.symbol.name == "Start"));
    assert!(idx.fuzzy_symbols("zzzz", 5).is_empty());

    // incremental update
    write(dir.path(), "src/util.js", "function brandNew() {}\n");
    idx.update(Path::new("src/util.js"));
    assert_eq!(syms(&idx, "src/util.js"), vec![s("brandNew", SymbolKind::Function, None)]);
    assert_eq!(idx.fuzzy_symbols("bn", 3)[0].symbol.name, "brandNew");
    idx.update_source(Path::new("src/util.js"), "class Live {}\n");
    assert_eq!(syms(&idx, "src/util.js")[0].0, "Live");
    std::fs::remove_file(dir.path().join("src/util.js")).unwrap();
    idx.update(Path::new("src/util.js"));
    assert!(idx.file_structure(Path::new("src/util.js")).is_empty());
}
