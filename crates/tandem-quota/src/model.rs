//! Quota data model. Extends `tandem_core::QuotaWindow` locally with units, a finer
//! source ladder and a window kind; converts back with [`Window::to_core`].

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tandem_core::{AgentId, QuotaSource, QuotaWindow};

/// What a window's `used` / `limit` count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Percent,
    Tokens,
    Usd,
    Credits,
    Requests,
}

/// Confidence ladder. Ordering is confidence: `AgentRpc` > `AgentEvent` > `LocalLog` > `Heuristic`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Our own aggregation of local logs (e.g. ccusage-style 5h blocks).
    Heuristic,
    /// Vendor numbers read from the agent's local log files.
    LocalLog,
    /// Vendor numbers carried by the agent's own stream / status line.
    AgentEvent,
    /// Vendor numbers returned by the agent's RPC (Codex app-server).
    AgentRpc,
}
impl Source {
    pub fn to_core(self) -> QuotaSource {
        match self {
            Source::AgentRpc => QuotaSource::Official,
            Source::AgentEvent | Source::LocalLog => QuotaSource::Derived,
            Source::Heuristic => QuotaSource::Estimated,
        }
    }
    /// Router confidence multiplier.
    pub fn weight(self) -> f64 {
        match self {
            Source::AgentRpc | Source::AgentEvent => 1.0,
            Source::LocalLog => 0.8,
            Source::Heuristic => 0.6,
        }
    }
}

/// Whether a window constrains routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    /// A plan limit (5h, weekly, monthly, credits). Used by the router.
    #[default]
    Quota,
    /// Context window of the current session. Informational.
    Context,
    /// Token / cost counters without a plan limit. Informational.
    Usage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Warning,
    Rejected,
    #[default]
    Unknown,
}

/// One window, e.g. Codex "5h", Claude "weekly", ACP "context".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub label: String,
    pub kind: WindowKind,
    pub unit: Unit,
    /// Fraction used, 0.0..=1.0, when known (directly or from `used / limit`).
    pub used_fraction: Option<f64>,
    /// Amount used in `unit` (tokens, USD, credits). For `Percent`, 0..=100.
    pub used: Option<f64>,
    /// Limit in `unit`, when known.
    pub limit: Option<f64>,
    pub window_mins: Option<i64>,
    pub reset_at: Option<DateTime<Utc>>,
    pub source: Source,
    pub observed_at: DateTime<Utc>,
}

impl Window {
    pub fn new(label: impl Into<String>, unit: Unit, source: Source, observed_at: DateTime<Utc>) -> Self {
        Self {
            label: label.into(),
            kind: WindowKind::Quota,
            unit,
            used_fraction: None,
            used: None,
            limit: None,
            window_mins: None,
            reset_at: None,
            source,
            observed_at,
        }
    }
    /// A percent window from a 0..=100 value.
    pub fn percent(label: impl Into<String>, used_pct: f64, source: Source, observed_at: DateTime<Utc>) -> Self {
        let mut w = Self::new(label, Unit::Percent, source, observed_at);
        w.used = Some(used_pct);
        w.used_fraction = Some((used_pct / 100.0).clamp(0.0, 1.0));
        w
    }
    pub fn with_kind(mut self, kind: WindowKind) -> Self {
        self.kind = kind;
        self
    }
    pub fn with_reset(mut self, reset_at: Option<DateTime<Utc>>) -> Self {
        self.reset_at = reset_at;
        self
    }
    /// Sets `used`/`limit` and derives `used_fraction` when both are known.
    pub fn with_amounts(mut self, used: Option<f64>, limit: Option<f64>) -> Self {
        self.used = used;
        self.limit = limit;
        if let (Some(u), Some(l)) = (used, limit)
            && l > 0.0 {
                self.used_fraction = Some((u / l).clamp(0.0, 1.0));
            }
        self
    }
    pub fn headroom(&self) -> Option<f64> {
        self.used_fraction.map(|u| (1.0 - u).clamp(0.0, 1.0))
    }
    pub fn age(&self, now: DateTime<Utc>) -> Duration {
        now - self.observed_at
    }
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.reset_at.is_some_and(|r| r <= now)
    }
    pub fn to_core(&self, agent: &AgentId) -> QuotaWindow {
        let tokens = matches!(self.unit, Unit::Tokens);
        QuotaWindow {
            agent: agent.clone(),
            label: self.label.clone(),
            used_fraction: self.used_fraction,
            used_tokens: self.used.filter(|_| tokens).map(|v| v as u64),
            limit_tokens: self.limit.filter(|_| tokens).map(|v| v as u64),
            reset_at: self.reset_at,
            source: self.source.to_core(),
            observed_at: self.observed_at,
        }
    }
}

