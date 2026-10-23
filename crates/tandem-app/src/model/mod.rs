//! Small in-crate stand-ins for the backend crates. Shapes follow `tandem-core` (used directly
//! where it already has the type) so wiring `tandem-project`, `tandem-git`, `tandem-acp`,
//! `tandem-quota`, `tandem-review` and `tandem-db` later is mechanical.

pub mod agents;
pub mod fuzzy;
pub mod project;
pub mod review;

use std::path::PathBuf;

/// Git working-tree status of one file. Mirrors `tandem_git::StatusEntry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitFileStatus {
    Added,
    Modified,
    Deleted,
    Untracked,
}

#[derive(Clone, Debug)]
pub struct GitChange {
    pub path: PathBuf,
    pub status: GitFileStatus,
}

/// Integration point: `tandem_git::status(root) -> Vec<StatusEntry>`.
pub fn fake_git_changes() -> Vec<GitChange> {
    use GitFileStatus::*;
    [
        ("src/http/client.ts", Modified),
        ("src/http/retry.ts", Added),
        ("src/http/retry.test.ts", Untracked),
        ("src/legacy/xhr.ts", Deleted),
        ("package.json", Modified),
    ]
    .into_iter()
    .map(|(p, s)| GitChange { path: p.into(), status: s })
    .collect()
}

/// A database connection as `tandem_db::ConnectionConfig` will expose it.
#[derive(Clone, Debug)]
pub struct DbConnection {
    pub name: String,
    pub driver: &'static str,
    pub tables: Vec<&'static str>,
}

pub fn fake_db_connections() -> Vec<DbConnection> {
    vec![
        DbConnection { name: "local-postgres".into(), driver: "PostgreSQL", tables: vec!["users", "orders", "order_items", "products", "sessions"] },
        DbConnection { name: "analytics.sqlite".into(), driver: "SQLite", tables: vec!["events", "daily_rollup"] },
    ]
}

/// Integration point: `tandem_db::Query::rows()` streaming into this shape.
pub fn fake_query_rows() -> (Vec<&'static str>, Vec<Vec<String>>) {
    let cols = vec!["id", "email", "plan", "created_at", "orders"];
    let rows = (1..=40)
        .map(|i| {
            vec![
                i.to_string(),
                format!("user{i}@example.com"),
                ["free", "pro", "team"][i % 3].to_string(),
                format!("2026-10-{:02} 09:{:02}:00", (i % 24) + 1, (i * 7) % 60),
                ((i * 13) % 37).to_string(),
            ]
        })
        .collect();
    (cols, rows)
}

#[derive(Clone, Debug)]
pub struct HarnessRow {
    pub agent: &'static str,
    pub pass: u32,
    pub total: u32,
    pub median_time: &'static str,
    pub cost: &'static str,
    pub conformance: &'static str,
}

/// Integration point: `tandem_harness::Report::rows()`.
pub fn fake_leaderboard() -> Vec<HarnessRow> {
    vec![
        HarnessRow { agent: "Claude Code", pass: 18, total: 20, median_time: "3m 41s", cost: "$0.00 (Max)", conformance: "5/5" },
        HarnessRow { agent: "Codex", pass: 16, total: 20, median_time: "4m 02s", cost: "$0.00 (Pro)", conformance: "5/5" },
        HarnessRow { agent: "Gemini CLI", pass: 13, total: 20, median_time: "2m 55s", cost: "$0.41", conformance: "4/5" },
        HarnessRow { agent: "OpenCode", pass: 12, total: 20, median_time: "5m 10s", cost: "$1.12", conformance: "4/5" },
        HarnessRow { agent: "Goose", pass: 9, total: 20, median_time: "6m 33s", cost: "$0.87", conformance: "3/5" },
    ]
}
