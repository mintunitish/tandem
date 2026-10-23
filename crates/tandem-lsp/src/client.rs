//! A single language server process: JSON-RPC over stdio, handshake, document sync,
//! typed requests, notification fan-out, crash detection with auto-restart.
//!
//! Concurrency model: one reader task per process dispatches responses (to oneshot
//! channels), server requests (answered immediately via the unbounded writer queue)
//! and notifications (to unbounded subscriber channels). Nothing in the reader awaits
//! anything other than stdout, so callers can never stall it.

use crate::framing::{FrameDecoder, encode};
use crate::{LanguageServerConfig, LspError, Result, ServerCommand, path_to_uri, uri_to_path};
use lsp_types::notification::{self as notif, Notification};
use lsp_types::request::{self as req, Request};
use lsp_types::*;
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// Default timeout for ordinary requests.
    pub request_timeout: Duration,
    /// Timeout for the `initialize` request (servers may index first).
    pub init_timeout: Duration,
    /// How long `shutdown()` waits for the polite `shutdown`/`exit` sequence.
    pub shutdown_timeout: Duration,
    /// Automatic restarts after an unexpected exit.
    pub max_restarts: u32,
    /// Delay before the first restart; doubles for each further attempt.
    pub restart_backoff: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            init_timeout: Duration::from_secs(60),
            shutdown_timeout: Duration::from_secs(2),
            max_restarts: 1,
            restart_backoff: Duration::from_millis(500),
        }
    }
}

/// Everything the server pushes to us, plus lifecycle events.
#[derive(Clone, Debug)]
pub enum ServerNotification {
    Diagnostics { uri: Uri, path: Option<PathBuf>, version: Option<i32>, diagnostics: Vec<Diagnostic> },
    Progress(ProgressParams),
    LogMessage(LogMessageParams),
    ShowMessage(ShowMessageParams),
    /// `window/showMessageRequest`; already answered with `null` (no action chosen).
    ShowMessageRequest(ShowMessageRequestParams),
    /// Any other notification, and server requests we only acknowledge
    /// (e.g. `workspace/applyEdit`, `*/refresh`).
    Other { method: String, params: Value },
    /// The process exited. `restarting` is true when an auto-restart is scheduled.
    Exited { status: Option<i32>, restarting: bool },
    /// A replacement process finished its handshake; open documents were re-sent.
    Restarted { attempt: u32 },
}

/// Notifications emitted before the first subscriber are kept (up to this many) and
/// delivered to that first subscriber, so handshake-time logs/progress aren't lost.
const BACKLOG_LIMIT: usize = 256;

#[derive(Default)]
struct Subscribers {
    list: Vec<async_channel::Sender<ServerNotification>>,
    backlog: Option<Vec<ServerNotification>>,
}

struct Shared {
    subscribers: Mutex<Subscribers>,
    shutting_down: AtomicBool,
    /// Process gone and no restart pending.
    dead: AtomicBool,
    root: PathBuf,
    root_uri: Uri,
    server_id: String,
}

impl Shared {
    fn broadcast(&self, n: ServerNotification) {
        let mut subs = self.subscribers.lock();
        if let Some(backlog) = subs.backlog.as_mut() {
            if backlog.len() < BACKLOG_LIMIT {
                backlog.push(n);
            }
            return;
        }
        subs.list.retain(|s| s.try_send(n.clone()).is_ok());
    }
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>;

struct ExitEvent {
    generation: u64,
    status: Option<i32>,
}

struct Connection {
    generation: u64,
    writer: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    next_id: AtomicI64,
    kill: Mutex<Option<oneshot::Sender<()>>>,
    exited: watch::Receiver<bool>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(k) = self.kill.lock().take() {
            let _ = k.send(());
        }
    }
}

/// Removes the pending entry (and tells the server to cancel) if the request future is
/// dropped or times out before the response arrives.
struct PendingGuard<'a> {
    conn: &'a Connection,
    id: i64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if self.conn.pending.lock().remove(&self.id).is_some() {
            let _ = self.conn.send(&json!({
                "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": self.id }
            }));
        }
    }
}

impl Connection {
    fn send(&self, msg: &Value) -> Result<()> {
        let body = serde_json::to_vec(msg)?;
        self.writer.send(encode(&body)).map_err(|_| LspError::ServerExited)
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn is_alive(&self) -> bool {
        !*self.exited.borrow()
    }

    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        if !self.is_alive() {
            return Err(LspError::ServerExited);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        let _guard = PendingGuard { conn: self, id };
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(LspError::ServerExited),
            Err(_) => Err(LspError::Timeout { method: method.to_string() }),
        }
    }
}

