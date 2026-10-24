//! Router: rank agents for a task by quota headroom, confidence and phase preference.

use crate::model::{derive_status, Status};
use crate::pool::PoolState;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::Duration;
use tandem_core::{AgentId, TaskPhase, Usage};

/// `TaskPhase` with `Ord`, so it can key a `BTreeMap` (core's enum lacks `Ord`/`Hash`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PhaseKey(pub TaskPhase);
impl PhaseKey {
    fn rank(self) -> u8 {
        match self.0 {
            TaskPhase::Plan => 0,
            TaskPhase::Implement => 1,
            TaskPhase::Test => 2,
            TaskPhase::Review => 3,
            TaskPhase::Any => 4,
        }
    }
}
impl Ord for PhaseKey {
    fn cmp(&self, o: &Self) -> Ordering {
        self.rank().cmp(&o.rank())
    }
}
impl PartialOrd for PhaseKey {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl From<TaskPhase> for PhaseKey {
    fn from(p: TaskPhase) -> Self {
        Self(p)
    }
}

#[derive(Clone, Debug)]
pub struct RouteRequest<'a> {
    pub phase: TaskPhase,
    /// User pin: always ranked first unless rejected/excluded.
    pub pins: Option<AgentId>,
    pub exclude: Vec<AgentId>,
    /// Ordered preferred agents per phase (`Any` applies to every phase).
    pub prefs: BTreeMap<PhaseKey, Vec<AgentId>>,
    /// Installed agents. Non-empty: only these are candidates (missing from pool → unknown).
    pub require_installed: &'a [AgentId],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteChoice {
    pub agent: AgentId,
    pub score: f64,
    /// Tightest known headroom (0..=1), `None` when quota is unknown.
    pub headroom: Option<f64>,
    pub reason: String,
}

pub const UNKNOWN_HEADROOM: f64 = 0.5;
const PIN_BONUS: f64 = 10.0;

fn pref_bonus(req: &RouteRequest<'_>, agent: &AgentId) -> f64 {
    let pos = |k: TaskPhase| req.prefs.get(&PhaseKey(k)).and_then(|v| v.iter().position(|a| a == agent));
    match pos(req.phase).or_else(|| pos(TaskPhase::Any)) {
        Some(i) => (0.2 - 0.05 * i as f64).max(0.05),
        None => 0.0,
    }
}

/// Ranks candidate agents, best first. Rejected and excluded agents are omitted.
/// Ties break by agent name for determinism.
pub fn route(pool: &PoolState, req: &RouteRequest<'_>) -> Vec<RouteChoice> {
    let candidates: Vec<AgentId> = if req.require_installed.is_empty() {
        pool.snapshots.keys().cloned().collect()
    } else {
        let mut v = req.require_installed.to_vec();
        v.sort();
        v.dedup();
        v
    };
    let mut out = Vec::new();
    for agent in candidates {
        if req.exclude.contains(&agent) {
            continue;
        }
        let snap = pool.snapshots.get(&agent);
        if let Some(s) = snap
            && (s.status == Status::Rejected || derive_status(&s.windows) == Status::Rejected) {
                continue;
            }
        let tightest = snap.and_then(|s| {
            s.quota_windows().min_by(|a, b| {
                a.headroom().partial_cmp(&b.headroom()).unwrap_or(Ordering::Equal).then_with(|| a.label.cmp(&b.label))
            })
        });
        let (mut score, headroom, mut reason) = match tightest {
            Some(w) => {
                let h = w.headroom().unwrap_or(UNKNOWN_HEADROOM);
                let wt = w.source.weight();
                let src = serde_json::to_value(w.source).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
                (h * wt, Some(h), format!("{} {:.0}% left ({src}, x{wt})", w.label, h * 100.0))
            }
            None => (UNKNOWN_HEADROOM, None, "unknown quota".to_string()),
        };
        if snap.is_some_and(|s| s.status == Status::Warning) {
            reason.push_str("; near limit");
        }
        let bonus = pref_bonus(req, &agent);
        if bonus > 0.0 {
            score += bonus;
            reason.push_str(&format!("; +{bonus:.2} phase preference"));
        }
        if req.pins.as_ref() == Some(&agent) {
            score += PIN_BONUS;
            reason = format!("pinned by user; {reason}");
        }
        out.push(RouteChoice { agent, score, headroom, reason });
    }
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal).then_with(|| a.agent.cmp(&b.agent)));
    out
}

/// One finished turn, for the pre-prompt estimate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub agent: AgentId,
    pub usage: Usage,
    pub elapsed: Duration,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub median_tokens: u64,
    pub p90_tokens: u64,
    pub median_secs: f64,
    /// Turns the estimate is based on (0 → no history).
    pub samples: usize,
}

/// Window of recent turns used by [`estimate`].
pub const ESTIMATE_WINDOW: usize = 10;

fn total_tokens(u: &Usage) -> u64 {
    u.total()
}

fn median_u64(sorted: &[u64]) -> u64 {
    let n = sorted.len();
    if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2 }
}

