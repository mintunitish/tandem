//! Quota pool wiring: one provider per detected agent, using each agent's *own* signals.
//!
//! - Codex: `codex app-server` `account/rateLimits/read` (official), session logs as fallback.
//! - Claude: stream events (rate-limit events, usage) + the status-line sink + local transcripts.
//! - Gemini: per-session usage from stream stats.
//! - Goose: its local sessions.db (24h token usage).
//! - Everything else: generic ACP usage updates, otherwise shown honestly as unknown.
//!
//! `TANDEM_QUOTA_OFFLINE=1` keeps every provider in-memory (no log scans, no app-server); the
//! UI tests set it.

use std::sync::Arc;
use tandem_core::AgentId;
use tandem_quota::providers::{acp::AcpGenericProvider, claude, codex, gemini::GeminiProvider, goose};
use tandem_quota::{QuotaPool, QuotaProvider};

pub fn offline() -> bool {
    std::env::var_os("TANDEM_QUOTA_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Build the pool and register providers on the tokio runtime. The UI polls
/// [`QuotaPool::state`] (no cross-thread wakeups into gpui).
pub fn start_pool(agents: Vec<AgentId>) -> Arc<QuotaPool> {
    let _guard = crate::runtime::tokio().enter();
    let pool = if offline() { QuotaPool::new() } else { QuotaPool::with_default_persistence() };
    let p = pool.clone();
    crate::runtime::tokio().spawn(async move {
        let offline = offline();
        for agent in agents {
            let provider: Arc<dyn QuotaProvider> = match agent.as_str() {
                "claude" if !offline => {
                    let c = claude::ClaudeProvider::new(claude::ClaudeOptions::default());
                    if let Err(e) = c.start() {
                        tracing_warn(&format!("claude quota provider: {e:#}"));
                    }
                    c
                }
                "claude" => claude::ClaudeProvider::new(claude::ClaudeOptions { statusline_file: None, projects_dir: None, calibration_path: None, ..Default::default() }),
                "codex" if !offline => {
                    let c = codex::CodexProvider::new(codex::CodexOptions::default());
                    let c2 = c.clone();
                    tokio::spawn(async move {
                        let _ = c2.start().await;
                    });
                    c
                }
                "codex" => codex::CodexProvider::new(codex::CodexOptions { sessions_dir: None, program: "/nonexistent".into(), ..Default::default() }),
                "gemini" => Arc::new(GeminiProvider::new(agent.clone())),
                "goose" => {
                    let g = goose::GooseProvider::new(if offline { None } else { goose::default_db_path() });
                    g.refresh();
                    Arc::new(g)
                }
                _ => Arc::new(AcpGenericProvider::new(agent.clone())),
            };
            p.add(provider);
        }
    });
    pool
}

fn tracing_warn(msg: &str) {
    eprintln!("tandem: {msg}");
}