fn spawn_connection(
    shared: &Arc<Shared>,
    command: &ServerCommand,
    env: &[(String, String)],
    generation: u64,
    exit_tx: mpsc::UnboundedSender<ExitEvent>,
) -> Result<Arc<Connection>> {
    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args)
        .current_dir(&shared.root)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|source| LspError::Spawn { program: command.program.clone(), source })?;
    let missing = || LspError::Io(std::io::Error::other("child stdio not captured"));
    let mut stdin = child.stdin.take().ok_or_else(missing)?;
    let mut stdout = child.stdout.take().ok_or_else(missing)?;
    let stderr = child.stderr.take().ok_or_else(missing)?;

    let (writer, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let pending: Pending = Arc::default();
    let (kill_tx, kill_rx) = oneshot::channel::<()>();
    let (exited_tx, exited_rx) = watch::channel(false);

    // Writer.
    tokio::spawn(async move {
        while let Some(buf) = write_rx.recv().await {
            if stdin.write_all(&buf).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });

    // Reader + dispatcher.
    {
        let pending = pending.clone();
        let writer = writer.clone();
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut decoder = FrameDecoder::new();
            let mut buf = vec![0u8; 64 * 1024];
            'outer: loop {
                let n = match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                decoder.push(&buf[..n]);
                loop {
                    match decoder.next_frame() {
                        Ok(Some(frame)) => dispatch(&frame, &pending, &writer, &shared),
                        Ok(None) => break,
                        Err(e) => {
                            tracing::error!(server = %shared.server_id, "framing error: {e}");
                            break 'outer;
                        }
                    }
                }
            }
            for (_, tx) in pending.lock().drain() {
                let _ = tx.send(Err(LspError::ServerExited));
            }
        });
    }

    // Stderr -> tracing.
    {
        let id = shared.server_id.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "tandem_lsp::stderr", server = %id, "{line}");
            }
        });
    }

    // Process waiter: reports exit to the supervisor.
    tokio::spawn(async move {
        let status = tokio::select! {
            s = child.wait() => s.ok().and_then(|s| s.code()),
            _ = kill_rx => {
                let _ = child.kill().await;
                child.wait().await.ok().and_then(|s| s.code())
            }
        };
        let _ = exited_tx.send(true);
        let _ = exit_tx.send(ExitEvent { generation, status });
    });

    Ok(Arc::new(Connection {
        generation,
        writer,
        pending,
        next_id: AtomicI64::new(1),
        kill: Mutex::new(Some(kill_tx)),
        exited: exited_rx,
    }))
}

fn dispatch(frame: &[u8], pending: &Pending, writer: &mpsc::UnboundedSender<Vec<u8>>, shared: &Shared) {
    let msg: Value = match serde_json::from_slice(frame) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(server = %shared.server_id, "invalid JSON from server: {e}");
            return;
        }
    };
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").filter(|v| !v.is_null());
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match (method, id) {
        (Some(method), Some(id)) => {
            let reply = match handle_server_request(method, params, shared) {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err((code, message)) => {
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
                }
            };
            if let Ok(body) = serde_json::to_vec(&reply) {
                let _ = writer.send(encode(&body));
            }
        }
        (Some(method), None) => shared.broadcast(map_notification(method, params)),
        (None, Some(id)) => {
            let Some(id) = id.as_i64().or_else(|| id.as_str().and_then(|s| s.parse().ok())) else {
                return;
            };
            let Some(tx) = pending.lock().remove(&id) else { return };
            let result = match msg.get("error") {
                Some(err) if !err.is_null() => Err(LspError::Rpc {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err.get("message").and_then(Value::as_str).unwrap_or_default().to_string(),
                    data: err.get("data").cloned(),
                }),
                _ => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(result);
        }
        (None, None) => {}
    }
}

