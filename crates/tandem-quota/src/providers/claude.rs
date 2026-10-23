//! Claude Code.
//!
//! Signals, best first:
//! * stream-json `rate_limit_event` (`rate_limit_info{status, resetsAt, rateLimitType,
//!   utilization?, unifiedWindows{five_hour,seven_day,..{utilization,resetsAt}}}`) —
//!   claude 2.1.309 sends `unifiedWindows` utilization even when `allowed`. AgentEvent.
//! * status-line sidecar: `--settings` with a `statusLine` command running
//!   `tandem-quota-sink <file>`; we watch the file (1 s debounce) and map
//!   `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at}`. AgentEvent.
//!   Verified 2026-10-24: does NOT fire under `claude -p`; interactive/ACP path only.
//! * `~/.claude/projects/**/*.jsonl` 5h blocks (see `claude_logs`). Heuristic.

use super::acp::parse_usage_update;
use super::claude_logs::{self, Calibration};
use super::{get, get_f64, raw_json, session_windows, usage_from_json};
use crate::model::{parse_ts, QuotaSnapshot, Source, Status, Unit, Window, WindowKind};
use crate::persist;
use crate::provider::{ProviderCore, QuotaProvider};
use crate::safe_fs;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use notify::{RecursiveMode, Watcher};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tandem_core::{AgentEvent, AgentId, Receiver, Usage};

pub const AGENT: &str = "claude";
pub const SINK_BIN: &str = "tandem-quota-sink";

/// `five_hour` → "5h", `seven_day` → "weekly", `seven_day_opus` → "weekly-opus".
pub fn claude_window_label(kind: &str) -> String {
    match kind {
        "five_hour" => "5h".into(),
        "seven_day" => "weekly".into(),
        k => match k.strip_prefix("seven_day_") {
            Some(rest) => format!("weekly-{}", rest.replace('_', "-")),
            None => k.replace('_', "-"),
        },
    }
}

/// Utilization may be a fraction (0.22) or a percent (22); returns percent.
fn as_percent(u: f64) -> f64 {
    if u > 2.0 { u } else { u * 100.0 }
}

/// Parses a stream-json `rate_limit_event` (or its bare `rate_limit_info`).
pub fn parse_rate_limit_event(v: &Value, now: DateTime<Utc>) -> Option<QuotaSnapshot> {
    let info = v.get("rate_limit_info").unwrap_or(v);
    let status = info.get("status").and_then(|s| s.as_str())?;
    let mut d = QuotaSnapshot::empty(AGENT.into(), now);
    let resets = info.get("resetsAt").and_then(parse_ts);
    if let Some(ws) = info.get("unifiedWindows").and_then(|w| w.as_object()) {
        for (k, w) in ws {
            if let Some(u) = get_f64(w, &["utilization"]) {
                d.windows.push(Window::percent(claude_window_label(k), as_percent(u), Source::AgentEvent, now).with_reset(w.get("resetsAt").and_then(parse_ts)));
            }
        }
    }
    let label = info.get("rateLimitType").and_then(|t| t.as_str()).map(claude_window_label);
    if let Some(label) = &label {
        let util = get_f64(info, &["utilization"]).map(as_percent);
        let util = util.or_else(|| (status == "rejected").then_some(100.0));
        match d.windows.iter_mut().find(|w| &w.label == label) {
            Some(w) => {
                if let Some(u) = util {
                    *w = Window::percent(label.clone(), u, Source::AgentEvent, now).with_reset(w.reset_at.or(resets));
                }
            }
            None => {
                let w = match util {
                    Some(u) => Window::percent(label.clone(), u, Source::AgentEvent, now),
                    None => Window::new(label.clone(), Unit::Percent, Source::AgentEvent, now),
                };
                d.windows.push(w.with_reset(resets));
            }
        }
    }
    d.status = match status {
        "allowed" => Status::Ok,
        "allowed_warning" => Status::Warning,
        "rejected" => Status::Rejected,
        _ => Status::Unknown,
    };
    if matches!(d.status, Status::Warning | Status::Rejected) {
        d.status_until = resets.or(Some(now + chrono::Duration::hours(1)));
    }
    Some(d)
}

