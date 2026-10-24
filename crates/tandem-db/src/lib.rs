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
pub use mcp::{ConfirmFile, Confirmer, DenyAll, McpPolicy, TandemDbServer, sql_hash, tool_definitions_json};
pub use schema::{ColumnInfo, ForeignKey, Index, Table, TableKind, quote_ident};
pub use value::{Column, QueryOptions, QueryResult, StreamItem, Value};

/// Name of the stdio MCP server binary.
pub const MCP_BIN: &str = "tandem-db-mcp";

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
    let env = std::env::var_os("TANDEM_DB_STORE")
        .map(|s| [("TANDEM_DB_STORE".to_string(), s.to_string_lossy().into_owned())].into())
        .unwrap_or_default();
    tandem_core::McpServerSpec {
        name: server_name(connection),
        command: mcp_binary().unwrap_or_else(|| MCP_BIN.into()),
        args,
        env,
    }
}