fn handle_server_request(method: &str, params: Value, shared: &Shared) -> Result<Value, (i64, String)> {
    match method {
        "workspace/configuration" => {
            let n = params.get("items").and_then(Value::as_array).map_or(0, Vec::len);
            Ok(Value::Array(vec![Value::Null; n]))
        }
        "client/registerCapability" | "client/unregisterCapability" | "window/workDoneProgress/create" => {
            Ok(Value::Null)
        }
        "workspace/workspaceFolders" => Ok(json!([{ "uri": shared.root_uri, "name": folder_name(&shared.root) }])),
        "window/showMessageRequest" => {
            if let Ok(p) = serde_json::from_value(params) {
                shared.broadcast(ServerNotification::ShowMessageRequest(p));
            }
            Ok(Value::Null)
        }
        "workspace/applyEdit" => {
            shared.broadcast(ServerNotification::Other { method: method.into(), params });
            Ok(json!({ "applied": false, "failureReason": "client does not apply server-initiated edits" }))
        }
        "window/showDocument" => {
            shared.broadcast(ServerNotification::Other { method: method.into(), params });
            Ok(json!({ "success": false }))
        }
        m if m.ends_with("/refresh") => {
            shared.broadcast(ServerNotification::Other { method: method.into(), params });
            Ok(Value::Null)
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

fn map_notification(method: &str, params: Value) -> ServerNotification {
    fn parse<T: serde::de::DeserializeOwned>(p: &Value) -> Option<T> {
        serde_json::from_value(p.clone()).ok()
    }
    let mapped = match method {
        notif::PublishDiagnostics::METHOD => parse::<PublishDiagnosticsParams>(&params).map(|p| {
            ServerNotification::Diagnostics {
                path: uri_to_path(&p.uri),
                uri: p.uri,
                version: p.version,
                diagnostics: p.diagnostics,
            }
        }),
        notif::Progress::METHOD => parse(&params).map(ServerNotification::Progress),
        notif::LogMessage::METHOD => parse(&params).map(ServerNotification::LogMessage),
        notif::ShowMessage::METHOD => parse(&params).map(ServerNotification::ShowMessage),
        _ => None,
    };
    mapped.unwrap_or(ServerNotification::Other { method: method.to_string(), params })
}

fn folder_name(root: &Path) -> String {
    root.file_name().map_or_else(|| root.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned())
}

fn client_capabilities() -> Value {
    json!({
        "general": { "positionEncodings": ["utf-16"] },
        "window": { "workDoneProgress": true, "showMessage": { "messageActionItem": { "additionalPropertiesSupport": false } } },
        "workspace": {
            "applyEdit": false,
            "workspaceEdit": { "documentChanges": true, "resourceOperations": ["create", "rename", "delete"] },
            "configuration": true,
            "workspaceFolders": true,
            "didChangeConfiguration": { "dynamicRegistration": false },
            "symbol": { "dynamicRegistration": false, "symbolKind": { "valueSet": (1..=26).collect::<Vec<u32>>() } },
            "semanticTokens": { "refreshSupport": false }
        },
        "textDocument": {
            "synchronization": { "dynamicRegistration": false, "didSave": true, "willSave": false, "willSaveWaitUntil": false },
            "completion": {
                "dynamicRegistration": false,
                "contextSupport": true,
                "completionItem": {
                    "snippetSupport": true,
                    "commitCharactersSupport": true,
                    "documentationFormat": ["markdown", "plaintext"],
                    "deprecatedSupport": true,
                    "preselectSupport": true,
                    "insertReplaceSupport": true,
                    "labelDetailsSupport": true,
                    "tagSupport": { "valueSet": [1] },
                    "resolveSupport": { "properties": ["documentation", "detail", "additionalTextEdits"] }
                },
                "completionItemKind": { "valueSet": (1..=25).collect::<Vec<u32>>() }
            },
            "hover": { "dynamicRegistration": false, "contentFormat": ["markdown", "plaintext"] },
            "signatureHelp": {
                "dynamicRegistration": false,
                "signatureInformation": {
                    "documentationFormat": ["markdown", "plaintext"],
                    "parameterInformation": { "labelOffsetSupport": true },
                    "activeParameterSupport": true
                }
            },
            "definition": { "dynamicRegistration": false, "linkSupport": true },
            "typeDefinition": { "dynamicRegistration": false, "linkSupport": true },
            "implementation": { "dynamicRegistration": false, "linkSupport": true },
            "references": { "dynamicRegistration": false },
            "documentHighlight": { "dynamicRegistration": false },
            "documentSymbol": {
                "dynamicRegistration": false,
                "hierarchicalDocumentSymbolSupport": true,
                "symbolKind": { "valueSet": (1..=26).collect::<Vec<u32>>() }
            },
            "codeAction": {
                "dynamicRegistration": false,
                "isPreferredSupport": true,
                "codeActionLiteralSupport": { "codeActionKind": { "valueSet": [
                    "", "quickfix", "refactor", "refactor.extract", "refactor.inline",
                    "refactor.rewrite", "source", "source.organizeImports", "source.fixAll"
                ] } }
            },
            "formatting": { "dynamicRegistration": false },
            "rangeFormatting": { "dynamicRegistration": false },
            "rename": { "dynamicRegistration": false, "prepareSupport": true },
            "diagnostic": { "dynamicRegistration": false, "relatedDocumentSupport": false },
            "publishDiagnostics": {
                "relatedInformation": true,
                "versionSupport": true,
                "codeDescriptionSupport": true,
                "tagSupport": { "valueSet": [1, 2] }
            },
            "semanticTokens": {
                "dynamicRegistration": false,
                "requests": { "range": false, "full": { "delta": false } },
                "tokenTypes": [
                    "namespace", "type", "class", "enum", "interface", "struct", "typeParameter",
                    "parameter", "variable", "property", "enumMember", "event", "function", "method",
                    "macro", "keyword", "modifier", "comment", "string", "number", "regexp", "operator"
                ],
                "tokenModifiers": [
                    "declaration", "definition", "readonly", "static", "deprecated", "abstract",
                    "async", "modification", "documentation", "defaultLibrary"
                ],
                "formats": ["relative"],
                "multilineTokenSupport": false,
                "overlappingTokenSupport": false
            }
        }
    })
}

async fn initialize(
    conn: &Connection,
    shared: &Shared,
    config: &LanguageServerConfig,
    timeout: Duration,
) -> Result<InitializeResult> {
    let params = json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "Tandem", "version": env!("CARGO_PKG_VERSION") },
        "locale": "en",
        "rootPath": shared.root.to_string_lossy(),
        "rootUri": shared.root_uri,
        "workspaceFolders": [{ "uri": shared.root_uri, "name": folder_name(&shared.root) }],
        "initializationOptions": config.initialization_options,
        "capabilities": client_capabilities(),
        "trace": "off",
    });
    let result = conn.request(req::Initialize::METHOD, params, timeout).await?;
    let result: InitializeResult = serde_json::from_value(result)?;
    conn.notify(notif::Initialized::METHOD, json!({}))?;
    Ok(result)
}

