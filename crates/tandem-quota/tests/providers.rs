use chrono::{Duration, TimeZone, Utc};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tandem_core::{AgentEvent, Direction, StopReason, Usage};
use tandem_quota::providers::{self, acp, claude, claude_logs, codex, codex_logs, gemini, goose};
use tandem_quota::*;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}
fn raw(v: &str) -> AgentEvent {
    AgentEvent::Raw { direction: Direction::FromAgent, json: v.to_string() }
}
async fn wait_for(rx: &tandem_core::Receiver<QuotaSnapshot>, pred: impl Fn(&QuotaSnapshot) -> bool) -> QuotaSnapshot {
    tokio::time::timeout(StdDuration::from_secs(10), async {
        loop {
            let s = rx.recv().await.expect("provider closed");
            if pred(&s) {
                return s;
            }
        }
    })
    .await
    .expect("timed out waiting for snapshot")
}

// ------------------------------------------------------------------ codex

#[test]
fn codex_session_log_fixture() {
    let text = std::fs::read_to_string(fixture("codex_session.jsonl")).unwrap();
    let d = codex_logs::latest_in_text(&text).unwrap();
    assert_eq!(d.plan.as_deref(), Some("plus"));
    let five = d.window("5h").unwrap();
    assert_eq!((five.used, five.source, five.unit), (Some(42.0), Source::LocalLog, Unit::Percent));
    assert_eq!(five.reset_at, Utc.timestamp_opt(1_900_000_000, 0).single());
    assert_eq!(five.observed_at, Utc.with_ymd_and_hms(2026, 10, 24, 8, 3, 0).unwrap());
    assert_eq!(d.window("weekly").unwrap().used_fraction, Some(0.12));
    let ctx = d.window("context").unwrap();
    assert_eq!((ctx.kind, ctx.used, ctx.limit), (WindowKind::Context, Some(2200.0), Some(258400.0)));
    assert_eq!(d.window("session").unwrap().used, Some(3300.0));
    assert_eq!(d.status, Status::Ok);
}

#[test]
fn codex_logs_from_dir_picks_newest() {
    let dir = tempfile::tempdir().unwrap();
    let day = dir.path().join("2026/10/24");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::copy(fixture("codex_session.jsonl"), day.join("rollout-a.jsonl")).unwrap();
    std::thread::sleep(StdDuration::from_millis(20));
    // Newer file without rate limits: falls back to the older one.
    std::fs::write(day.join("rollout-b.jsonl"), "{\"type\":\"session_meta\"}\n").unwrap();
    let d = codex_logs::latest_from_dir(dir.path()).unwrap();
    assert_eq!(d.window("5h").unwrap().used, Some(42.0));
}

#[test]
fn codex_read_response_parsing() {
    let now = Utc::now();
    let res = json!({"rateLimits": {"limitId":"codex","planType":"go","primary":{"usedPercent":0,"windowDurationMins":43200,"resetsAt":1900000000},"secondary":null,
        "credits":{"hasCredits":true,"unlimited":false,"balance":"12.5"},"rateLimitReachedType":null,"spendControlReached":false},
        "rateLimitsByLimitId": {"codex": {}, "codex_mini": {"primary":{"usedPercent":100,"windowDurationMins":300,"resetsAt":1900000000}}}});
    let d = codex::parse_read_response(&res, Source::AgentRpc, now);
    assert_eq!(d.plan.as_deref(), Some("go"));
    assert_eq!(d.window("monthly").unwrap().used_fraction, Some(0.0));
    assert_eq!(d.window("credits").unwrap().limit, Some(12.5));
    assert_eq!(d.window("codex_mini 5h").unwrap().used_fraction, Some(1.0));
    let reached = json!({"primary":{"usedPercent":100,"windowDurationMins":300,"resetsAt":1900000000},"rateLimitReachedType":"rate_limit_reached"});
    let d = codex::parse_rate_limits(&reached, Source::AgentRpc, now, true);
    assert_eq!(d.status, Status::Rejected);
    assert_eq!(d.status_until, Utc.timestamp_opt(1_900_000_000, 0).single());
    // Sparse update without reached info keeps status Unknown (= unchanged).
    let d = codex::parse_rate_limits(&json!({"primary":{"usedPercent":3}}), Source::AgentRpc, now, true);
    assert_eq!((d.status, d.window("primary").unwrap().used), (Status::Unknown, Some(3.0)));
}

