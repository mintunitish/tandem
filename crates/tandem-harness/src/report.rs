//! Report model, markdown rendering, leaderboard and a dependency-free SVG chart.

use crate::eval::CheckResult;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;
use tandem_core::{AgentId, StopReason, Usage};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Conformance,
    Bench,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    Skip,
    /// Not run on purpose (e.g. the sandbox blocks the agent's login pending a
    /// policy decision). Excluded from the leaderboard like `Skip`.
    Blocked,
}

impl Status {
    pub fn icon(self) -> &'static str {
        match self {
            Status::Pass => "✅",
            Status::Fail => "❌",
            Status::Skip => "⏭",
            Status::Blocked => "⛔",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub kind: RunKind,
    pub agent: AgentId,
    /// Scenario or bench task id.
    pub id: String,
    pub title: String,
    pub category: Option<String>,
    pub repeat: u32,
    pub status: Status,
    pub reason: String,
    pub checks: Vec<CheckResult>,
    pub elapsed_s: f64,
    pub usage: Usage,
    pub stop: Option<StopReason>,
    /// Log path relative to the report directory.
    pub log: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub date: String,
    pub generated_at: DateTime<Utc>,
    pub runner: String,
    pub results: Vec<RunResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LeaderRow {
    pub agent: AgentId,
    pub runs: usize,
    pub passed: usize,
    /// 0..=100.
    pub pass_rate: f64,
    pub median_time_s: f64,
    /// Input + output + cache read + cache write.
    pub tokens: u64,
    pub cost_usd: Option<f64>,
    /// pass% − 0.1 × normalized time (median / slowest median × 100).
    pub score: f64,
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

fn esc_md(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

fn fmt_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1e6)
    } else if t >= 1_000 {
        format!("{:.1}k", t as f64 / 1e3)
    } else {
        t.to_string()
    }
}

impl RunResult {
    /// Counted in pass rates (not skipped or blocked).
    pub fn counts(&self) -> bool {
        !matches!(self.status, Status::Skip | Status::Blocked)
    }
    fn key(&self) -> (RunKind, &str, &str, u32) {
        (self.kind, self.agent.as_str(), self.id.as_str(), self.repeat)
    }
}

/// Reads a report JSON; a missing, empty or unparsable file yields `None`
/// (unparsable is warned about and left in place as `<name>.bad`).
fn read_report(p: &Path) -> Option<Report> {
    let text = std::fs::read_to_string(p).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    match serde_json::from_str::<Report>(&text) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("warning: ignoring unparsable {}: {e}", p.display());
            let _ = std::fs::rename(p, p.with_extension("json.bad"));
            None
        }
    }
}

