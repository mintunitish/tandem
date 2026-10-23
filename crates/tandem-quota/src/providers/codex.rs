//! Codex: `codex app-server` JSON-RPC 2.0 over stdio (NDJSON).
//!
//! Verified against codex 0.147 (`codex app-server generate-json-schema`):
//! `initialize{clientInfo}` → `initialized` notification → `account/rateLimits/read`
//! (`{rateLimits{primary,secondary{usedPercent,windowDurationMins,resetsAt},planType,
//! credits,rateLimitReachedType,spendControlReached}, rateLimitsByLimitId}`); pushes
//! `account/rateLimits/updated` (sparse) and `thread/tokenUsage/updated`.
//! The app-server uses its own login; Tandem never touches `~/.codex/auth.json`.

use super::{codex_logs, get, get_f64, raw_json, session_windows, usage_from_json};
use crate::model::{duration_label, parse_ts, QuotaSnapshot, Source, Status, Unit, Window, WindowKind};
use crate::provider::{ProviderCore, QuotaProvider};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tandem_core::{AgentEvent, AgentId, Receiver, Usage};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

pub const AGENT: &str = "codex";

#[derive(Clone, Debug)]
pub struct CodexOptions {
    pub program: String,
    pub args: Vec<String>,
    /// `~/.codex/sessions` (fallback logs). `None` disables the fallback.
    pub sessions_dir: Option<PathBuf>,
    pub request_timeout: Duration,
}
impl Default for CodexOptions {
    fn default() -> Self {
        Self {
            program: "codex".into(),
            args: vec!["app-server".into()],
            sessions_dir: dirs::home_dir().map(|h| h.join(".codex").join("sessions")),
            request_timeout: Duration::from_secs(20),
        }
    }
}

// ---------------------------------------------------------------- parsing

fn window_from(v: &Value, fallback_label: &str, source: Source, now: DateTime<Utc>) -> Option<Window> {
    let used = get_f64(v, &["usedPercent", "used_percent"])?;
    let mins = get_f64(v, &["windowDurationMins", "window_minutes", "windowMinutes"]).map(|m| m as i64);
    let label = mins.map(duration_label).unwrap_or_else(|| fallback_label.to_string());
    let mut w = Window::percent(label, used, source, now).with_reset(get(v, &["resetsAt", "resets_at"]).and_then(parse_ts));
    w.window_mins = mins;
    Some(w)
}

/// Maps one `RateLimitSnapshot` (camelCase from app-server, snake_case from session logs)
/// to a delta. `sparse` = `account/rateLimits/updated`, where nulls mean "unchanged".
pub fn parse_rate_limits(rl: &Value, source: Source, now: DateTime<Utc>, sparse: bool) -> QuotaSnapshot {
    let mut d = QuotaSnapshot::empty(AGENT.into(), now);
    d.plan = get(rl, &["planType", "plan_type"]).and_then(|p| p.as_str()).map(String::from);
    for (k, fb) in [("primary", "primary"), ("secondary", "secondary")] {
        if let Some(w) = rl.get(k).and_then(|v| window_from(v, fb, source, now)) {
            d.windows.push(w);
        }
    }
    if let Some(c) = rl.get("credits").filter(|c| c.is_object()) {
        let unlimited = get(c, &["unlimited"]).and_then(|x| x.as_bool()).unwrap_or(false);
        if let Some(bal) = get_f64(c, &["balance"]).filter(|_| !unlimited) {
            d.windows.push(Window::new("credits", Unit::Credits, source, now).with_kind(WindowKind::Usage).with_amounts(None, Some(bal)));
        }
    }
    let reached = get(rl, &["rateLimitReachedType", "rate_limit_reached_type"]).is_some()
        || get(rl, &["spendControlReached", "spend_control_reached"]).and_then(|x| x.as_bool()) == Some(true);
    if reached {
        d.status = Status::Rejected;
        let full = d.windows.iter().filter(|w| w.used_fraction.is_some_and(|f| f >= 1.0)).filter_map(|w| w.reset_at).min();
        d.status_until = full.or_else(|| d.windows.iter().filter_map(|w| w.reset_at).min()).or(Some(now + chrono::Duration::hours(1)));
    } else if !sparse {
        d.status = Status::Ok;
    }
    d
}

/// Full `account/rateLimits/read` result (also covers extra limit ids).
pub fn parse_read_response(res: &Value, source: Source, now: DateTime<Utc>) -> QuotaSnapshot {
    let main = res.get("rateLimits").cloned().unwrap_or(Value::Null);
    let mut d = parse_rate_limits(&main, source, now, false);
    let main_id = main.get("limitId").and_then(|x| x.as_str()).unwrap_or("codex").to_string();
    if let Some(by) = res.get("rateLimitsByLimitId").and_then(|x| x.as_object()) {
        for (id, rl) in by.iter().filter(|(id, _)| **id != main_id) {
            for mut w in parse_rate_limits(rl, source, now, false).windows {
                w.label = format!("{id} {}", w.label);
                d.windows.push(w);
            }
        }
    }
    d
}

