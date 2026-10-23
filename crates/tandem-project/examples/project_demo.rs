//! Headless demo: `cargo run -p tandem-project --example project_demo -- --root <dir> <files|symbols|grep> <query>`

use std::path::{Path, PathBuf};
use std::time::Instant;
use tandem_project::{CancelToken, FileIndex, SearchOptions, SymbolIndex, find_in_files};

fn usage() -> ! {
    eprintln!("usage: project_demo [--root <dir>] files|symbols|grep <query> [--regex] [--case] [--word] [--include <glob>] [--exclude <glob>]");
    std::process::exit(2);
}

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut root = PathBuf::from(".");
    let mut opts = SearchOptions { max_results: 200, ..Default::default() };
    let mut pos = Vec::new();
    while !args.is_empty() {
        let a = args.remove(0);
        match a.as_str() {
            "--root" => root = PathBuf::from(args.remove(0)),
            "--regex" => opts.regex = true,
            "--case" => opts.case = true,
            "--word" => opts.whole_word = true,
            "--include" => opts.include_glob.push(args.remove(0)),
            "--exclude" => opts.exclude_glob.push(args.remove(0)),
            _ => pos.push(a),
        }
    }
    let (Some(cmd), Some(query)) = (pos.first(), pos.get(1)) else { usage() };
    let root = root.canonicalize()?;
    let t = Instant::now();
    match cmd.as_str() {
        "files" => {
            let idx = FileIndex::build(&root);
            let built = t.elapsed();
            let t = Instant::now();
            let hits = idx.fuzzy_files(query, 20);
            for m in &hits {
                println!("{:>6}  {}", m.score, m.path);
            }
            eprintln!("{} files indexed in {built:?}; query in {:?}", idx.len(), t.elapsed());
        }
        "symbols" => {
            let idx = SymbolIndex::build(&root);
            let built = t.elapsed();
            for m in idx.fuzzy_symbols(query, 20) {
                let s = &m.symbol;
                let rel: &Path = m.path.strip_prefix(&root).unwrap_or(&m.path);
                let c = s.container.as_deref().map(|c| format!("{c}::")).unwrap_or_default();
                println!("{:>6}  {:<9} {c}{}  {}:{}", m.score, s.kind, s.name, rel.display(), s.selection.start_line + 1);
            }
            eprintln!("{} symbols in {} files indexed in {built:?}", idx.symbol_count(), idx.file_count());
        }
        "grep" => {
            let rx = find_in_files(&root, query, &opts, CancelToken::new())?;
            let mut n = 0;
            while let Ok(m) = rx.recv_blocking() {
                let rel = m.path.strip_prefix(&root).unwrap_or(&m.path);
                println!("{}:{}:{}: {}", rel.display(), m.line_no, m.col, m.line_text.trim());
                n += 1;
            }
            eprintln!("{n} matches in {:?}", t.elapsed());
        }
        _ => usage(),
    }
    Ok(())
}