/// Parses the status-line stdin JSON.
pub fn parse_statusline(v: &Value, now: DateTime<Utc>) -> QuotaSnapshot {
    let mut d = QuotaSnapshot::empty(AGENT.into(), now);
    if let Some(rl) = v.get("rate_limits").and_then(|r| r.as_object()) {
        for (k, w) in rl {
            if let Some(p) = get_f64(w, &["used_percentage", "used_percent"]) {
                d.windows.push(Window::percent(claude_window_label(k), p, Source::AgentEvent, now).with_reset(get(w, &["resets_at", "resetsAt"]).and_then(parse_ts)));
            }
        }
    }
    if let Some(cw) = v.get("context_window") {
        let size = get_f64(cw, &["context_window_size", "size"]);
        if let Some(p) = get_f64(cw, &["used_percentage"]) {
            let mut w = Window::new("context", Unit::Tokens, Source::AgentEvent, now).with_kind(WindowKind::Context);
            w.used_fraction = Some((p / 100.0).clamp(0.0, 1.0));
            w.limit = size;
            w.used = size.map(|s| s * p / 100.0);
            d.windows.push(w);
        }
    }
    if let Some(c) = v.get("cost").and_then(|c| get_f64(c, &["total_cost_usd"])) {
        d.windows.push(Window::new("session cost", Unit::Usd, Source::AgentEvent, now).with_kind(WindowKind::Usage).with_amounts(Some(c), None));
    }
    d
}

fn shell_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:@".contains(c)) {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// `{"statusLine":{"type":"command","command":"<sink> <out>"}}` for `claude --settings`.
pub fn statusline_settings(sink: &Path, out_file: &Path) -> Value {
    json!({"statusLine": {"type": "command", "command": format!("{} {}", shell_quote(sink), shell_quote(out_file)), "padding": 0}})
}

/// Writes the settings file and returns its path (pass as `claude --settings <path>`).
pub fn write_statusline_settings(settings_path: &Path, sink: &Path, out_file: &Path) -> Result<PathBuf> {
    let bytes = serde_json::to_vec_pretty(&statusline_settings(sink, out_file))?;
    safe_fs::atomic_write(settings_path, &bytes).with_context(|| format!("write {}", settings_path.display()))?;
    Ok(settings_path.to_path_buf())
}

/// The sink binary: next to the current executable (or its parent, for test/example
/// binaries in `target/*/deps|examples`), else `tandem-quota-sink` from PATH.
pub fn sink_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1).take(2) {
            let p = dir.join(SINK_BIN);
            if p.is_file() {
                return p;
            }
        }
    }
    PathBuf::from(SINK_BIN)
}

#[derive(Clone, Debug)]
pub struct ClaudeOptions {
    /// File the status-line sink writes; watched when set.
    pub statusline_file: Option<PathBuf>,
    /// `~/.claude/projects` for the 5h-block fallback; `None` disables it.
    pub projects_dir: Option<PathBuf>,
    pub calibration_path: Option<PathBuf>,
    pub debounce: Duration,
}
impl Default for ClaudeOptions {
    fn default() -> Self {
        Self {
            statusline_file: Some(persist::statusline_path()),
            projects_dir: dirs::home_dir().map(|h| h.join(".claude").join("projects")),
            calibration_path: Some(persist::calibration_path()),
            debounce: Duration::from_secs(1),
        }
    }
}

pub struct ClaudeProvider {
    core: Arc<ProviderCore>,
    opts: ClaudeOptions,
    session: Mutex<Usage>,
    seen: Mutex<HashSet<String>>,
    block: Mutex<Option<claude_logs::Block>>,
    calibration: Mutex<Calibration>,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    me: Weak<ClaudeProvider>,
}

impl ClaudeProvider {
    pub fn new(opts: ClaudeOptions) -> Arc<Self> {
        let calibration = opts.calibration_path.as_deref().map(Calibration::load).unwrap_or_default();
        Arc::new_cyclic(|me| Self {
            core: Arc::new(ProviderCore::new(AGENT.into())),
            opts,
            session: Mutex::new(Usage::default()),
            seen: Mutex::new(HashSet::new()),
            block: Mutex::new(None),
            calibration: Mutex::new(calibration),
            watcher: Mutex::new(None),
            me: me.clone(),
        })
    }

    /// Scans logs, reads any existing status-line file and starts the watcher.
    /// Must be called inside a tokio runtime.
    pub fn start(&self) -> Result<()> {
        self.rescan_logs();
        if let Some(f) = self.opts.statusline_file.clone() {
            self.read_statusline_file(&f);
            self.watch(&f)?;
        }
        Ok(())
    }

