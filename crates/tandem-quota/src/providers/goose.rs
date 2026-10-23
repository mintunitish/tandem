//! Goose: `~/.local/share/goose/sessions/sessions.db` per-session token columns, opened
//! read-only. Quota belongs to the upstream provider → status Unknown, tokens only.

use crate::model::{QuotaSnapshot, Source, Unit, Window, WindowKind};
use crate::provider::{ProviderCore, QuotaProvider};
use crate::safe_fs;
use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use futures::future::BoxFuture;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use tandem_core::{AgentEvent, AgentId, Receiver};

pub const AGENT: &str = "goose";

pub fn default_db_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".local/share/goose/sessions/sessions.db"))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GooseTotals {
    pub tokens_24h: u64,
    pub sessions_24h: u64,
    pub last_updated: Option<DateTime<Utc>>,
}

/// Sums `accumulated_total_tokens` (falling back to `total_tokens`) of sessions updated
/// in the 24h before `now`.
pub fn read_totals(db: &Path, now: DateTime<Utc>) -> Result<GooseTotals> {
    let db = safe_fs::check(db)?;
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    let since = (now - Duration::hours(24)).format("%Y-%m-%d %H:%M:%S").to_string();
    let (tokens, n, last): (i64, i64, Option<String>) = conn.query_row(
        "SELECT COALESCE(SUM(COALESCE(accumulated_total_tokens, total_tokens, 0)), 0), COUNT(*), MAX(updated_at)
         FROM sessions WHERE updated_at >= ?1",
        [since],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let last_updated = last.and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()).map(|n| n.and_utc());
    Ok(GooseTotals { tokens_24h: tokens.max(0) as u64, sessions_24h: n.max(0) as u64, last_updated })
}

pub struct GooseProvider {
    core: ProviderCore,
    db: Option<PathBuf>,
}

impl GooseProvider {
    pub fn new(db: Option<PathBuf>) -> Self {
        Self { core: ProviderCore::new(AGENT.into()), db }
    }
    /// Re-reads the database. Errors are recorded in the snapshot.
    pub fn refresh(&self) {
        let Some(db) = &self.db else { return };
        if !db.exists() {
            return;
        }
        let now = Utc::now();
        match read_totals(db, now) {
            Ok(t) => {
                let mut d = self.core.delta();
                d.windows.push(
                    Window::new("24h tokens", Unit::Tokens, Source::LocalLog, now)
                        .with_kind(WindowKind::Usage)
                        .with_amounts(Some(t.tokens_24h as f64), None),
                );
                self.core.apply(d);
            }
            Err(e) => self.core.set_error(format!("goose sessions.db: {e:#}")),
        }
    }
}

impl QuotaProvider for GooseProvider {
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
        Box::pin(async move { self.refresh() })
    }
    fn ingest_event(&self, ev: &AgentEvent) {
        if matches!(ev, AgentEvent::Done { .. }) {
            self.refresh();
        }
    }
}
