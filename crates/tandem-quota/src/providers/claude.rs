//! Claude Code.
//!
//! Signals, best first:
//! * stream-json `rate_limit_event` (`rate_limit_info{status, resetsAt, rateLimitType,
//!   utilization?, unifiedWindows{five_hour,seven_day,..{utilization,resetsAt}}}`) —
//!   claude 2.1.309 sends `unifiedWindows` utilization even when `allowed`. AgentEvent.
//! * `AgentEvent::Usage` (session tokens, cost, context fill).
//! * `~/.claude/projects/**/*.jsonl` 5h blocks (see `claude_logs`). Heuristic.

use super::claude_logs::{self, Calibration};
use super::{get_f64, raw_json, session_windows};
use crate::model::{parse_ts, QuotaSnapshot, Source, Status, Unit, Window};
use crate::persist;
use crate::provider::{ProviderCore, QuotaProvider};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tandem_core::{AgentEvent, Usage};

pub const AGENT: &str = "claude";

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

#[derive(Clone, Debug)]
pub struct ClaudeOptions {
    /// `~/.claude/projects` for the 5h-block fallback; `None` disables it.
    pub projects_dir: Option<PathBuf>,
    pub calibration_path: Option<PathBuf>,
    /// In-memory only: no transcript scans, no calibration file.
    pub offline: bool,
}
impl Default for ClaudeOptions {
    fn default() -> Self {
        Self {
            projects_dir: Some(tandem_core::fs::home().join(".claude").join("projects")),
            calibration_path: Some(persist::calibration_path()),
            offline: false,
        }
    }
}

pub struct ClaudeProvider {
    core: Arc<ProviderCore>,
    opts: ClaudeOptions,
    session: Mutex<Usage>,
    block: Mutex<Option<claude_logs::Block>>,
    calibration: Mutex<Calibration>,
    me: Weak<ClaudeProvider>,
}

impl ClaudeProvider {
    pub fn new(mut opts: ClaudeOptions) -> Arc<Self> {
        if opts.offline {
            (opts.projects_dir, opts.calibration_path) = (None, None);
        }
        let calibration = opts.calibration_path.as_deref().map(Calibration::load).unwrap_or_default();
        Arc::new_cyclic(|me| Self {
            core: Arc::new(ProviderCore::new(AGENT.into())),
            opts,
            session: Mutex::new(Usage::default()),
            block: Mutex::new(None),
            calibration: Mutex::new(calibration),
            me: me.clone(),
        })
    }

    /// Scans the transcript logs once (nothing when offline).
    pub fn start(&self) {
        self.rescan_logs();
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

    /// Only stream `rate_limit_event`s are read from raw lines; usage arrives typed.
    fn ingest_raw(&self, v: &Value) {
        if v.get("type").and_then(|t| t.as_str()) == Some("rate_limit_event")
            && let Some(d) = parse_rate_limit_event(v, Utc::now())
        {
            self.apply_percent(d);
        }
    }
}

impl QuotaProvider for ClaudeProvider {
    fn core(&self) -> &ProviderCore {
        &self.core
    }
    fn reconcile(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.rescan_logs() })
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