#[derive(Clone, Debug)]
struct OpenDoc {
    language_id: String,
    version: i32,
    text: String,
}

struct Inner {
    config: LanguageServerConfig,
    command: ServerCommand,
    shared: Arc<Shared>,
    options: ClientOptions,
    conn: RwLock<Arc<Connection>>,
    init: RwLock<InitializeResult>,
    docs: Mutex<HashMap<PathBuf, OpenDoc>>,
    exit_tx: mpsc::UnboundedSender<ExitEvent>,
    restarts: AtomicU32,
    next_generation: AtomicU64,
    runtime: tokio::runtime::Handle,
}

impl Inner {
    fn conn(&self) -> Arc<Connection> {
        self.conn.read().clone()
    }

    async fn restart(&self, attempt: u32) -> Result<()> {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let conn = spawn_connection(&self.shared, &self.command, &self.config.env, generation, self.exit_tx.clone())?;
        let init = initialize(&conn, &self.shared, &self.config, self.options.init_timeout).await?;
        *self.init.write() = init;
        {
            let docs = self.docs.lock();
            *self.conn.write() = conn.clone();
            for (path, doc) in docs.iter() {
                let Ok(uri) = path_to_uri(path) else { continue };
                let item = TextDocumentItem::new(uri, doc.language_id.clone(), doc.version, doc.text.clone());
                conn.notify(notif::DidOpenTextDocument::METHOD, json!({ "textDocument": item }))?;
            }
        }
        self.shared.broadcast(ServerNotification::Restarted { attempt });
        Ok(())
    }
}

async fn supervise(inner: Weak<Inner>, mut exit_rx: mpsc::UnboundedReceiver<ExitEvent>) {
    while let Some(ev) = exit_rx.recv().await {
        let Some(this) = inner.upgrade() else { return };
        if ev.generation != this.conn().generation {
            continue; // stale process (already replaced)
        }
        let attempt = this.restarts.load(Ordering::Relaxed);
        let restart = !this.shared.shutting_down.load(Ordering::Relaxed) && attempt < this.options.max_restarts;
        if !restart {
            this.shared.dead.store(true, Ordering::Relaxed);
        }
        this.shared.broadcast(ServerNotification::Exited { status: ev.status, restarting: restart });
        if !restart {
            continue;
        }
        tracing::warn!(server = %this.config.id, status = ?ev.status, "language server exited; restarting");
        this.restarts.store(attempt + 1, Ordering::Relaxed);
        let delay = this.options.restart_backoff.saturating_mul(1u32 << attempt.min(16));
        drop(this);
        tokio::time::sleep(delay).await;
        let Some(this) = inner.upgrade() else { return };
        if this.shared.shutting_down.load(Ordering::Relaxed) {
            this.shared.dead.store(true, Ordering::Relaxed);
            continue;
        }
        if let Err(e) = this.restart(attempt + 1).await {
            tracing::error!(server = %this.config.id, "restart failed: {e}");
            this.shared.dead.store(true, Ordering::Relaxed);
            this.shared.broadcast(ServerNotification::Exited { status: None, restarting: false });
        }
    }
}

