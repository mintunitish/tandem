//! Claude offline fallback: ccusage-style 5-hour blocks over
//! `~/.claude/projects/**/*.jsonl` assistant `message.usage` (undocumented format).
//!
//! Tokens only. A percent observation ("5h" from `rate_limit_event`/status line) taken
//! while a block is active calibrates a token ceiling per plan, stored in
//! `~/.tandem/quota/calibration.json`; later blocks then get a fraction (Source::Heuristic).

use super::{get, usage_from_json, usage_total};
use crate::model::{parse_ts, Source, Unit, Window, WindowKind};
use crate::safe_fs;
use chrono::{DateTime, Duration, DurationRound, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use tandem_core::Usage;

pub const BLOCK: Duration = Duration::hours(5);
/// Fractions below this are too noisy to calibrate from.
const MIN_CALIBRATION_FRACTION: f64 = 0.05;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub at: DateTime<Utc>,
    pub usage: Usage,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub start: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub usage: Usage,
    pub entries: usize,
}
impl Block {
    pub fn end(&self) -> DateTime<Utc> {
        self.start + BLOCK
    }
    pub fn tokens(&self) -> u64 {
        usage_total(&self.usage)
    }
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        now < self.end() && now - self.last_at < BLOCK
    }
}

/// One transcript line → (dedupe key, entry). Only assistant rows with usage count.
pub fn parse_line(v: &Value) -> Option<(String, Entry)> {
    if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return None;
    }
    let msg = v.get("message")?;
    let usage = msg.get("usage").and_then(usage_from_json)?;
    let at = v.get("timestamp").and_then(parse_ts)?;
    let mid = get(msg, &["id"]).and_then(|x| x.as_str()).unwrap_or("");
    let rid = get(v, &["requestId"]).and_then(|x| x.as_str()).unwrap_or("");
    let key = if mid.is_empty() && rid.is_empty() {
        get(v, &["uuid"]).and_then(|x| x.as_str()).unwrap_or("").to_string()
    } else {
        format!("{mid}:{rid}")
    };
    Some((key, Entry { at, usage }))
}

/// Entries from transcripts modified since `since`, deduplicated, sorted by time.
pub fn read_entries(projects_dir: &Path, since: DateTime<Utc>) -> Vec<Entry> {
    let mut files = Vec::new();
    safe_fs::walk_files(projects_dir, "jsonl", &mut files);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for f in files {
        let fresh = std::fs::metadata(&f).ok().and_then(|m| m.modified().ok()).map(DateTime::<Utc>::from).is_some_and(|t| t >= since);
        if !fresh {
            continue;
        }
        let Ok(text) = safe_fs::safe_read_to_string(&f) else { continue };
        for line in text.lines().filter(|l| l.contains("\"assistant\"") && l.contains("\"usage\"")) {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if let Some((k, e)) = parse_line(&v)
                && e.at >= since && (k.is_empty() || seen.insert(k)) {
                    out.push(e);
                }
        }
    }
    out.sort_by_key(|e| e.at);
    out
}

/// Groups sorted entries into 5h blocks: a block starts at the hour-floor of the first
/// entry after a gap > 5h (or once the current block's 5h have elapsed).
pub fn blocks(entries: &[Entry]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for e in entries {
        let new_block = match out.last() {
            None => true,
            Some(b) => e.at - b.start >= BLOCK || e.at - b.last_at > BLOCK,
        };
        if new_block {
            let start = e.at.duration_trunc(Duration::hours(1)).unwrap_or(e.at);
            out.push(Block { start, last_at: e.at, usage: Usage::default(), entries: 0 });
        }
        let b = out.last_mut().expect("pushed above");
        b.usage.add(&e.usage);
        b.last_at = e.at;
        b.entries += 1;
    }
    out
}

pub fn active_block(projects_dir: &Path, now: DateTime<Utc>) -> Option<Block> {
    // 24h of history is enough to place the current block boundary in practice.
    let entries = read_entries(projects_dir, now - Duration::hours(24));
    blocks(&entries).pop().filter(|b| b.is_active(now))
}

/// Per-plan token ceilings learned from percent observations.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    /// plan ("default" when unknown) → label → ceiling.
    pub plans: BTreeMap<String, BTreeMap<String, CalibrationEntry>>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationEntry {
    pub ceiling_tokens: f64,
    pub observed_at: DateTime<Utc>,
}

impl Calibration {
    pub fn load(path: &Path) -> Self {
        safe_fs::safe_read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        safe_fs::atomic_write(path, &serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)
    }
    pub fn ceiling(&self, plan: Option<&str>, label: &str) -> Option<f64> {
        self.plans.get(plan.unwrap_or("default")).and_then(|m| m.get(label)).map(|e| e.ceiling_tokens)
    }
    /// Records `ceiling = tokens / fraction`. Returns false when the sample is too small.
    pub fn observe(&mut self, plan: Option<&str>, label: &str, tokens: u64, fraction: f64, now: DateTime<Utc>) -> bool {
        if fraction < MIN_CALIBRATION_FRACTION || tokens == 0 {
            return false;
        }
        let e = CalibrationEntry { ceiling_tokens: tokens as f64 / fraction, observed_at: now };
        self.plans.entry(plan.unwrap_or("default").to_string()).or_default().insert(label.to_string(), e);
        true
    }
}

/// Heuristic window for the active block: calibrated → "5h" quota window with a fraction;
/// otherwise "5h tokens" (usage only, so it never masks a vendor percent).
pub fn block_window(b: &Block, ceiling: Option<f64>, now: DateTime<Utc>) -> Window {
    let used = Some(b.tokens() as f64);
    let w = match ceiling {
        Some(c) => Window::new("5h", Unit::Tokens, Source::Heuristic, now).with_amounts(used, Some(c)),
        None => Window::new("5h tokens", Unit::Tokens, Source::Heuristic, now).with_kind(WindowKind::Usage).with_amounts(used, None),
    };
    let mut w = w.with_reset(Some(b.end()));
    w.window_mins = Some(300);
    w
}
