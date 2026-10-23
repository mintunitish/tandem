//! `cargo run -p tandem-quota --example quota_demo -- --phase implement`
//!
//! Builds a pool from the agents' own local signals (Codex app-server, Claude logs and
//! status-line file, Goose sessions.db), prints it with bars, and a route recommendation.

use chrono::Utc;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tandem_core::{AgentId, TaskPhase};
use tandem_quota::providers::{claude, codex, gemini, goose};
use tandem_quota::*;

fn bar(frac: f64, width: usize) -> String {
    let filled = (frac.clamp(0.0, 1.0) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

fn parse_phase(s: &str) -> TaskPhase {
    match s {
        "plan" => TaskPhase::Plan,
        "implement" => TaskPhase::Implement,
        "test" => TaskPhase::Test,
        "review" => TaskPhase::Review,
        _ => TaskPhase::Any,
    }
}

fn age(s: &Window) -> String {
    let m = (Utc::now() - s.observed_at).num_minutes();
    if m < 1 { "now".into() } else if m < 120 { format!("{m}m ago") } else { format!("{}h ago", m / 60) }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let phase = args.iter().position(|a| a == "--phase").and_then(|i| args.get(i + 1)).map(|s| parse_phase(s)).unwrap_or(TaskPhase::Any);

    let pool = QuotaPool::with_default_persistence();
    let installed: Vec<AgentId> = ["claude", "codex", "gemini", "goose", "opencode"].into_iter().filter(|b| on_path(b)).map(AgentId::from).collect();

    if on_path("codex") {
        let p = codex::CodexProvider::new(codex::CodexOptions::default());
        if let Err(e) = p.start().await {
            eprintln!("codex app-server: {e:#} (using session logs)");
        }
        pool.add(p);
    }
    let c = claude::ClaudeProvider::new(claude::ClaudeOptions::default());
    c.start()?;
    pool.add(c);
    let g = Arc::new(goose::GooseProvider::new(goose::default_db_path()));
    g.refresh();
    pool.add(g);
    pool.add(Arc::new(gemini::GeminiProvider::new("gemini".into())));
    tokio::time::sleep(Duration::from_millis(300)).await;

    let st = pool.state();
    println!("{:<9} {:<6} {:<8} {:<26} {:<34} {:<11} age", "agent", "plan", "status", "window", "usage", "source");
    for (agent, s) in &st.snapshots {
        let mut first = true;
        let head = |first: bool| {
            if first {
                format!("{:<9} {:<6} {:<8}", agent.as_str(), s.plan.as_deref().unwrap_or("-"), format!("{:?}", s.status).to_lowercase())
            } else {
                format!("{:<9} {:<6} {:<8}", "", "", "")
            }
        };
        if s.windows.is_empty() {
            println!("{} {}", head(true), s.error.as_deref().unwrap_or("no signal yet"));
        }
        for w in &s.windows {
            let usage = match (w.used_fraction, w.used) {
                (Some(f), _) => format!("{} {:>5.1}%", bar(f, 20), f * 100.0),
                (None, Some(u)) => match w.unit {
                    Unit::Usd => format!("${u:.2}"),
                    _ => format!("{u:.0} {:?}", w.unit).to_lowercase(),
                },
                _ => "?".into(),
            };
            let reset = w.reset_at.map(|r| format!(" (resets {})", r.format("%m-%d %H:%M"))).unwrap_or_default();
            let label = format!("{}{reset}", w.label);
            let src = format!("{:?}", w.source);
            println!("{} {:<26} {:<34} {:<11} {}", head(first), label, usage, src, age(w));
            first = false;
        }
    }

    let mut prefs = BTreeMap::new();
    prefs.insert(TaskPhase::Plan.into(), vec!["claude".into()]);
    prefs.insert(TaskPhase::Implement.into(), vec!["codex".into(), "claude".into()]);
    prefs.insert(TaskPhase::Test.into(), vec!["gemini".into()]);
    let req = RouteRequest { phase, pins: None, exclude: vec![], prefs, require_installed: &installed };
    println!("\nroute for {phase:?}:");
    for (i, c) in route(&st, &req).iter().enumerate() {
        println!("  {}. {:<9} score {:.2}  {}", i + 1, c.agent.as_str(), c.score, c.reason);
    }
    Ok(())
}