/// Handle to one running language server. Cheap to clone; the process is killed when
/// the last handle is dropped (call [`shutdown`](Self::shutdown) for a polite exit).
#[derive(Clone)]
pub struct LspClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for LspClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspClient")
            .field("server", &self.inner.config.id)
            .field("root", &self.inner.shared.root)
            .finish()
    }
}

fn text_doc(path: &Path) -> Result<TextDocumentIdentifier> {
    Ok(TextDocumentIdentifier::new(path_to_uri(path)?))
}

fn tdpp(path: &Path, position: Position) -> Result<TextDocumentPositionParams> {
    Ok(TextDocumentPositionParams::new(text_doc(path)?, position))
}

impl LspClient {
    /// Spawn the server for `root` (absolute project directory) and complete the
    /// `initialize`/`initialized` handshake.
    pub async fn start(config: LanguageServerConfig, root: impl AsRef<Path>) -> Result<Self> {
        Self::start_with_options(config, root, ClientOptions::default()).await
    }

    pub async fn start_with_options(
        config: LanguageServerConfig,
        root: impl AsRef<Path>,
        options: ClientOptions,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let root_uri = path_to_uri(&root)?;
        let candidates = config.resolve_commands();
        if candidates.is_empty() {
            return Err(LspError::NotInstalled { server: config.id.clone(), hint: config.install_hint.clone() });
        }
        let shared = Arc::new(Shared {
            subscribers: Mutex::new(Subscribers { list: vec![], backlog: Some(vec![]) }),
            shutting_down: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            root,
            root_uri,
            server_id: config.id.clone(),
        });
        let (exit_tx, exit_rx) = mpsc::unbounded_channel();
        let mut last_err = None;
        let mut started = None;
        for (generation, command) in candidates.into_iter().enumerate() {
            let attempt = async {
                let conn = spawn_connection(&shared, &command, &config.env, generation as u64, exit_tx.clone())?;
                let init = initialize(&conn, &shared, &config, options.init_timeout).await?;
                Ok::<_, LspError>((conn, init))
            };
            match attempt.await {
                Ok((conn, init)) => {
                    started = Some((command, conn, init));
                    break;
                }
                Err(e) => {
                    tracing::info!(server = %config.id, program = %command.program, "candidate failed: {e}");
                    last_err = Some(e);
                    // Drop the failed candidate's handshake chatter.
                    if let Some(b) = shared.subscribers.lock().backlog.as_mut() {
                        b.clear();
                    }
                }
            }
        }
        let Some((command, conn, init)) = started else {
            return Err(last_err.unwrap_or(LspError::ServerExited));
        };
        let generation = conn.generation;
        let inner = Arc::new(Inner {
            config,
            command,
            shared,
            options,
            conn: RwLock::new(conn),
            init: RwLock::new(init),
            docs: Mutex::new(HashMap::new()),
            exit_tx,
            restarts: AtomicU32::new(0),
            next_generation: AtomicU64::new(generation + 1),
            runtime: tokio::runtime::Handle::current(),
        });
        tokio::spawn(supervise(Arc::downgrade(&inner), exit_rx));
        Ok(Self { inner })
    }

    pub fn config(&self) -> &LanguageServerConfig {
        &self.inner.config
    }

    pub fn root(&self) -> &Path {
        &self.inner.shared.root
    }

    pub fn root_uri(&self) -> &Uri {
        &self.inner.shared.root_uri
    }

    /// The launch command actually used (resolved absolute program path).
    pub fn command(&self) -> &ServerCommand {
        &self.inner.command
    }

    pub fn capabilities(&self) -> ServerCapabilities {
        self.inner.init.read().capabilities.clone()
    }

    pub fn server_info(&self) -> Option<ServerInfo> {
        self.inner.init.read().server_info.clone()
    }

    pub fn is_running(&self) -> bool {
        self.inner.conn().is_alive()
    }

    /// The server is gone for good (shut down, or crashed with no restarts left).
    pub fn is_dead(&self) -> bool {
        self.inner.shared.dead.load(Ordering::Relaxed)
    }

    /// Number of automatic restarts performed so far.
    pub fn restart_count(&self) -> u32 {
        self.inner.restarts.load(Ordering::Relaxed)
    }

