//! Real-server smoke test; skipped when typescript-language-server isn't installed.

use std::time::Duration;
use tandem_lsp::lsp_types::{HoverContents, MarkedString};
use tandem_lsp::*;

#[tokio::test]
async fn typescript_hover_and_definition() {
    let Some(config) = builtin_servers().into_iter().find(|c| c.id == "typescript") else {
        panic!("typescript config missing");
    };
    if !config.is_installed() {
        eprintln!("skipping: typescript-language-server not installed ({})", config.install_hint);
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonicalize");
    std::fs::write(root.join("tsconfig.json"), r#"{"compilerOptions":{"strict":true}}"#).expect("w");
    let file = root.join("index.ts");
    let text = "const greeting = \"hi\";\nconsole.log(greeting);\n";
    std::fs::write(&file, text).expect("w");

    let client = LspClient::start(config, &root).await.expect("start tsserver");
    eprintln!("typescript server: {:?} {:?}", client.command(), client.server_info());
    client.did_open(&file, "typescript", text).expect("open");
    // `greeting` in line 2.
    let pos = offset_to_position(text, text.rfind("greeting").expect("ident") + 2);

    let mut hover_text = String::new();
    for _ in 0..40 {
        if let Ok(Some(h)) = client.hover(&file, pos).await {
            hover_text = match h.contents {
                HoverContents::Markup(m) => m.value,
                HoverContents::Scalar(MarkedString::String(s)) => s,
                HoverContents::Scalar(MarkedString::LanguageString(l)) => l.value,
                HoverContents::Array(v) => format!("{v:?}"),
            };
            if hover_text.contains("greeting") {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(hover_text.contains("const greeting"), "hover was {hover_text:?}");

    let defs = client.definition(&file, pos).await.expect("definition");
    assert_eq!(defs.len(), 1, "{defs:?}");
    assert_eq!(uri_to_path(&defs[0].uri).as_deref(), Some(file.as_path()));
    assert_eq!(defs[0].range.start.line, 0);

    client.shutdown().await.expect("shutdown");
    assert!(!client.is_running());
}
