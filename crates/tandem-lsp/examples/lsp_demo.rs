//! Headless LSP demo: prints hover and definition at a position.
//!
//! cargo run -p tandem-lsp --example lsp_demo -- --root <dir> --file <path> --line <n> --col <n>
//! `--line`/`--col` are 1-based; `--col` counts characters.

use std::path::PathBuf;
use std::time::Duration;
use tandem_lsp::lsp_types::{HoverContents, MarkedString};
use tandem_lsp::{LspManager, ManagerEvent, offset_to_position, uri_to_path};

fn usage() -> anyhow::Error {
    anyhow::anyhow!("usage: lsp_demo --root <dir> --file <path> --line <n> --col <n>")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut root = None;
    let mut file = None;
    let mut line = None;
    let mut col = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let v = args.next().ok_or_else(usage)?;
        match a.as_str() {
            "--root" => root = Some(PathBuf::from(v)),
            "--file" => file = Some(PathBuf::from(v)),
            "--line" => line = Some(v.parse::<usize>()?),
            "--col" => col = Some(v.parse::<usize>()?),
            _ => return Err(usage()),
        }
    }
    let (Some(root), Some(file), Some(line), Some(col)) = (root, file, line, col) else {
        return Err(usage());
    };
    let root = std::fs::canonicalize(root)?;
    let file = std::fs::canonicalize(if file.is_absolute() { file } else { root.join(file) })?;
    let text = std::fs::read_to_string(&file)?;

    let line_start: usize = text.split_inclusive('\n').take(line.saturating_sub(1)).map(str::len).sum();
    let line_text = text[line_start..].split('\n').next().unwrap_or_default();
    let col_bytes: usize = line_text.chars().take(col.saturating_sub(1)).map(char::len_utf8).sum();
    let pos = offset_to_position(&text, line_start + col_bytes);

    let manager = LspManager::new(&root);
    let events = manager.subscribe();
    let language_id = manager.language_id_for(&file).ok_or_else(|| anyhow::anyhow!("no server for {}", file.display()))?.to_string();
    let client = manager.ensure_client(&file).await?.ok_or_else(|| anyhow::anyhow!("no server"))?;
    println!("server: {} ({:?})", client.config().name, client.command().program);
    client.did_open(&file, &language_id, &text)?;

    // Servers index lazily; retry hover briefly.
    let mut hover = None;
    for _ in 0..40 {
        if let Ok(Some(h)) = client.hover(&file, pos).await {
            hover = Some(h);
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    println!("--- hover @ {}:{} (lsp {}:{})", line, col, pos.line, pos.character);
    match hover.map(|h| h.contents) {
        Some(HoverContents::Markup(m)) => println!("{}", m.value),
        Some(HoverContents::Scalar(MarkedString::String(s))) => println!("{s}"),
        Some(HoverContents::Scalar(MarkedString::LanguageString(l))) => println!("```{}\n{}\n```", l.language, l.value),
        Some(HoverContents::Array(v)) => println!("{v:?}"),
        None => println!("(none)"),
    }
    println!("--- definition");
    let defs = client.definition(&file, pos).await?;
    if defs.is_empty() {
        println!("(none)");
    }
    for d in defs {
        let path = uri_to_path(&d.uri).map_or_else(|| d.uri.as_str().to_string(), |p| p.display().to_string());
        println!("{}:{}:{}", path, d.range.start.line + 1, d.range.start.character + 1);
    }
    // Give the server a moment to publish diagnostics.
    let _ = tokio::time::timeout(Duration::from_secs(3), async {
        while let Ok(ev) = events.recv().await {
            if matches!(&ev, ManagerEvent::DiagnosticsChanged { path } if path == &file) {
                break;
            }
        }
    })
    .await;
    let diags = manager.diagnostics(&file);
    println!("--- diagnostics: {}", diags.len());
    for d in diags {
        println!("{}:{} {}", d.range.start.line + 1, d.range.start.character + 1, d.message);
    }
    manager.shutdown_all().await;
    Ok(())
}