    /// New receiver that gets every subsequent notification (broadcast; each subscriber
    /// sees all events). The first subscriber also receives everything emitted since
    /// start (handshake logs, progress). Dropping the receiver unsubscribes.
    pub fn subscribe(&self) -> async_channel::Receiver<ServerNotification> {
        let (tx, rx) = async_channel::unbounded();
        let mut subs = self.inner.shared.subscribers.lock();
        for n in subs.backlog.take().unwrap_or_default() {
            let _ = tx.try_send(n);
        }
        subs.list.push(tx);
        rx
    }

    // ---- raw / typed JSON-RPC ----

    pub async fn request_raw(&self, method: &str, params: Value) -> Result<Value> {
        self.request_raw_timeout(method, params, self.inner.options.request_timeout).await
    }

    pub async fn request_raw_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let conn = self.inner.conn();
        conn.request(method, params, timeout).await
    }

    pub fn notify_raw(&self, method: &str, params: Value) -> Result<()> {
        self.inner.conn().notify(method, params)
    }

    pub async fn request<R: Request>(&self, params: R::Params) -> Result<R::Result> {
        let value = self.request_raw(R::METHOD, serde_json::to_value(params)?).await?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn notify<N: Notification>(&self, params: N::Params) -> Result<()> {
        self.notify_raw(N::METHOD, serde_json::to_value(params)?)
    }

    // ---- document sync ----

    /// Text most recently sent for an open document.
    pub fn document_text(&self, path: &Path) -> Option<String> {
        self.inner.docs.lock().get(path).map(|d| d.text.clone())
    }

    pub fn document_version(&self, path: &Path) -> Option<i32> {
        self.inner.docs.lock().get(path).map(|d| d.version)
    }

    pub fn did_open(&self, path: &Path, language_id: &str, text: &str) -> Result<()> {
        let uri = path_to_uri(path)?;
        let mut docs = self.inner.docs.lock();
        let doc = OpenDoc { language_id: language_id.to_string(), version: 0, text: text.to_string() };
        let item = TextDocumentItem::new(uri, doc.language_id.clone(), doc.version, doc.text.clone());
        docs.insert(path.to_path_buf(), doc);
        self.inner.conn().notify(notif::DidOpenTextDocument::METHOD, json!({ "textDocument": item }))?;
        drop(docs);
        self.schedule_pull_diagnostics(path, 0);
        Ok(())
    }

    /// Full-document sync. `version` must increase with every change.
    pub fn did_change_full(&self, path: &Path, version: i32, text: &str) -> Result<()> {
        let uri = path_to_uri(path)?;
        let mut docs = self.inner.docs.lock();
        if let Some(doc) = docs.get_mut(path) {
            doc.version = version;
            doc.text = text.to_string();
        }
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(uri, version),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_string(),
            }],
        };
        self.inner.conn().notify(notif::DidChangeTextDocument::METHOD, serde_json::to_value(params)?)?;
        drop(docs);
        self.schedule_pull_diagnostics(path, version);
        Ok(())
    }

    /// Incremental sync: apply `edits` (LSP ranges relative to the previous text, in
    /// order) locally and send them as-is. Falls back to full sync if the server only
    /// supports full sync.
    pub fn did_change(&self, path: &Path, version: i32, edits: Vec<TextDocumentContentChangeEvent>) -> Result<()> {
        let new_text = {
            let docs = self.inner.docs.lock();
            let mut text = docs.get(path).map(|d| d.text.clone()).unwrap_or_default();
            for e in &edits {
                match e.range {
                    Some(r) => {
                        let span = crate::range_to_offsets(&text, r);
                        text.replace_range(span, &e.text);
                    }
                    None => text = e.text.clone(),
                }
            }
            text
        };
        if !self.supports_incremental_sync() {
            return self.did_change_full(path, version, &new_text);
        }
        let uri = path_to_uri(path)?;
        let mut docs = self.inner.docs.lock();
        if let Some(doc) = docs.get_mut(path) {
            doc.version = version;
            doc.text = new_text;
        }
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(uri, version),
            content_changes: edits,
        };
        self.inner.conn().notify(notif::DidChangeTextDocument::METHOD, serde_json::to_value(params)?)?;
        drop(docs);
        self.schedule_pull_diagnostics(path, version);
        Ok(())
    }

    /// Pull-model diagnostics (`textDocument/diagnostic`, LSP 3.17). Returns `None` when
    /// the server doesn't support it or reports "unchanged".
    pub async fn pull_diagnostics(&self, path: &Path) -> Result<Option<Vec<Diagnostic>>> {
        if self.inner.init.read().capabilities.diagnostic_provider.is_none() {
            return Ok(None);
        }
        let params = json!({ "textDocument": { "uri": path_to_uri(path)? } });
        let result = self.request_raw(req::DocumentDiagnosticRequest::METHOD, params).await?;
        if result.get("kind").and_then(Value::as_str) != Some("full") {
            return Ok(None);
        }
        let items = result.get("items").cloned().unwrap_or(Value::Array(vec![]));
        Ok(Some(serde_json::from_value(items)?))
    }

    /// For pull-model servers, fetch diagnostics after open/change and broadcast them as
    /// [`ServerNotification::Diagnostics`] so consumers see one uniform stream.
    fn schedule_pull_diagnostics(&self, path: &Path, version: i32) {
        if self.inner.init.read().capabilities.diagnostic_provider.is_none() {
            return;
        }
        let client = self.clone();
        let path = path.to_path_buf();
        self.inner.runtime.spawn(async move {
            match client.pull_diagnostics(&path).await {
                Ok(Some(diagnostics)) => {
                    // Drop stale results: the document changed (or closed) meanwhile.
                    if client.document_version(&path) != Some(version) {
                        return;
                    }
                    let Ok(uri) = path_to_uri(&path) else { return };
                    client.inner.shared.broadcast(ServerNotification::Diagnostics {
                        uri,
                        path: Some(path),
                        version: Some(version),
                        diagnostics,
                    });
                }
                Ok(None) => {}
                Err(e) if e.is_cancelled() => {}
                Err(e) => tracing::debug!("pull diagnostics for {}: {e}", path.display()),
            }
        });
    }

    fn supports_incremental_sync(&self) -> bool {
        let kind = match &self.inner.init.read().capabilities.text_document_sync {
            Some(TextDocumentSyncCapability::Kind(k)) => Some(*k),
            Some(TextDocumentSyncCapability::Options(o)) => o.change,
            None => None,
        };
        kind == Some(TextDocumentSyncKind::INCREMENTAL)
    }

    fn save_includes_text(&self) -> bool {
        match &self.inner.init.read().capabilities.text_document_sync {
            Some(TextDocumentSyncCapability::Options(o)) => matches!(
                &o.save,
                Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions { include_text: Some(true) }))
            ),
            _ => false,
        }
    }

    pub fn did_save(&self, path: &Path) -> Result<()> {
        let uri = path_to_uri(path)?;
        let docs = self.inner.docs.lock();
        let text = if self.save_includes_text() { docs.get(path).map(|d| d.text.clone()) } else { None };
        let params = DidSaveTextDocumentParams { text_document: TextDocumentIdentifier::new(uri), text };
        self.inner.conn().notify(notif::DidSaveTextDocument::METHOD, serde_json::to_value(params)?)
    }

    pub fn did_close(&self, path: &Path) -> Result<()> {
        let uri = path_to_uri(path)?;
        let mut docs = self.inner.docs.lock();
        docs.remove(path);
        let params = DidCloseTextDocumentParams { text_document: TextDocumentIdentifier::new(uri) };
        self.inner.conn().notify(notif::DidCloseTextDocument::METHOD, serde_json::to_value(params)?)
    }

    // ---- language features ----

    pub async fn completion(&self, path: &Path, position: Position) -> Result<Vec<CompletionItem>> {
        let params = CompletionParams {
            text_document_position: tdpp(path, position)?,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: Some(CompletionContext { trigger_kind: CompletionTriggerKind::INVOKED, trigger_character: None }),
        };
        Ok(match self.request::<req::Completion>(params).await? {
            Some(CompletionResponse::Array(items)) => items,
            Some(CompletionResponse::List(list)) => list.items,
            None => vec![],
        })
    }

    /// `completionItem/resolve`; returns the item unchanged if the server can't resolve.
    pub async fn resolve_completion(&self, item: CompletionItem) -> Result<CompletionItem> {
        let can = self
            .inner
            .init
            .read()
            .capabilities
            .completion_provider
            .as_ref()
            .and_then(|c| c.resolve_provider)
            .unwrap_or(false);
        if !can {
            return Ok(item);
        }
        self.request::<req::ResolveCompletionItem>(item).await
    }

    pub async fn hover(&self, path: &Path, position: Position) -> Result<Option<Hover>> {
        let params = HoverParams { text_document_position_params: tdpp(path, position)?, work_done_progress_params: Default::default() };
        self.request::<req::HoverRequest>(params).await
    }

    pub async fn definition(&self, path: &Path, position: Position) -> Result<Vec<Location>> {
        let params = GotoDefinitionParams {
            text_document_position_params: tdpp(path, position)?,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        Ok(match self.request::<req::GotoDefinition>(params).await? {
            None => vec![],
            Some(GotoDefinitionResponse::Scalar(l)) => vec![l],
            Some(GotoDefinitionResponse::Array(v)) => v,
            Some(GotoDefinitionResponse::Link(links)) => links
                .into_iter()
                .map(|l| Location::new(l.target_uri, l.target_selection_range))
                .collect(),
        })
    }

    pub async fn references(&self, path: &Path, position: Position, include_declaration: bool) -> Result<Vec<Location>> {
        let params = ReferenceParams {
            text_document_position: tdpp(path, position)?,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext { include_declaration },
        };
        Ok(self.request::<req::References>(params).await?.unwrap_or_default())
    }

    /// Returns an empty edit when the server reports nothing to rename.
    pub async fn rename(&self, path: &Path, position: Position, new_name: &str) -> Result<WorkspaceEdit> {
        let params = RenameParams {
            text_document_position: tdpp(path, position)?,
            new_name: new_name.to_string(),
            work_done_progress_params: Default::default(),
        };
        Ok(self.request::<req::Rename>(params).await?.unwrap_or_default())
    }

    pub async fn document_symbols(&self, path: &Path) -> Result<Option<DocumentSymbolResponse>> {
        let params = DocumentSymbolParams {
            text_document: text_doc(path)?,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.request::<req::DocumentSymbolRequest>(params).await
    }

    /// Workspace symbols, normalized to `WorkspaceSymbol` (flat results are converted).
    pub async fn workspace_symbols(&self, query: &str) -> Result<Vec<WorkspaceSymbol>> {
        let params = WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        Ok(match self.request::<req::WorkspaceSymbolRequest>(params).await? {
            None => vec![],
            Some(WorkspaceSymbolResponse::Nested(v)) => v,
            Some(WorkspaceSymbolResponse::Flat(v)) => v
                .into_iter()
                .map(|s| WorkspaceSymbol {
                    name: s.name,
                    kind: s.kind,
                    tags: s.tags,
                    container_name: s.container_name,
                    location: OneOf::Left(s.location),
                    data: None,
                })
                .collect(),
        })
    }

    pub async fn code_actions(&self, path: &Path, range: Range, diagnostics: Vec<Diagnostic>) -> Result<Vec<CodeActionOrCommand>> {
        let params = CodeActionParams {
            text_document: text_doc(path)?,
            range,
            context: CodeActionContext { diagnostics, only: None, trigger_kind: Some(CodeActionTriggerKind::INVOKED) },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        Ok(self.request::<req::CodeActionRequest>(params).await?.unwrap_or_default())
    }

    pub async fn format_document(&self, path: &Path, tab_size: u32, insert_spaces: bool) -> Result<Vec<TextEdit>> {
        let params = DocumentFormattingParams {
            text_document: text_doc(path)?,
            options: FormattingOptions {
                tab_size,
                insert_spaces,
                trim_trailing_whitespace: Some(true),
                insert_final_newline: Some(true),
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        };
        Ok(self.request::<req::Formatting>(params).await?.unwrap_or_default())
    }

    pub async fn signature_help(&self, path: &Path, position: Position) -> Result<Option<SignatureHelp>> {
        let params = SignatureHelpParams {
            context: None,
            text_document_position_params: tdpp(path, position)?,
            work_done_progress_params: Default::default(),
        };
        self.request::<req::SignatureHelpRequest>(params).await
    }

    // ---- lifecycle ----

    /// Polite `shutdown` request + `exit` notification, then kill if the process lingers.
    /// Disables auto-restart. Idempotent.
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.shared.shutting_down.store(true, Ordering::Relaxed);
        let conn = self.inner.conn();
        if !conn.is_alive() {
            return Ok(());
        }
        let timeout = self.inner.options.shutdown_timeout;
        let polite = conn.request(req::Shutdown::METHOD, Value::Null, timeout).await;
        if polite.is_ok() {
            let _ = conn.notify(notif::Exit::METHOD, Value::Null);
        }
        let mut exited = conn.exited.clone();
        let timed_out = tokio::time::timeout(timeout, exited.wait_for(|e| *e)).await.is_err();
        if timed_out {
            if let Some(k) = conn.kill.lock().take() {
                let _ = k.send(());
            }
            let _ = tokio::time::timeout(timeout, exited.wait_for(|e| *e)).await;
        }
        self.inner.shared.dead.store(true, Ordering::Relaxed);
        Ok(())
    }
}
