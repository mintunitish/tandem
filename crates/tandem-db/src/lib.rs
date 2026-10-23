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
pub use mcp::{McpPolicy, TandemDbServer, tool_definitions_json};
pub use schema::{ColumnInfo, ForeignKey, Index, Table, TableKind, quote_ident};
pub use value::{Column, QueryOptions, QueryResult, StreamItem, Value};