#[tokio::test]
async fn codex_fake_app_server() {
    if std::process::Command::new("node").arg("--version").output().is_err() {
        eprintln!("node not installed; skipping");
        return;
    }
    let opts = codex::CodexOptions {
        program: "node".into(),
        args: vec![fixture("fake_app_server.js").to_string_lossy().into_owned()],
        sessions_dir: None,
        request_timeout: StdDuration::from_secs(5),
        offline: false,
        agent: "codex".into(),
    };
    let p = codex::CodexProvider::new(opts);
    let rx = p.subscribe();
    p.start().await.expect("start against fake app-server");
    let s = p.snapshot();
    assert_eq!(s.plan.as_deref(), Some("pro"));
    let five = s.window("5h").unwrap();
    assert_eq!((five.used, five.source), (Some(25.0), Source::AgentRpc));
    assert_eq!(s.window("weekly").unwrap().used, Some(5.0));
    // Pushed sparse update + token usage.
    let s = wait_for(&rx, |s| s.window("5h").and_then(|w| w.used) == Some(30.0)).await;
    assert_eq!(s.plan.as_deref(), Some("pro"), "sparse null plan must not clear it");
    assert_eq!(s.window("weekly").unwrap().used, Some(5.0));
    let s = wait_for(&rx, |s| s.window("context").is_some()).await;
    assert_eq!(s.window("context").unwrap().used_fraction, Some(0.15));
    assert_eq!(s.window("session").unwrap().used, Some(390.0));
    // Reconcile re-reads.
    p.reconcile().await;
    assert_eq!(p.snapshot().window("5h").unwrap().used, Some(40.0));
    assert_eq!(p.snapshot().error, None);
}

#[tokio::test]
async fn codex_missing_binary_records_error() {
    let opts = codex::CodexOptions { program: "/nonexistent/codex".into(), sessions_dir: Some(fixture("")), ..Default::default() };
    let p = codex::CodexProvider::new(opts);
    assert!(p.start().await.is_err());
    let s = p.snapshot();
    assert!(s.error.as_deref().unwrap().contains("codex app-server"));
}

/// Live check against the installed codex (`cargo test -p tandem-quota -- --ignored codex_live`).
#[tokio::test]
#[ignore]
async fn codex_live() {
    let p = codex::CodexProvider::new(codex::CodexOptions::default());
    let r = p.start().await;
    let s = p.snapshot();
    println!("{r:?}\n{}", serde_json::to_string_pretty(&s).unwrap());
    r.unwrap();
    assert!(s.windows.iter().any(|w| w.source == Source::AgentRpc));
}

// ------------------------------------------------------------------ claude

