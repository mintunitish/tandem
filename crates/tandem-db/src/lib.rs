//! tandem-db: DataGrip-lite backend for Tandem plus an MCP server (`tandem-db-mcp`)
//! that exposes a connection to agents, read-only by default.
//!
//! Layers of protection for agent access:
//! 1. [`classify`] gates statements by class (read/write/ddl/dangerous);
//! 2. [`Db`] enforces read-only at the engine level, so a classifier bypass still cannot write.

pub mod classify;
pub mod config;
pub mod db;
pub mod error;
pub mod mcp;
pub mod schema;
pub mod value;

pub use classify::{Classification, SqlClass, classify, classify_detailed, classify_for};
pub use config::{
    ConnectionConfig, ConnectionStore, DbKind, KeychainStore, MemorySecretStore, Secret, SecretStore, SecretUrl,
    SslMode,
};
pub use db::{ConnectOptions, Db};
pub use error::DbError;
pub use mcp::{APPROVAL_TTL, ConfirmFile, Confirmer, DenyAll, McpPolicy, TandemDbServer, WriterFactory, sql_hash, tool_definitions_json};
pub use schema::{ColumnInfo, ForeignKey, Index, Table, TableKind, quote_ident};
pub use value::{Column, QueryOptions, QueryResult, StreamItem, Value};

/// Name of the stdio MCP server binary.
pub const MCP_BIN: &str = "tandem-db-mcp";

/// Env var overriding the connections file (`~/.tandem/db/connections.json`).
pub const STORE_ENV: &str = "TANDEM_DB_STORE";

/// Filesystem-safe, collision-resistant name for a project: its canonical path with
/// every non-alphanumeric run turned into `-`, plus a short hash of the full path.
pub fn project_slug(root: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let s = canon.to_string_lossy();
    let mut slug = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let tail: String = slug.chars().rev().take(48).collect::<Vec<_>>().into_iter().rev().collect();
    let hash: String = Sha256::digest(s.as_bytes()).iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{}-{hash}", tail.trim_matches('-'))
}

/// `~/.tandem/db/confirm/<project-slug>.json`: the user's destructive-SQL approvals for a
/// project ([`ConfirmFile`]). Outside the repository, so nothing in the project can
/// pre-approve statements.
pub fn confirm_path(root: &std::path::Path) -> std::path::PathBuf {
    tandem_core::fs::tandem_home().join("db").join("confirm").join(format!("{}.json", project_slug(root)))
}

/// `~/.tandem/db/agents-<project-slug>.json`: the user's per-project agent exposure,
/// the only place write / dangerous access can be granted.
pub fn user_agents_path(root: &std::path::Path) -> std::path::PathBuf {
    tandem_core::fs::tandem_home().join("db").join(format!("agents-{}.json", project_slug(root)))
}

/// `tandem-db-mcp` next to the running binary (cargo target dir / app bundle), else on PATH.
pub fn mcp_binary() -> Option<std::path::PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|e| {
        let dir = e.parent()?.to_path_buf();
        // Test binaries live in target/<profile>/deps.
        [dir.join(MCP_BIN), dir.parent()?.join(MCP_BIN)].into_iter().find(|p| p.is_file())
    });
    beside.or_else(|| tandem_core::fs::which_in(MCP_BIN, std::env::var_os("PATH").as_deref().unwrap_or_default()))
}

/// MCP server name for a saved connection: `tandem-db-<connection>`.
pub fn server_name(connection: &str) -> String {
    format!("tandem-db-{connection}")
}

/// The MCP server an agent gets for one saved connection: read-only unless `allow_write` /
/// `allow_ddl`. Named `tandem-db-<connection>`; `TANDEM_DB_STORE` is passed through.
pub fn server_spec(connection: &str, allow_write: bool, allow_ddl: bool) -> tandem_core::McpServerSpec {
    let mut args = vec!["--connection".to_string(), connection.to_string()];
    if allow_write {
        args.push("--allow-write".into());
    }
    if allow_ddl {
        args.push("--allow-ddl".into());
    }
    let env = std::env::var_os(STORE_ENV)
        .map(|s| [(STORE_ENV.to_string(), s.to_string_lossy().into_owned())].into())
        .unwrap_or_default();
    tandem_core::McpServerSpec {
        name: server_name(connection),
        command: mcp_binary().unwrap_or_else(|| MCP_BIN.into()),
        args,
        env,
    }
}