/// Write via a temp file + rename so a killed run never leaves a truncated JSON.
fn write_atomic(p: &Path, content: &str) -> Result<()> {
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

impl Report {
    /// Results of `newer` replace this report's results with the same
    /// (kind, agent, id, repeat); other results are kept, new ones appended.
    pub fn upsert(&mut self, newer: Report) {
        for r in newer.results {
            match self.results.iter_mut().find(|x| x.key() == r.key()) {
                Some(slot) => *slot = r,
                None => self.results.push(r),
            }
        }
        self.generated_at = self.generated_at.max(newer.generated_at);
        if !self.runner.split('+').any(|x| x == newer.runner) {
            self.runner = format!("{}+{}", self.runner, newer.runner);
        }
    }

    pub fn new(date: &str, runner: &str, results: Vec<RunResult>) -> Self {
        Self { date: date.into(), generated_at: Utc::now(), runner: runner.into(), results }
    }

    /// Concatenates results from several reports (e.g. conformance + bench).
    pub fn merge(reports: Vec<Report>) -> Option<Report> {
        let mut it = reports.into_iter();
        let mut first = it.next()?;
        for r in it {
            if !first.runner.split('+').any(|x| x == r.runner) {
                first.runner = format!("{}+{}", first.runner, r.runner);
            }
            first.generated_at = first.generated_at.max(r.generated_at);
            first.results.extend(r.results);
        }
        Some(first)
    }

    fn agents(&self) -> Vec<AgentId> {
        let mut v: Vec<AgentId> = Vec::new();
        for r in &self.results {
            if !v.contains(&r.agent) {
                v.push(r.agent.clone());
            }
        }
        v
    }

    fn ids(&self, kind: RunKind) -> Vec<(String, String, Option<String>)> {
        let mut v: Vec<(String, String, Option<String>)> = Vec::new();
        for r in self.results.iter().filter(|r| r.kind == kind) {
            if !v.iter().any(|x| x.0 == r.id) {
                v.push((r.id.clone(), r.title.clone(), r.category.clone()));
            }
        }
        v
    }

    /// Leaderboard over bench runs if any exist, else conformance runs. Skips excluded.
    pub fn leaderboard(&self) -> Vec<LeaderRow> {
        let kind = if self.results.iter().any(|r| r.kind == RunKind::Bench) { RunKind::Bench } else { RunKind::Conformance };
        let mut rows: Vec<LeaderRow> = self
            .agents()
            .into_iter()
            .filter_map(|agent| {
                let runs: Vec<&RunResult> =
                    self.results.iter().filter(|r| r.agent == agent && r.kind == kind && r.counts()).collect();
                if runs.is_empty() {
                    return None;
                }
                let passed = runs.iter().filter(|r| r.status == Status::Pass).count();
                let mut times: Vec<f64> = runs.iter().map(|r| r.elapsed_s).collect();
                let mut usage = Usage::default();
                for r in &runs {
                    usage.add(&r.usage);
                }
                Some(LeaderRow {
                    agent,
                    runs: runs.len(),
                    passed,
                    pass_rate: passed as f64 * 100.0 / runs.len() as f64,
                    median_time_s: median(&mut times),
                    // Real agents put most of the prompt in cache reads/writes; count them too.
                    tokens: usage.input_tokens + usage.output_tokens + usage.cache_read_tokens + usage.cache_write_tokens,
                    cost_usd: usage.cost_usd,
                    score: 0.0,
                })
            })
            .collect();
        let slowest = rows.iter().map(|r| r.median_time_s).fold(0.0_f64, f64::max);
        for r in &mut rows {
            let norm = if slowest > 0.0 { r.median_time_s / slowest * 100.0 } else { 0.0 };
            r.score = r.pass_rate - 0.1 * norm;
        }
        rows.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.agent.cmp(&b.agent)));
        rows
    }

    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        let agents = self.agents();
        let _ = writeln!(md, "# Tandem Harness Report: {}\n", self.date);
        let _ = writeln!(md, "Runner: `{}` · Agents: {}\n", self.runner, agents.iter().map(|a| format!("`{a}`")).collect::<Vec<_>>().join(", "));

        let lb = self.leaderboard();
        if !lb.is_empty() {
            let _ = writeln!(md, "## Leaderboard\n");
            let _ = writeln!(md, "| # | Agent | Pass rate | Median time | Tokens | Est. cost | Score |");
            let _ = writeln!(md, "|---|---|---|---|---|---|---|");
            for (i, r) in lb.iter().enumerate() {
                let cost = r.cost_usd.map(|c| format!("${c:.2}")).unwrap_or_else(|| "n/a".into());
                let _ = writeln!(
                    md,
                    "| {} | `{}` | {:.0}% ({}/{}) | {:.1}s | {} | {} | {:.1} |",
                    i + 1,
                    r.agent,
                    r.pass_rate,
                    r.passed,
                    r.runs,
                    r.median_time_s,
                    fmt_tokens(r.tokens),
                    cost,
                    r.score
                );
            }
            let _ = writeln!(md, "\nScore = pass% − 0.1 × normalized median time (slowest = 100).\n");
        }

        let conf = self.ids(RunKind::Conformance);
        if !conf.is_empty() {
            let _ = writeln!(md, "## Conformance\n");
            let _ = write!(md, "| Scenario |");
            for a in &agents {
                let _ = write!(md, " `{a}` |");
            }
            let _ = write!(md, "\n|---|");
            for _ in &agents {
                let _ = write!(md, "---|");
            }
            md.push('\n');
            for (id, _title, cat) in &conf {
                let _ = write!(md, "| `{id}` ({}) |", cat.as_deref().unwrap_or("-"));
                for a in &agents {
                    let cell = self
                        .results
                        .iter()
                        .find(|r| r.kind == RunKind::Conformance && &r.agent == a && &r.id == id)
                        .map(|r| {
                            if r.reason.is_empty() {
                                r.status.icon().to_string()
                            } else {
                                format!("{} {}", r.status.icon(), esc_md(&short(&r.reason, 60)))
                            }
                        })
                        .unwrap_or_else(|| "-".into());
                    let _ = write!(md, " {cell} |");
                }
                md.push('\n');
            }
            md.push('\n');
        }

        let bench = self.ids(RunKind::Bench);
        if !bench.is_empty() {
            let _ = writeln!(md, "## Bench\n");
            let _ = write!(md, "| Task |");
            for a in &agents {
                let _ = write!(md, " `{a}` |");
            }
            let _ = write!(md, "\n|---|");
            for _ in &agents {
                let _ = write!(md, "---|");
            }
            md.push('\n');
            for (id, _, _) in &bench {
                let _ = write!(md, "| `{id}` |");
                for a in &agents {
                    let runs: Vec<&RunResult> =
                        self.results.iter().filter(|r| r.kind == RunKind::Bench && &r.agent == a && &r.id == id).collect();
                    if runs.is_empty() {
                        let _ = write!(md, " - |");
                        continue;
                    }
                    let pass = runs.iter().filter(|r| r.status == Status::Pass).count();
                    let mut t: Vec<f64> = runs.iter().map(|r| r.elapsed_s).collect();
                    let icon = if pass == runs.len() { Status::Pass.icon() } else { Status::Fail.icon() };
                    let _ = write!(md, " {icon} {pass}/{} · {:.1}s |", runs.len(), median(&mut t));
                }
                md.push('\n');
            }
            md.push('\n');
        }

        let blocked: Vec<&RunResult> = self.results.iter().filter(|r| r.status == Status::Blocked).collect();
        if !blocked.is_empty() {
            let _ = writeln!(md, "## Blocked\n");
            for r in blocked {
                let _ = writeln!(md, "- `{}` / `{}`: {}", r.agent, r.id, esc_md(&r.reason));
            }
            md.push('\n');
        }

        let failures: Vec<&RunResult> = self.results.iter().filter(|r| r.status == Status::Fail).collect();
        if !failures.is_empty() {
            let _ = writeln!(md, "## Failures\n");
            for r in failures {
                let rep = if r.repeat > 0 { format!(" (repeat {})", r.repeat) } else { String::new() };
                let _ = writeln!(md, "- `{}` / `{}`{rep}: {}", r.agent, r.id, esc_md(&r.reason));
                for c in r.checks.iter().filter(|c| !c.passed && r.kind == RunKind::Conformance) {
                    let _ = writeln!(md, "  - {}: {}", esc_md(&c.check), esc_md(&c.detail));
                }
                if let Some(l) = &r.log {
                    let _ = writeln!(md, "  - log: `{l}`");
                }
            }
            md.push('\n');
        }
        md
    }

    /// Upserts into `<kind>.json` (by the kinds present; results for other
    /// agents/ids already there are kept), then re-renders the merged
    /// `report.json`, `report.md` and `leaderboard.svg` from every JSON in `dir`.
    pub fn write(&self, dir: &Path) -> Result<Report> {
        std::fs::create_dir_all(dir)?;
        let name = match (
            self.results.iter().any(|r| r.kind == RunKind::Conformance),
            self.results.iter().any(|r| r.kind == RunKind::Bench),
        ) {
            (true, false) => "conformance.json",
            (false, true) => "bench.json",
            _ => "report.json",
        };
        let path = dir.join(name);
        let combined = match read_report(&path) {
            Some(mut old) => {
                old.upsert(self.clone());
                old
            }
            None => self.clone(),
        };
        write_atomic(&path, &serde_json::to_string_pretty(&combined)?)?;
        render_dir(dir)
    }
}