#[test]
fn claude_rate_limit_event_statuses() {
    let now = Utc::now();
    let allowed = json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1900000000,"rateLimitType":"five_hour",
        "unifiedWindows":{"five_hour":{"utilization":0.22,"resetsAt":1900000000},"seven_day":{"utilization":0.42,"resetsAt":1900500000},"seven_day_opus":{"utilization":0.1}}}});
    let d = claude::parse_rate_limit_event(&allowed, now).unwrap();
    assert_eq!(d.status, Status::Ok);
    assert_eq!(d.status_until, None);
    assert_eq!(d.window("5h").unwrap().used, Some(22.0));
    assert_eq!(d.window("weekly").unwrap().used_fraction, Some(0.42));
    assert!(d.window("weekly-opus").is_some());

    let warn = json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1900000000,"rateLimitType":"seven_day","utilization":0.91}});
    let d = claude::parse_rate_limit_event(&warn, now).unwrap();
    assert_eq!(d.status, Status::Warning);
    assert_eq!(d.status_until, Utc.timestamp_opt(1_900_000_000, 0).single());
    assert_eq!(d.window("weekly").unwrap().used, Some(91.0));

    let rejected = json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1900000000,"rateLimitType":"five_hour"}});
    let d = claude::parse_rate_limit_event(&rejected, now).unwrap();
    assert_eq!(d.status, Status::Rejected);
    assert_eq!(d.window("5h").unwrap().used_fraction, Some(1.0));

    // Missing fields: no type, no reset, no utilization.
    let bare = json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}});
    let d = claude::parse_rate_limit_event(&bare, now).unwrap();
    assert_eq!(d.status, Status::Rejected);
    assert!(d.windows.is_empty());
    assert!(d.status_until.unwrap() > now);
    let allowed_bare = json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour"}});
    let d = claude::parse_rate_limit_event(&allowed_bare, now).unwrap();
    assert_eq!(d.window("5h").unwrap().used_fraction, None);
    assert!(claude::parse_rate_limit_event(&json!({"type":"rate_limit_event"}), now).is_none());
}

fn claude_opts(dir: &Path) -> claude::ClaudeOptions {
    claude::ClaudeOptions { projects_dir: Some(dir.join("projects")), calibration_path: Some(dir.join("calibration.json")), offline: false, ..Default::default() }
}

#[tokio::test]
async fn claude_stream_ingest() {
    let dir = tempfile::tempdir().unwrap();
    let p = claude::ClaudeProvider::new(claude_opts(dir.path()));
    // Raw lines: only `rate_limit_event` is read; stream usage arrives as typed events.
    for line in std::fs::read_to_string(fixture("claude_stream.jsonl")).unwrap().lines() {
        p.ingest_event(&raw(line));
    }
    p.ingest_event(&AgentEvent::Usage(Usage { input_tokens: 10, cost_usd: Some(0.25), context_used: Some(500), context_size: Some(1000), ..Default::default() }));
    let s = p.snapshot();
    assert_eq!(s.window("session").unwrap().used, Some(10.0));
    assert_eq!(s.window("session cost").unwrap().used, Some(0.25));
    assert_eq!(s.window("context").unwrap().used_fraction, Some(0.5));
    assert_eq!(s.window("5h").unwrap().used, Some(22.0));
    assert_eq!(s.window("weekly-overage-included").unwrap().used, Some(15.0));
    assert_eq!(s.status, Status::Ok);
    let rejected = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1900000000,"rateLimitType":"five_hour"}}"#;
    p.ingest_event(&raw(rejected));
    assert_eq!(p.snapshot().status, Status::Rejected);
}

#[test]
fn claude_transcript_blocks() {
    let text = std::fs::read_to_string(fixture("claude_transcript.jsonl")).unwrap();
    let mut seen = std::collections::HashSet::new();
    let entries: Vec<_> = text
        .lines()
        .filter_map(|l| claude_logs::parse_line(&serde_json::from_str(l).unwrap()))
        .filter(|(k, _)| seen.insert(k.clone()))
        .map(|(_, e)| e)
        .collect();
    assert_eq!(entries.len(), 4);
    let b = claude_logs::blocks(&entries);
    let at = |h, m| Utc.with_ymd_and_hms(2026, 10, 24, h, m, 0).unwrap();
    assert_eq!(b.iter().map(|b| b.start).collect::<Vec<_>>(), [at(1, 0), at(6, 0), at(13, 0)]);
    assert_eq!(b.iter().map(|b| b.tokens()).collect::<Vec<_>>(), [3260, 10, 200]);
    assert_eq!(b[0].entries, 2);
    assert!(b[2].is_active(at(17, 59)));
    assert!(!b[2].is_active(at(18, 0)));
}