    /// Applies a status-line JSON file if present.
    pub fn read_statusline_file(&self, f: &Path) -> bool {
        let Ok(text) = safe_fs::safe_read_to_string(f) else { return false };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return false };
        let at = std::fs::metadata(f).ok().and_then(|m| m.modified().ok()).map(DateTime::<Utc>::from).unwrap_or_else(Utc::now);
        let d = parse_statusline(&v, at);
        if d.windows.is_empty() {
            return false;
        }
        self.apply_percent(d);
        true
    }

    fn watch(&self, f: &Path) -> Result<()> {
        safe_fs::check(f)?;
        let dir = f.parent().context("status-line file has no parent")?.to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let name = f.file_name().map(|n| n.to_os_string());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && ev.paths.iter().any(|p| p.file_name().map(|n| n.to_os_string()) == name) {
                    let _ = tx.send(());
                }
        })?;
        w.watch(&dir, RecursiveMode::NonRecursive)?;
        *self.watcher.lock() = Some(w);
        let me = self.me.clone();
        let file = f.to_path_buf();
        let debounce = self.opts.debounce;
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                // Debounce: wait until no event for `debounce`.
                while let Ok(Some(())) = tokio::time::timeout(debounce, rx.recv()).await {}
                let Some(p) = me.upgrade() else { return };
                p.read_statusline_file(&file);
            }
        });
        Ok(())
    }

    /// Recomputes the active 5h block from transcripts (Source::Heuristic).
    pub fn rescan_logs(&self) {
        let Some(dir) = &self.opts.projects_dir else { return };
        let now = Utc::now();
        let block = claude_logs::active_block(dir, now);
        *self.block.lock() = block.clone();
        let Some(b) = block else { return };
        let plan = self.core.raw().plan;
        let ceiling = self.calibration.lock().ceiling(plan.as_deref(), "5h");
        let mut d = self.core.delta();
        d.windows.push(claude_logs::block_window(&b, ceiling, now));
        self.core.apply(d);
    }

    /// Applies a vendor percent observation and calibrates the heuristic ceiling from it.
    fn apply_percent(&self, d: QuotaSnapshot) {
        if let (Some(w), Some(b)) = (d.window("5h"), self.block.lock().clone())
            && let Some(f) = w.used_fraction {
                let mut cal = self.calibration.lock();
                if cal.observe(d.plan.as_deref(), "5h", b.tokens(), f, d.observed_at)
                    && let Some(p) = &self.opts.calibration_path
                        && let Err(e) = cal.save(p) {
                            tracing::warn!("calibration save failed: {e}");
                        }
            }
        self.core.apply(d);
    }

    fn add_usage(&self, u: &Usage) {
        let total = {
            let mut s = self.session.lock();
            s.add(u);
            s.clone()
        };
        let mut d = self.core.delta();
        d.windows = session_windows(&total, Source::AgentEvent, d.observed_at);
        self.core.apply(d);
    }

    fn ingest_raw(&self, v: &Value) {
        let now = Utc::now();
        match v.get("type").and_then(|t| t.as_str()) {
            Some("rate_limit_event") => {
                if let Some(d) = parse_rate_limit_event(v, now) {
                    self.apply_percent(d);
                }
            }
            Some("assistant") => {
                let Some(msg) = v.get("message") else { return };
                let id = msg.get("id").and_then(|x| x.as_str()).unwrap_or_default();
                if !id.is_empty() && !self.seen.lock().insert(id.to_string()) {
                    return; // one message is streamed as several events with the same usage
                }
                if let Some(mut u) = msg.get("usage").and_then(usage_from_json) {
                    u.cost_usd = None;
                    self.add_usage(&u);
                }
            }
            Some("result") => {
                if let Some(c) = get_f64(v, &["total_cost_usd"]) {
                    self.add_usage(&Usage { cost_usd: Some(c), ..Default::default() });
                }
            }
            _ => {
                if let Some(ws) = parse_usage_update(v, now) {
                    let mut d = self.core.delta();
                    d.windows = ws;
                    self.core.apply(d);
                }
            }
        }
    }
}

impl QuotaProvider for ClaudeProvider {
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
        Box::pin(async move {
            if let Some(f) = self.opts.statusline_file.clone() {
                self.read_statusline_file(&f);
            }
            self.rescan_logs();
        })
    }
    fn ingest_event(&self, ev: &AgentEvent) {
        match ev {
            AgentEvent::Usage(u) => self.add_usage(u),
            AgentEvent::Raw { json, .. } => {
                if let Some(v) = raw_json(json) {
                    self.ingest_raw(&v);
                }
            }
            AgentEvent::Done { .. } => {
                if let (Ok(rt), Some(me)) = (tokio::runtime::Handle::try_current(), self.me.upgrade()) {
                    rt.spawn_blocking(move || me.rescan_logs());
                }
            }
            _ => {}
        }
    }
}
