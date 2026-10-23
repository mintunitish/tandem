//! Codex offline fallback: `~/.codex/sessions/YYYY/MM/DD/*.jsonl` rows
//! `{"type":"event_msg","payload":{"type":"token_count","info":{..},"rate_limits":{..}}}`.

use super::codex::{parse_rate_limits, parse_token_usage, AGENT};
use crate::model::{parse_ts, QuotaSnapshot, Source};
use crate::safe_fs;
use chrono::Utc;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// How much of each log file's tail to scan before falling back to the whole file.
const TAIL_BYTES: u64 = 512 * 1024;
/// Newest files inspected when the newest one has no rate-limit row.
const MAX_FILES: usize = 5;

/// One `token_count` row → delta (rate limits + context), Source::LocalLog.
pub fn parse_row(v: &Value) -> Option<QuotaSnapshot> {
    if v.get("type").and_then(|t| t.as_str()) != Some("event_msg") {
        return None;
    }
    let p = v.get("payload")?;
    if p.get("type").and_then(|t| t.as_str()) != Some("token_count") {
        return None;
    }
    let at = v.get("timestamp").and_then(parse_ts).unwrap_or_else(Utc::now);
    let mut d = QuotaSnapshot::empty(AGENT.into(), at);
    if let Some(rl) = p.get("rate_limits").filter(|r| r.is_object()) {
        d = parse_rate_limits(rl, Source::LocalLog, at, false);
    }
    if let Some(info) = p.get("info").filter(|i| i.is_object()) {
        d.windows.extend(parse_token_usage(info, Source::LocalLog, at).windows);
    }
    (!d.windows.is_empty()).then_some(d)
}

/// Last row carrying `rate_limits` in `text` (latest wins).
pub fn latest_in_text(text: &str) -> Option<QuotaSnapshot> {
    text.lines().rev().filter(|l| l.contains("\"token_count\"") && l.contains("\"rate_limits\"")).find_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        let d = parse_row(&v)?;
        d.windows.iter().any(|w| w.window_mins.is_some() || w.label == "primary").then_some(d)
    })
}

pub fn latest_in_file(path: &Path) -> Option<QuotaSnapshot> {
    let tail = safe_fs::safe_read_tail(path, TAIL_BYTES).ok()?;
    latest_in_text(&tail).or_else(|| latest_in_text(&safe_fs::safe_read_to_string(path).ok()?))
}

/// Session files under `dir`, newest first.
pub fn session_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    safe_fs::walk_files(dir, "jsonl", &mut files);
    let mut with_t: Vec<_> = files.into_iter().filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p))).collect();
    with_t.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    with_t.into_iter().map(|(_, p)| p).collect()
}

pub fn latest_from_dir(dir: &Path) -> Option<QuotaSnapshot> {
    session_files(dir).iter().take(MAX_FILES).find_map(|p| latest_in_file(p))
}