fn write_live_transcript(dir: &Path, tokens_out: u64) {
    let proj = dir.join("projects/-tmp-x");
    std::fs::create_dir_all(&proj).unwrap();
    let ts = (Utc::now() - Duration::minutes(10)).to_rfc3339();
    let line = json!({"type":"assistant","requestId":"r1","timestamp":ts,"message":{"id":"m1","usage":{"input_tokens":0,"output_tokens":tokens_out}}});
    std::fs::write(proj.join("s.jsonl"), format!("{line}\n")).unwrap();
}

#[tokio::test]
async fn claude_heuristic_calibration() {
    let dir = tempfile::tempdir().unwrap();
    write_live_transcript(dir.path(), 1000);
    let p = claude::ClaudeProvider::new(claude_opts(dir.path()));
    p.rescan_logs();
    let s = p.snapshot();
    let w = s.window("5h tokens").unwrap();
    assert_eq!((w.source, w.kind, w.used, w.used_fraction), (Source::Heuristic, WindowKind::Usage, Some(1000.0), None));
    assert!(s.window("5h").is_none());
    // A 25% observation while 1000 tokens are in the block → ceiling 4000.
    let ev = json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour","utilization":0.25,"resetsAt":1900000000}});
    p.ingest_event(&raw(&ev.to_string()));
    let cal = claude_logs::Calibration::load(&dir.path().join("calibration.json"));
    assert_eq!(cal.ceiling(None, "5h"), Some(4000.0));
    // Fresh provider (no vendor signal): calibrated heuristic percent.
    write_live_transcript(dir.path(), 2000);
    let p2 = claude::ClaudeProvider::new(claude_opts(dir.path()));
    p2.rescan_logs();
    let w = p2.snapshot().window("5h").cloned().unwrap();
    assert_eq!((w.source, w.kind, w.used_fraction), (Source::Heuristic, WindowKind::Quota, Some(0.5)));
}

// ------------------------------------------------------------------ others