/// Merges `conformance.json` and `bench.json` in `dir` (or `report.json` if
/// neither exists) and writes `report.json`, `report.md`, `leaderboard.svg`.
pub fn render_dir(dir: &Path) -> Result<Report> {
    let mut parts = Vec::new();
    for name in ["conformance.json", "bench.json"] {
        if let Some(r) = read_report(&dir.join(name)) {
            parts.push(r);
        }
    }
    if parts.is_empty() {
        let p = dir.join("report.json");
        let text = std::fs::read_to_string(&p).with_context(|| format!("no report JSON in {}", dir.display()))?;
        parts.push(serde_json::from_str::<Report>(&text)?);
    }
    let merged = Report::merge(parts).context("no reports")?;
    write_atomic(&dir.join("report.json"), &serde_json::to_string_pretty(&merged)?)?;
    write_atomic(&dir.join("report.md"), &merged.to_markdown())?;
    write_atomic(&dir.join("leaderboard.svg"), &leaderboard_svg(&merged.date, &merged.leaderboard()))?;
    Ok(merged)
}

fn esc_xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Horizontal bar chart of leaderboard scores. Hand-written SVG, no deps.
pub fn leaderboard_svg(date: &str, rows: &[LeaderRow]) -> String {
    const W: f64 = 640.0;
    const LABEL_W: f64 = 140.0;
    const ROW_H: f64 = 34.0;
    const TOP: f64 = 56.0;
    let bar_max = W - LABEL_W - 170.0;
    let h = TOP + ROW_H * rows.len().max(1) as f64 + 28.0;
    let palette = ["#4f46e5", "#0891b2", "#16a34a", "#ca8a04", "#dc2626", "#9333ea"];
    let mut s = String::new();
    let _ = writeln!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{h}" viewBox="0 0 {W} {h}" font-family="-apple-system,Segoe UI,Helvetica,Arial,sans-serif">"#
    );
    let _ = writeln!(s, r##"<rect width="100%" height="100%" rx="12" fill="#0f172a"/>"##);
    let _ = writeln!(
        s,
        r##"<text x="20" y="32" font-size="18" font-weight="600" fill="#f8fafc">Tandem Repo Leaderboard</text><text x="{}" y="32" font-size="12" fill="#94a3b8" text-anchor="end">{}</text>"##,
        W - 20.0,
        esc_xml(date)
    );
    if rows.is_empty() {
        let _ = writeln!(s, r##"<text x="20" y="{}" font-size="13" fill="#94a3b8">No runs yet</text>"##, TOP + 20.0);
    }
    for (i, r) in rows.iter().enumerate() {
        let y = TOP + ROW_H * i as f64;
        let len = (r.score.max(0.0) / 100.0 * bar_max).max(2.0);
        let color = palette[i % palette.len()];
        let _ = writeln!(
            s,
            r##"<text x="20" y="{:.1}" font-size="13" fill="#e2e8f0">{}</text><rect x="{LABEL_W}" y="{:.1}" width="{len:.1}" height="20" rx="4" fill="{color}"/><text x="{:.1}" y="{:.1}" font-size="12" fill="#e2e8f0">{:.1} · {:.0}% · {:.1}s</text>"##,
            y + 15.0,
            esc_xml(&short(r.agent.as_str(), 16)),
            y,
            LABEL_W + len + 8.0,
            y + 15.0,
            r.score,
            r.pass_rate,
            r.median_time_s
        );
    }
    let _ = writeln!(
        s,
        r##"<text x="20" y="{:.1}" font-size="11" fill="#64748b">score = pass% − 0.1 × normalized time · tandem-harness</text>"##,
        h - 12.0
    );
    s.push_str("</svg>\n");
    s
}
