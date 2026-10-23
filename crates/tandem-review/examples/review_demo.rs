//! Usage: cargo run -p tandem-review --example review_demo -- a.json b.json ["intent A"] ["intent B"]
//!
//! Reads two serialized `DiffSet`s, prints intent conflicts between them and a
//! summary of each. With no arguments, runs on a built-in fixture.

use anyhow::{Context, Result};
use tandem_core::DiffSet;
use tandem_review::{TaskMeta, describe, detect, diff_texts};

fn load(path: &str) -> Result<DiffSet> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    serde_json::from_str(&s).with_context(|| format!("parsing {path} as DiffSet"))
}

fn builtin() -> (DiffSet, DiffSet) {
    let old = "fn load(p: &str) -> String {\n    let s = read(p);\n    s.trim().to_string()\n}\n\nfn main() {\n    println!(\"{}\", load(\"a\"));\n}\n";
    let a = old.replace("s.trim()", "s.trim_end()");
    let b = old.replace("let s = read(p);", "let s = read_cached(p);");
    let mk = |task: &str, agent: &str, new: &str| DiffSet {
        task: task.into(), agent: agent.into(), base_commit: "HEAD".into(),
        files: vec![diff_texts("src/main.rs", Some(old), Some(new))],
    };
    (mk("demo-a", "claude", &a), mk("demo-b", "codex", &b))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (a, b) = if args.len() >= 2 { (load(&args[0])?, load(&args[1])?) } else { builtin() };
    let intent = |i: usize, d: &DiffSet| args.get(i).cloned().unwrap_or_else(|| format!("task {}", d.task));
    let ma = TaskMeta::new(a.task.clone(), a.agent.clone(), intent(2, &a));
    let mb = TaskMeta::new(b.task.clone(), b.agent.clone(), intent(3, &b));

    let conflicts = detect(&[(ma, a.clone()), (mb, b.clone())]);
    println!("== {} conflict(s)", conflicts.len());
    for c in &conflicts {
        println!("[{:?}/{:?}] {}", c.severity, c.kind, c.explanation);
        for s in [&c.a, &c.b] {
            let ranges: Vec<String> = s.ranges.iter().map(ToString::to_string).collect();
            println!("    {} ({}): {} @ {} — \"{}\"", s.agent, s.task, s.path.display(), ranges.join(","), s.intent);
        }
    }
    for d in [&a, &b] {
        let s = describe(d);
        println!("== summary {} ({}): {} file(s), +{} -{}", d.task, d.agent, s.files, s.additions, s.deletions);
        for sym in &s.touched_symbols { println!("    touches {sym}") }
        for r in &s.risky { println!("    RISK {r}") }
    }
    Ok(())
}
