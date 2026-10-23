//! Integration tests against the scripted fake server (tests/fixtures/fake_lsp.rs).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tandem_lsp::lsp_types::*;
use tandem_lsp::*;

fn fake_config() -> LanguageServerConfig {
    LanguageServerConfig {
        id: "fake".into(),
        name: "Fake".into(),
        commands: vec![ServerCommand::new(env!("CARGO_BIN_EXE_tandem-fake-lsp"), &[])],
        file_types: vec![FileType { extension: "ts".into(), language_id: "typescript".into() }],
        root_markers: vec![],
        install_hint: "built by cargo".into(),
        initialization_options: None,
        env: vec![],
    }
}

fn fast_options() -> ClientOptions {
    ClientOptions {
        request_timeout: Duration::from_secs(5),
        restart_backoff: Duration::from_millis(50),
        ..Default::default()
    }
}

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    file: PathBuf,
}

fn project() -> Project {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonicalize");
    let file = root.join("src dir").join("main.ts");
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(&file, "const foo = 1;\nfoo;\n").expect("write");
    Project { _dir: dir, root, file }
}

async fn wait_for<T>(
    rx: &async_channel::Receiver<T>,
    mut pred: impl FnMut(&T) -> bool,
) -> T {
    let fut = async {
        loop {
            let n = rx.recv().await.expect("channel closed");
            if pred(&n) {
                return n;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), fut).await.expect("timed out waiting for notification")
}

fn is_log(n: &ServerNotification, text: &str) -> bool {
    matches!(n, ServerNotification::LogMessage(m) if m.message.contains(text))
}

async fn stat(client: &LspClient) -> serde_json::Value {
    client.request_raw("fake/stats", serde_json::Value::Null).await.expect("stats")
}

#[tokio::test]
#[allow(clippy::mutable_key_type)] // lsp_types::Uri as HashMap key (upstream type)
async fn handshake_requests_notifications_and_shutdown() {
    let p = project();
    let client = LspClient::start_with_options(fake_config(), &p.root, fast_options()).await.expect("start");
    let rx = client.subscribe();

    // Handshake + server-initiated requests answered (backlog delivered to first subscriber).
    assert_eq!(client.server_info().expect("info").name, "fake-lsp");
    assert!(client.capabilities().hover_provider.is_some());
    wait_for(&rx, |n| is_log(n, "configuration ok: 2")).await;
    wait_for(&rx, |n| matches!(n, ServerNotification::Progress(_))).await;
    wait_for(&rx, |n| matches!(n, ServerNotification::ShowMessage(m) if m.message == "hello from fake")).await;
    // Our reply to client/registerCapability reaches the server after its other pushes.
    wait_for(&rx, |n| is_log(n, "register ok")).await;

    // didOpen -> publishDiagnostics routed with decoded path.
    let text = std::fs::read_to_string(&p.file).expect("read");
    client.did_open(&p.file, "typescript", &text).expect("open");
    let diag = wait_for(&rx, |n| matches!(n, ServerNotification::Diagnostics { .. })).await;
    let ServerNotification::Diagnostics { path, diagnostics, version, .. } = diag else { unreachable!() };
    assert_eq!(path.as_deref(), Some(p.file.as_path()));
    assert_eq!(version, Some(0));
    assert_eq!(diagnostics[0].message, "fake error");

    // Requests.
    let pos = Position::new(1, 2);
    let items = client.completion(&p.file, pos).await.expect("completion");
    assert_eq!(items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["alpha", "beta"]);
    let resolved = client.resolve_completion(items[0].clone()).await.expect("resolve");
    assert_eq!(resolved.detail.as_deref(), Some("resolved alpha"));

    let hover = client.hover(&p.file, pos).await.expect("hover").expect("some hover");
    match hover.contents {
        HoverContents::Markup(m) => assert_eq!(m.value, "**hover** 1:2"),
        other => panic!("unexpected hover {other:?}"),
    }

    let defs = client.definition(&p.file, pos).await.expect("definition");
    assert_eq!(defs.len(), 1);
    assert_eq!(uri_to_path(&defs[0].uri).as_deref(), Some(p.file.as_path()));
    assert_eq!(defs[0].range.start, Position::new(0, 6));

    assert_eq!(client.references(&p.file, pos, true).await.expect("refs").len(), 2);
    let edit = client.rename(&p.file, pos, "bar").await.expect("rename");
    let changes = edit.changes.expect("changes");
    assert_eq!(changes[&path_to_uri(&p.file).expect("uri")][0].new_text, "bar");
    assert!(matches!(
        client.document_symbols(&p.file).await.expect("symbols"),
        Some(DocumentSymbolResponse::Flat(v)) if v[0].name == "foo"
    ));
    let ws = client.workspace_symbols("foo").await.expect("ws symbols");
    assert_eq!(ws[0].name, "sym_foo");
    let actions = client
        .code_actions(&p.file, Range::default(), diagnostics.clone())
        .await
        .expect("code actions");
    assert!(matches!(&actions[0], CodeActionOrCommand::CodeAction(a) if a.title == "Fix it (1 diags)"));
    let edits = client.format_document(&p.file, 2, true).await.expect("format");
    assert_eq!(edits[0].new_text, "// tab 2\n");
    let sig = client.signature_help(&p.file, pos).await.expect("sig").expect("some sig");
    assert_eq!(sig.signatures[0].label, "foo(a: number)");

    // Out-of-order responses: a slow request doesn't block a fast one.
    let slow = {
        let c = client.clone();
        tokio::spawn(async move { c.request_raw("fake/slow", serde_json::Value::Null).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(client.hover(&p.file, pos).await.expect("hover").is_some());
    assert!(!slow.is_finished(), "slow request should still be pending");
    assert_eq!(slow.await.expect("join").expect("slow"), "slow done");

    // Errors and timeouts.
    let err = client.request_raw("fake/unknown", serde_json::Value::Null).await.expect_err("unknown");
    assert!(matches!(err, LspError::Rpc { code: -32601, .. }));
    let err = client
        .request_raw_timeout("fake/never", serde_json::Value::Null, Duration::from_millis(100))
        .await
        .expect_err("timeout");
    assert!(matches!(err, LspError::Timeout { .. }));

    // Sync: full change, debounced burst, save with text, close.
    client.did_change_full(&p.file, 1, "const foo = 2;\n").expect("change");
    assert_eq!(stat(&client).await["didChange"], 1);
    assert_eq!(client.document_version(&p.file), Some(1));

    let debouncer = ChangeDebouncer::new(client.clone());
    for v in 2..12 {
        debouncer.push(&p.file, v, format!("v{v}"));
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let s = stat(&client).await;
    assert_eq!(s["didChange"], 2, "burst coalesced into one didChange");
    assert_eq!(s["lastText"], "v11");
    debouncer.push(&p.file, 12, "v12");
    debouncer.flush().await;
    assert_eq!(stat(&client).await["lastText"], "v12");

    client.did_save(&p.file).expect("save");
    wait_for(&rx, |n| is_log(n, "saved with text: v12")).await;
    client.did_close(&p.file).expect("close");
    wait_for(&rx, |n| matches!(n, ServerNotification::Diagnostics { diagnostics, .. } if diagnostics.is_empty())).await;

    client.shutdown().await.expect("shutdown");
    assert!(!client.is_running());
    assert!(client.is_dead());
    wait_for(&rx, |n| matches!(n, ServerNotification::Exited { restarting: false, status: Some(0) })).await;
    assert!(matches!(client.hover(&p.file, pos).await, Err(LspError::ServerExited)));
}

#[tokio::test]
async fn crash_triggers_one_restart_and_reopens_documents() {
    let p = project();
    let client = LspClient::start_with_options(fake_config(), &p.root, fast_options()).await.expect("start");
    let rx = client.subscribe();
    client.did_open(&p.file, "typescript", "const foo = 1;").expect("open");
    wait_for(&rx, |n| matches!(n, ServerNotification::Diagnostics { .. })).await;

    // In-flight request fails fast when the server dies.
    let pending = {
        let c = client.clone();
        tokio::spawn(async move { c.request_raw("fake/never", serde_json::Value::Null).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    client.notify_raw("fake/crash", serde_json::Value::Null).expect("crash");
    assert!(matches!(pending.await.expect("join"), Err(LspError::ServerExited)));

    wait_for(&rx, |n| matches!(n, ServerNotification::Exited { status: Some(3), restarting: true })).await;
    wait_for(&rx, |n| matches!(n, ServerNotification::Restarted { attempt: 1 })).await;
    // Document replayed into the new process -> diagnostics again.
    wait_for(&rx, |n| matches!(n, ServerNotification::Diagnostics { .. })).await;
    assert!(client.is_running());
    assert_eq!(client.restart_count(), 1);
    assert!(client.hover(&p.file, Position::new(0, 0)).await.expect("hover").is_some());

    // Second crash: no more restarts.
    client.notify_raw("fake/crash", serde_json::Value::Null).expect("crash");
    wait_for(&rx, |n| matches!(n, ServerNotification::Exited { restarting: false, .. })).await;
    assert!(client.is_dead());
    assert!(client.hover(&p.file, Position::new(0, 0)).await.is_err());
}

#[tokio::test]
async fn manager_routes_clients_and_aggregates_diagnostics() {
    let p = project();
    let manager = Arc::new(LspManager::with_configs(&p.root, vec![fake_config()], fast_options()));
    let events = manager.subscribe();
    assert!(manager.client_for(&p.file).is_none());
    assert!(manager.ensure_client(Path::new("/x/readme.md")).await.expect("none").is_none());

    // Concurrent ensure_client calls share one start.
    let (a, b) = tokio::join!(manager.ensure_client(&p.file), manager.ensure_client(&p.file));
    let a = a.expect("a").expect("client a");
    let b = b.expect("b").expect("client b");
    assert!(Arc::ptr_eq(&a, &b));
    assert!(Arc::ptr_eq(&a, &manager.client_for(&p.file).expect("running")));
    assert_eq!(manager.running().len(), 1);

    a.did_open(&p.file, "typescript", "x").expect("open");
    wait_for(&events, |e| matches!(e, ManagerEvent::DiagnosticsChanged { path } if path == &p.file)).await;
    assert_eq!(manager.diagnostics(&p.file)[0].message, "fake error");
    assert_eq!(manager.all_diagnostics().len(), 1);

    a.did_close(&p.file).expect("close");
    wait_for(&events, |e| matches!(e, ManagerEvent::DiagnosticsChanged { path } if path == &p.file)).await;
    assert!(manager.diagnostics(&p.file).is_empty());

    manager.shutdown_all().await;
    assert!(manager.running().is_empty());
    assert!(!a.is_running());
}

#[tokio::test]
async fn missing_server_reports_install_hint() {
    let mut cfg = fake_config();
    cfg.commands = vec![ServerCommand::new("tandem-no-such-server-xyz", &[])];
    let err = LspClient::start(cfg, std::env::temp_dir()).await.expect_err("missing");
    assert!(matches!(&err, LspError::NotInstalled { hint, .. } if hint == "built by cargo"), "{err}");
}