/// `thread/tokenUsage/updated` params → context + session windows.
pub fn parse_token_usage(params: &Value, source: Source, now: DateTime<Utc>) -> QuotaSnapshot {
    let mut d = QuotaSnapshot::empty(AGENT.into(), now);
    let tu = params.get("tokenUsage").or_else(|| params.get("info")).unwrap_or(params);
    let ctx = get_f64(tu, &["modelContextWindow", "model_context_window"]);
    if let Some(last) = get(tu, &["last", "last_token_usage"]) {
        let used = get_f64(last, &["totalTokens", "total_tokens"]);
        d.windows.push(Window::new("context", Unit::Tokens, source, now).with_kind(WindowKind::Context).with_amounts(used, ctx));
    }
    // Codex `inputTokens` already include cached input, so use `totalTokens` as is.
    if let Some(total) = get(tu, &["total", "total_token_usage"]).and_then(|t| get_f64(t, &["totalTokens", "total_tokens"])) {
        d.windows.push(Window::new("session", Unit::Tokens, source, now).with_kind(WindowKind::Usage).with_amounts(Some(total), None));
    }
    d
}

// ---------------------------------------------------------------- JSON-RPC client

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

/// Minimal NDJSON JSON-RPC 2.0 client for `codex app-server`.
pub struct AppServerClient {
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    pending: Arc<Pending>,
    next_id: AtomicU64,
    child: Mutex<Option<Child>>,
    reader: tokio::task::JoinHandle<()>,
}

impl AppServerClient {
    pub async fn spawn(opts: &CodexOptions, on_notify: impl Fn(&str, &Value) + Send + Sync + 'static) -> Result<Self> {
        let mut child = Command::new(&opts.program)
            .args(&opts.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {} {}", opts.program, opts.args.join(" ")))?;
        let stdin = Arc::new(tokio::sync::Mutex::new(child.stdin.take().context("no stdin")?));
        let stdout = child.stdout.take().context("no stdout")?;
        let pending: Arc<Pending> = Arc::default();
        let (p2, in2) = (pending.clone(), stdin.clone());
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                let method = msg.get("method").and_then(|m| m.as_str());
                match (msg.get("id").filter(|i| !i.is_null()), method) {
                    (Some(id), Some(_)) => {
                        // Server → client request (approvals etc.): we are a read-only client.
                        let resp = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"tandem-quota: unsupported"}});
                        let mut w = in2.lock().await;
                        let _ = w.write_all(format!("{resp}\n").as_bytes()).await;
                    }
                    (Some(id), None) => {
                        let Some(id) = id.as_u64() else { continue };
                        if let Some(tx) = p2.lock().remove(&id) {
                            let r = match msg.get("error") {
                                Some(e) => Err(e.get("message").and_then(|m| m.as_str()).unwrap_or("error").to_string()),
                                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                            };
                            let _ = tx.send(r);
                        }
                    }
                    (None, Some(m)) => on_notify(m, msg.get("params").unwrap_or(&Value::Null)),
                    _ => {}
                }
            }
            for (_, tx) in p2.lock().drain() {
                let _ = tx.send(Err("codex app-server exited".into()));
            }
        });
        Ok(Self { stdin, pending, next_id: AtomicU64::new(1), child: Mutex::new(Some(child)), reader })
    }

    async fn write(&self, msg: &Value) -> Result<()> {
        let mut w = self.stdin.lock().await;
        w.write_all(format!("{msg}\n").as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }

    pub async fn request(&self, method: &str, params: Option<Value>, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        let mut msg = json!({"jsonrpc":"2.0","id":id,"method":method});
        if let Some(p) = params {
            msg["params"] = p;
        }
        self.write(&msg).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => bail!("{method}: {e}"),
            Ok(Err(_)) => bail!("{method}: connection closed"),
            Err(_) => {
                self.pending.lock().remove(&id);
                bail!("{method}: timed out after {timeout:?}")
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let mut msg = json!({"jsonrpc":"2.0","method":method});
        if let Some(p) = params {
            msg["params"] = p;
        }
        self.write(&msg).await
    }

    pub async fn initialize(&self, timeout: Duration) -> Result<Value> {
        let params = json!({
            "clientInfo": {"name": "tandem", "title": "Tandem", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": false}
        });
        let res = self.request("initialize", Some(params), timeout).await?;
        self.notify("initialized", None).await?;
        Ok(res)
    }
}

impl Drop for AppServerClient {
    fn drop(&mut self) {
        self.reader.abort();
        if let Some(mut c) = self.child.lock().take() {
            let _ = c.start_kill();
        }
    }
}

// ---------------------------------------------------------------- provider

pub struct CodexProvider {
    core: Arc<ProviderCore>,
    opts: CodexOptions,
    client: tokio::sync::Mutex<Option<Arc<AppServerClient>>>,
    session: Mutex<Usage>,
    me: Weak<CodexProvider>,
}

fn handle_notification(core: &ProviderCore, method: &str, params: &Value) {
    let now = Utc::now();
    match method {
        "account/rateLimits/updated" => {
            if let Some(rl) = params.get("rateLimits") {
                core.apply(parse_rate_limits(rl, Source::AgentRpc, now, true));
            }
        }
        "thread/tokenUsage/updated" => {
            core.apply(parse_token_usage(params, Source::AgentRpc, now));
        }
        _ => {}
    }
}

impl CodexProvider {
    pub fn new(opts: CodexOptions) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            core: Arc::new(ProviderCore::new(AGENT.into())),
            opts,
            client: tokio::sync::Mutex::new(None),
            session: Mutex::new(Usage::default()),
            me: me.clone(),
        })
    }

    /// Loads the session-log fallback, then connects to the app-server and reads rate
    /// limits. App-server failures are recorded in the snapshot's `error`.
    pub async fn start(&self) -> Result<()> {
        self.load_logs();
        let r = self.connect().await;
        if let Err(e) = &r {
            tracing::info!("codex app-server unavailable: {e:#}");
            self.core.set_error(format!("codex app-server: {e:#}"));
        }
        r
    }

    /// Reads the newest `~/.codex/sessions/**/*.jsonl` `token_count` row (Source::LocalLog).
    pub fn load_logs(&self) -> bool {
        let Some(dir) = &self.opts.sessions_dir else { return false };
        match codex_logs::latest_from_dir(dir) {
            Some(d) => {
                self.core.apply(d);
                true
            }
            None => false,
        }
    }

    async fn connect(&self) -> Result<()> {
        let core = self.core.clone();
        let client = AppServerClient::spawn(&self.opts, move |m, p| handle_notification(&core, m, p)).await?;
        client.initialize(self.opts.request_timeout).await.context("initialize")?;
        let client = Arc::new(client);
        *self.client.lock().await = Some(client.clone());
        self.read_rate_limits(&client).await
    }

    async fn read_rate_limits(&self, client: &AppServerClient) -> Result<()> {
        let res = client.request("account/rateLimits/read", None, self.opts.request_timeout).await?;
        if res.get("rateLimits").is_none() {
            return Err(anyhow!("account/rateLimits/read: no rateLimits in {res}"));
        }
        self.core.apply(parse_read_response(&res, Source::AgentRpc, Utc::now()));
        Ok(())
    }

    async fn reconcile_inner(&self) {
        let client = self.client.lock().await.clone();
        let r = match client {
            Some(c) => self.read_rate_limits(&c).await,
            None => self.connect().await,
        };
        if let Err(e) = r {
            *self.client.lock().await = None;
            self.core.set_error(format!("codex app-server: {e:#}"));
            self.load_logs();
        }
    }
}