/// Median / p90 tokens and median seconds over the agent's last [`ESTIMATE_WINDOW`] turns
/// (history is chronological). Tokens = input + output + cache read + cache write.
/// p90 uses nearest rank.
pub fn estimate(history: &[TurnRecord], agent: &AgentId) -> Estimate {
    let recent: Vec<&TurnRecord> = history.iter().rev().filter(|t| &t.agent == agent).take(ESTIMATE_WINDOW).collect();
    if recent.is_empty() {
        return Estimate::default();
    }
    let mut toks: Vec<u64> = recent.iter().map(|t| total_tokens(&t.usage)).collect();
    toks.sort_unstable();
    let mut secs: Vec<f64> = recent.iter().map(|t| t.elapsed.as_secs_f64()).collect();
    secs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let n = toks.len();
    let p90_idx = ((0.9 * n as f64).ceil() as usize).clamp(1, n) - 1;
    let median_secs = if n % 2 == 1 { secs[n / 2] } else { (secs[n / 2 - 1] + secs[n / 2]) / 2.0 };
    Estimate { median_tokens: median_u64(&toks), p90_tokens: toks[p90_idx], median_secs, samples: n }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{QuotaSnapshot, Source, Window};
    use chrono::Utc;

    fn pool() -> PoolState {
        let now = Utc::now();
        let mut st = PoolState::default();
        let mut rejected = QuotaSnapshot::empty("claude".into(), now);
        rejected.status = Status::Rejected;
        rejected.windows.push(Window::percent("5h", 100.0, Source::AgentEvent, now));
        let unknown = QuotaSnapshot::empty("gemini".into(), now);
        let mut hot = QuotaSnapshot::empty("codex".into(), now);
        hot.windows.push(Window::percent("5h", 90.0, Source::AgentRpc, now));
        hot.windows.push(Window::percent("weekly", 30.0, Source::AgentRpc, now));
        hot.status = Status::Warning;
        let mut cool = QuotaSnapshot::empty("opencode".into(), now);
        cool.windows.push(Window::percent("monthly", 10.0, Source::LocalLog, now));
        cool.status = Status::Ok;
        for s in [rejected, unknown, hot, cool] {
            st.snapshots.insert(s.agent.clone(), s);
        }
        st
    }
    fn req<'a>(phase: TaskPhase) -> RouteRequest<'a> {
        RouteRequest { phase, pins: None, exclude: vec![], prefs: BTreeMap::new(), require_installed: &[] }
    }
    fn ids(v: &[RouteChoice]) -> Vec<&str> {
        v.iter().map(|c| c.agent.as_str()).collect()
    }

    #[test]
    fn scoring_matrix() {
        let r = route(&pool(), &req(TaskPhase::Implement));
        assert_eq!(ids(&r), ["opencode", "gemini", "codex"]);
        assert!((r[0].score - 0.9 * 0.8).abs() < 1e-9);
        assert_eq!(r[1].score, 0.5);
        assert_eq!(r[1].reason, "unknown quota");
        assert!((r[2].score - 0.1).abs() < 1e-9);
        assert!(r[2].reason.starts_with("5h 10% left"));
        assert!(r[2].reason.contains("near limit"));
    }

    #[test]
    fn prefs_pins_exclude_installed() {
        let mut q = req(TaskPhase::Plan);
        q.prefs.insert(TaskPhase::Plan.into(), vec!["codex".into()]);
        q.prefs.insert(TaskPhase::Any.into(), vec!["gemini".into()]);
        let r = route(&pool(), &q);
        assert_eq!(ids(&r), ["opencode", "gemini", "codex"]);
        assert!((r[1].score - 0.7).abs() < 1e-9);
        assert!((r[2].score - 0.3).abs() < 1e-9);

        q.pins = Some("codex".into());
        q.exclude = vec!["opencode".into()];
        assert_eq!(ids(&route(&pool(), &q)), ["codex", "gemini"]);

        q.pins = Some("claude".into()); // rejected stays out even when pinned
        let installed: Vec<AgentId> = vec!["claude".into(), "goose".into(), "gemini".into()];
        q.require_installed = &installed;
        q.exclude.clear();
        q.prefs.clear();
        let r = route(&pool(), &q);
        assert_eq!(ids(&r), ["gemini", "goose"]); // tie at 0.5 broken by name
    }

    #[test]
    fn estimate_math() {
        let rec = |a: &str, tok: u64, s: u64| TurnRecord {
            agent: a.into(),
            usage: Usage { input_tokens: tok, ..Default::default() },
            elapsed: Duration::from_secs(s),
        };
        assert_eq!(estimate(&[], &"x".into()).samples, 0);
        // 12 codex turns: only the last 10 (tokens 300..=1200) count.
        let mut h: Vec<TurnRecord> = (1..=12).map(|i| rec("codex", i * 100, i)).collect();
        h.push(rec("claude", 99_999, 999));
        let e = estimate(&h, &"codex".into());
        assert_eq!(e.samples, 10);
        assert_eq!(e.median_tokens, 750);
        assert_eq!(e.p90_tokens, 1100);
        assert_eq!(e.median_secs, 7.5);
        let e = estimate(&h, &"claude".into());
        assert_eq!((e.median_tokens, e.p90_tokens, e.samples), (99_999, 99_999, 1));
    }
}