#[tokio::test]
async fn acp_usage_update() {
    let p = acp::AcpGenericProvider::new("opencode".into());
    // tandem-acp maps `usage_update` to typed context fields and cost deltas.
    p.ingest_event(&AgentEvent::Usage(Usage { context_used: Some(50000), context_size: Some(200000), cost_usd: Some(0.40), ..Default::default() }));
    p.ingest_event(&AgentEvent::Usage(Usage { cost_usd: Some(0.02), ..Default::default() }));
    p.ingest_event(&raw(r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk"}}}"#));
    p.ingest_event(&AgentEvent::Usage(Usage { input_tokens: 7, output_tokens: 3, ..Default::default() }));
    let s = p.snapshot();
    let ctx = s.window("context").unwrap();
    assert_eq!((ctx.unit, ctx.limit, ctx.used_fraction, ctx.source), (Unit::Tokens, Some(200000.0), Some(0.25), Source::AgentEvent));
    assert!((s.window("session cost").unwrap().used.unwrap() - 0.42).abs() < 1e-9);
    assert_eq!(s.window("session").unwrap().used, Some(10.0));
    assert_eq!(s.status, Status::Unknown);
}

#[tokio::test]
async fn gemini_session_usage() {
    let p = gemini::GeminiProvider::new("gemini".into());
    p.ingest_event(&AgentEvent::Usage(Usage { input_tokens: 100, output_tokens: 20, cache_read_tokens: 5, ..Default::default() }));
    p.ingest_event(&AgentEvent::Usage(Usage { input_tokens: 10, ..Default::default() }));
    p.ingest_event(&raw(r#"{"type":"result","stats":{"total_tokens":10}}"#));
    let s = p.snapshot();
    assert_eq!(s.window("session").unwrap().used, Some(135.0));
    assert_eq!(s.status, Status::Unknown);
}

#[test]
fn for_agent_follows_quota_kind() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _g = rt.enter();
    let desc = |id: &str, kind| tandem_core::AgentDescriptor { id: id.into(), quota_kind: kind, ..Default::default() };
    use providers::Mode::Offline;
    use tandem_core::QuotaKind;
    assert_eq!(providers::for_agent(&desc("claude", QuotaKind::Claude), Offline).unwrap().agent().as_str(), "claude");
    assert_eq!(providers::for_agent(&desc("codex", QuotaKind::Codex), Offline).unwrap().agent().as_str(), "codex");
    assert_eq!(providers::for_agent(&desc("x", QuotaKind::AcpGeneric), Offline).unwrap().agent().as_str(), "x");
    assert!(providers::for_agent(&desc("y", QuotaKind::None), Offline).is_none());
    // Registered under the descriptor's id, not a fixed one.
    for (id, kind) in [("claude-work", QuotaKind::Claude), ("codex-2", QuotaKind::Codex), ("goose-local", QuotaKind::Goose)] {
        let p = providers::for_agent(&desc(id, kind), Offline).unwrap();
        assert_eq!(p.agent().as_str(), id);
        assert_eq!(p.snapshot().agent.as_str(), id);
    }
}

#[tokio::test]
async fn goose_sessions_db() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sessions.db");
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, working_dir TEXT NOT NULL, updated_at TIMESTAMP, total_tokens INTEGER, accumulated_total_tokens INTEGER);",
        )
        .unwrap();
        let fmt = |d: chrono::DateTime<Utc>| d.format("%Y-%m-%d %H:%M:%S").to_string();
        let now = Utc::now();
        c.execute("INSERT INTO sessions VALUES ('a','/x',?1,100,500)", [fmt(now - Duration::hours(1))]).unwrap();
        c.execute("INSERT INTO sessions VALUES ('b','/x',?1,70,NULL)", [fmt(now - Duration::hours(2))]).unwrap();
        c.execute("INSERT INTO sessions VALUES ('c','/x',?1,9999,9999)", [fmt(now - Duration::hours(48))]).unwrap();
    }
    let t = goose::read_totals(&db, Utc::now()).unwrap();
    assert_eq!((t.tokens_24h, t.sessions_24h), (570, 2));
    let p = goose::GooseProvider::new(Some(db));
    p.ingest_event(&AgentEvent::Done { reason: StopReason::EndTurn });
    let s = p.snapshot();
    assert_eq!(s.window("24h tokens").unwrap().source, Source::LocalLog);
}

// ------------------------------------------------------------------ pool

#[tokio::test]
async fn pool_aggregates_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    {
        let pool = QuotaPool::with_persistence(path.clone());
        let rx = pool.subscribe();
        let acp = Arc::new(acp::AcpGenericProvider::new("opencode".into()));
        let cl = claude::ClaudeProvider::new(claude::ClaudeOptions { offline: true, ..Default::default() });
        pool.add(acp.clone());
        pool.add(cl.clone());
        let line = std::fs::read_to_string(fixture("claude_stream.jsonl")).unwrap();
        for l in line.lines() {
            pool.ingest(&"claude".into(), &raw(l));
        }
        let st = tokio::time::timeout(StdDuration::from_secs(5), async {
            loop {
                let st = rx.recv().await.unwrap();
                if st.snapshots.get(&"claude".into()).and_then(|s| s.window("5h")).is_some() && st.snapshots.contains_key(&"opencode".into()) {
                    return st;
                }
            }
        })
        .await
        .unwrap();
        let installed = ["claude".into(), "opencode".into()];
        let r = route(&st, &RouteRequest { phase: tandem_core::TaskPhase::Implement, pins: None, exclude: vec![], prefs: Default::default(), require_installed: &installed });
        assert_eq!(r[0].agent.as_str(), "claude"); // 78% headroom beats unknown 0.5
    }
    let pool = QuotaPool::with_persistence(path);
    let st = pool.state();
    assert_eq!(st.snapshots[&"claude".into()].window("5h").unwrap().used, Some(22.0));
}