/// Everything known about one agent's quota at a point in time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub agent: AgentId,
    pub plan: Option<String>,
    pub windows: Vec<Window>,
    pub status: Status,
    /// When a `Rejected`/`Warning` status stops applying (vendor reset time).
    #[serde(default)]
    pub status_until: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
    /// Last provider error (e.g. "codex app-server: not logged in"), for the UI tooltip.
    #[serde(default)]
    pub error: Option<String>,
}

/// A higher-confidence window older than this loses to a fresher lower-confidence one.
pub const STALE_AFTER: Duration = Duration::minutes(30);

impl QuotaSnapshot {
    pub fn empty(agent: AgentId, now: DateTime<Utc>) -> Self {
        Self { agent, plan: None, windows: vec![], status: Status::Unknown, status_until: None, observed_at: now, error: None }
    }
    pub fn window(&self, label: &str) -> Option<&Window> {
        self.windows.iter().find(|w| w.label == label)
    }
    /// Quota windows with a known fraction.
    pub fn quota_windows(&self) -> impl Iterator<Item = &Window> {
        self.windows.iter().filter(|w| w.kind == WindowKind::Quota && w.used_fraction.is_some())
    }
    /// Drops windows whose `reset_at` has passed and lifts an elapsed `status_until`.
    pub fn expire(&mut self, now: DateTime<Utc>) {
        self.windows.retain(|w| !w.is_expired(now));
        if self.status_until.is_some_and(|t| t <= now) {
            self.status_until = None;
            self.status = derive_status(&self.windows);
        }
    }
}

/// Status implied by quota windows: >=100% → Rejected, >=80% → Warning.
pub fn derive_status(windows: &[Window]) -> Status {
    let mut max: Option<f64> = None;
    for w in windows.iter().filter(|w| w.kind == WindowKind::Quota) {
        if let Some(f) = w.used_fraction {
            max = Some(max.map_or(f, |m: f64| m.max(f)));
        }
    }
    match max {
        None => Status::Unknown,
        Some(f) if f >= 1.0 => Status::Rejected,
        Some(f) if f >= 0.8 => Status::Warning,
        Some(_) => Status::Ok,
    }
}

/// True when `new` should replace `old` for the same label.
fn prefer_new(old: &Window, new: &Window) -> bool {
    if new.observed_at - old.observed_at > STALE_AFTER {
        return true; // old is stale relative to new, whatever its source
    }
    if old.observed_at - new.observed_at > STALE_AFTER {
        return false;
    }
    (new.source, new.observed_at) >= (old.source, old.observed_at)
}

fn merge_window(old: &Window, new: &Window) -> Window {
    let (mut win, lose) = if prefer_new(old, new) { (new.clone(), old) } else { (old.clone(), new) };
    // Sparse updates: fill gaps from the other observation of the same window.
    let same_period = match (win.reset_at, lose.reset_at) {
        (Some(a), Some(b)) => (a - b).num_seconds().abs() < 120,
        _ => true,
    };
    if same_period {
        if win.used_fraction.is_none() && win.used.is_none() {
            win.used_fraction = lose.used_fraction;
            win.used = lose.used;
        }
        win.limit = win.limit.or(lose.limit);
        win.reset_at = win.reset_at.or(lose.reset_at);
        win.window_mins = win.window_mins.or(lose.window_mins);
    }
    win
}

/// Merges two snapshots of the same agent, keeping per label the higher-confidence,
/// fresher window (see [`STALE_AFTER`]). Plan/status/error come from `new` when set.
pub fn merge(old: &QuotaSnapshot, new: &QuotaSnapshot) -> QuotaSnapshot {
    let mut windows = old.windows.clone();
    for nw in &new.windows {
        match windows.iter_mut().find(|w| w.label == nw.label) {
            Some(ow) => *ow = merge_window(ow, nw),
            None => windows.push(nw.clone()),
        }
    }
    let (status, status_until) = if new.status_until.is_some() {
        (new.status, new.status_until)
    } else if old.status_until.is_some() && new.status != Status::Ok {
        (old.status, old.status_until)
    } else {
        (Status::Unknown, None)
    };
    let mut out = QuotaSnapshot {
        agent: new.agent.clone(),
        plan: new.plan.clone().or_else(|| old.plan.clone()),
        windows,
        status,
        status_until,
        observed_at: old.observed_at.max(new.observed_at),
        error: if new.windows.is_empty() { new.error.clone().or_else(|| old.error.clone()) } else { new.error.clone() },
    };
    if out.status_until.is_none() {
        let derived = derive_status(&out.windows);
        out.status = if derived == Status::Unknown && new.status != Status::Unknown { new.status } else { derived };
    }
    out
}

/// Unix seconds (or milliseconds, or ISO-8601 string) → UTC.
pub fn parse_ts(v: &serde_json::Value) -> Option<DateTime<Utc>> {
    if let Some(n) = v.as_f64() {
        let secs = if n > 1e12 { n / 1000.0 } else { n };
        return Utc.timestamp_opt(secs as i64, 0).single();
    }
    v.as_str().and_then(|s| {
        DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc)).or_else(|| s.parse::<f64>().ok().and_then(|n| parse_ts(&serde_json::json!(n))))
    })
}