impl QuotaProvider for CodexProvider {
    fn agent(&self) -> AgentId {
        self.core.agent()
    }
    fn snapshot(&self) -> BoxFuture<'_, QuotaSnapshot> {
        Box::pin(async move { self.core.current() })
    }
    fn subscribe(&self) -> Receiver<QuotaSnapshot> {
        self.core.subscribe()
    }
    fn reconcile(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.reconcile_inner())
    }
    fn ingest_event(&self, ev: &AgentEvent) {
        let now = Utc::now();
        match ev {
            AgentEvent::Usage(u) => {
                let total = {
                    let mut s = self.session.lock();
                    s.add(u);
                    s.clone()
                };
                let mut d = self.core.delta();
                d.windows = session_windows(&total, Source::AgentEvent, now);
                self.core.apply(d);
            }
            AgentEvent::Raw { json, .. } => {
                let Some(v) = raw_json(json) else { return };
                if let Some(m) = v.get("method").and_then(|m| m.as_str()) {
                    // App-server traffic captured by another client (e.g. tandem-acp).
                    handle_notification(&self.core, m, v.get("params").unwrap_or(&Value::Null));
                } else if v.get("type").and_then(|t| t.as_str()) == Some("turn.completed") {
                    // `codex exec --json`.
                    if let Some(mut u) = v.get("usage").and_then(usage_from_json) {
                        u.input_tokens = u.input_tokens.saturating_sub(u.cache_read_tokens); // cached ⊂ input
                        self.ingest_event(&AgentEvent::Usage(u));
                    }
                } else if let Some(d) = codex_logs::parse_row(&v) {
                    self.core.apply(d);
                }
            }
            AgentEvent::Done { .. } => {
                // Turn ended: re-read the authoritative numbers in the background.
                if let (Ok(rt), Some(me)) = (tokio::runtime::Handle::try_current(), self.me.upgrade()) {
                    rt.spawn(async move { me.reconcile_inner().await });
                }
            }
            _ => {}
        }
    }
}