/// "5h", "weekly", "monthly", ... from a window length in minutes.
pub fn duration_label(mins: i64) -> String {
    match mins {
        300 => "5h".into(),
        1440 => "daily".into(),
        10080 => "weekly".into(),
        43200 | 43800 | 44640 => "monthly".into(),
        m if m % 1440 == 0 => format!("{}d", m / 1440),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(min: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 24, 12, 0, 0).unwrap() + Duration::minutes(min)
    }
    fn snap(ws: Vec<Window>, at: DateTime<Utc>) -> QuotaSnapshot {
        let mut s = QuotaSnapshot::empty("claude".into(), at);
        s.windows = ws;
        s
    }

    #[test]
    fn merge_prefers_higher_source_when_fresh() {
        let old = snap(vec![Window::percent("5h", 40.0, Source::AgentRpc, t(0))], t(0));
        let new = snap(vec![Window::percent("5h", 10.0, Source::Heuristic, t(5))], t(5));
        let m = merge(&old, &new);
        assert_eq!(m.window("5h").unwrap().source, Source::AgentRpc);
        assert_eq!(m.window("5h").unwrap().used, Some(40.0));
    }

    #[test]
    fn merge_prefers_fresher_same_source_and_stale_loses() {
        let old = snap(vec![Window::percent("5h", 40.0, Source::AgentEvent, t(0))], t(0));
        let new = snap(vec![Window::percent("5h", 45.0, Source::AgentEvent, t(1))], t(1));
        assert_eq!(merge(&old, &new).window("5h").unwrap().used, Some(45.0));
        // Higher source but 31 minutes older than the new heuristic: stale, replaced.
        let old = snap(vec![Window::percent("5h", 40.0, Source::AgentRpc, t(0))], t(0));
        let new = snap(vec![Window::percent("5h", 60.0, Source::Heuristic, t(31))], t(31));
        assert_eq!(merge(&old, &new).window("5h").unwrap().source, Source::Heuristic);
        // Older observation arriving late never wins over a fresher same-source one.
        let m = merge(&new, &old);
        assert_eq!(m.window("5h").unwrap().source, Source::Heuristic);
    }

    #[test]
    fn merge_keeps_unrelated_labels_and_fills_sparse() {
        let mut a = Window::percent("5h", 20.0, Source::AgentEvent, t(0));
        a.reset_at = Some(t(100));
        let old = snap(vec![a, Window::percent("weekly", 50.0, Source::AgentEvent, t(0))], t(0));
        let sparse = Window::new("5h", Unit::Percent, Source::AgentEvent, t(2)).with_reset(Some(t(100)));
        let m = merge(&old, &snap(vec![sparse], t(2)));
        assert_eq!(m.windows.len(), 2);
        assert_eq!(m.window("5h").unwrap().used, Some(20.0));
        assert_eq!(m.window("5h").unwrap().observed_at, t(2));
        assert_eq!(m.status, Status::Ok);
    }

    #[test]
    fn expire_drops_past_windows_and_status() {
        let mut s = snap(
            vec![
                Window::percent("5h", 100.0, Source::AgentEvent, t(0)).with_reset(Some(t(60))),
                Window::percent("weekly", 50.0, Source::AgentEvent, t(0)).with_reset(Some(t(6000))),
                Window::new("session", Unit::Tokens, Source::AgentEvent, t(0)).with_kind(WindowKind::Usage),
            ],
            t(0),
        );
        s.status = Status::Rejected;
        s.status_until = Some(t(60));
        s.expire(t(59));
        assert_eq!(s.windows.len(), 3);
        assert_eq!(s.status, Status::Rejected);
        s.expire(t(60));
        assert_eq!(s.windows.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), ["weekly", "session"]);
        assert_eq!(s.status, Status::Ok);
    }

    #[test]
    fn labels_and_timestamps() {
        assert_eq!(duration_label(300), "5h");
        assert_eq!(duration_label(10080), "weekly");
        assert_eq!(duration_label(43200), "monthly");
        assert_eq!(duration_label(120), "2h");
        let a = parse_ts(&serde_json::json!(1792801200)).unwrap();
        assert_eq!(parse_ts(&serde_json::json!(1792801200000_i64)), Some(a));
        assert_eq!(parse_ts(&serde_json::json!(a.to_rfc3339())), Some(a));
    }

    #[test]
    fn to_core_maps_tokens() {
        let w = Window::new("context", Unit::Tokens, Source::LocalLog, t(0)).with_amounts(Some(50.0), Some(200.0));
        let c = w.to_core(&"codex".into());
        assert_eq!(c.used_tokens, Some(50));
        assert_eq!(c.used_fraction, Some(0.25));
        assert_eq!(c.source, QuotaSource::Derived);
    }
}
