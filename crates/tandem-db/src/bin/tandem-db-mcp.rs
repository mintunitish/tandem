//! `tandem-db-mcp --connection <name> [--allow-write] [--allow-ddl] [--allow-dangerous [--confirm-file <path>]]`
//!
//! Stdio MCP server for one saved connection. Logs go to stderr only.

use clap::Parser;
use std::sync::Arc;
use tandem_db::{ConfirmFile, Confirmer, ConnectOptions, ConnectionStore, Db, DenyAll, McpPolicy, TandemDbServer};

#[derive(Parser, Debug)]
#[command(name = "tandem-db-mcp", about = "MCP server exposing a Tandem database connection (read-only by default)")]
struct Args {
    /// Saved connection name (see ~/.tandem/db/connections.json).
    #[arg(long)]
    connection: String,
    /// Allow INSERT/UPDATE/DELETE with WHERE.
    #[arg(long)]
    allow_write: bool,
    /// Allow CREATE/ALTER.
    #[arg(long)]
    allow_ddl: bool,
    /// Allow destructive statements, each only once approved: its hash must be in
    /// `--confirm-file`, approved within the last 15 minutes. Each runs on its own
    /// writable connection; the main one stays read-only unless --allow-write/--allow-ddl.
    #[arg(long)]
    allow_dangerous: bool,
    /// JSON object `{ sql_hash(connection, sql): approved_at }` the IDE writes after each
    /// red-card approval (`~/.tandem/db/confirm/<project>.json`).
    #[arg(long)]
    confirm_file: Option<std::path::PathBuf>,
    /// Override the connections file (default ~/.tandem/db/connections.json or $TANDEM_DB_STORE).
    #[arg(long, hide = true)]
    store: Option<std::path::PathBuf>,
    /// Print the tool definitions JSON (for hash pinning) and exit.
    #[arg(long)]
    print_tools: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("TANDEM_DB_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_ansi(false)
        .init();
    let args = Args::parse();
    if args.print_tools {
        println!("{}", serde_json::to_string_pretty(&tandem_db::tool_definitions_json())?);
        return Ok(());
    }
    let store = Arc::new(match &args.store {
        Some(p) => ConnectionStore::open(p.clone(), Box::new(tandem_db::KeychainStore)),
        None => ConnectionStore::open_default()?,
    });
    let cfg = store
        .get(&args.connection)?
        .ok_or_else(|| anyhow::anyhow!("unknown connection '{}'", args.connection))?;
    let confirmer: Box<dyn Confirmer> = match &args.confirm_file {
        _ if !args.allow_dangerous => Box::new(DenyAll),
        Some(f) => Box::new(ConfirmFile::new(f.clone())),
        None => {
            tracing::warn!("--allow-dangerous without --confirm-file: every destructive statement is refused");
            Box::new(DenyAll)
        }
    };
    let policy = McpPolicy::new(args.allow_write, args.allow_ddl, args.allow_dangerous, confirmer).for_connection(cfg.name.clone());
    let password = store.password(&cfg)?;
    let opts = ConnectOptions { read_only: !policy.needs_writable_connection(), ..ConnectOptions::default() };
    let db = Db::connect(&cfg, password.as_ref(), opts).await?;
    drop(password);
    tracing::info!(connection = %cfg, read_only = db.is_read_only(), "serving MCP on stdio");
    let mut server = TandemDbServer::new(db, policy);
    if args.allow_dangerous {
        // Approved destructive statements get a writable connection of their own.
        let (store, cfg) = (store.clone(), cfg.clone());
        server = server.with_writer(Arc::new(move || {
            let (store, cfg) = (store.clone(), cfg.clone());
            Box::pin(async move {
                let password = store.password(&cfg)?;
                Db::connect(&cfg, password.as_ref(), ConnectOptions { read_only: false, ..ConnectOptions::default() }).await
            })
        }));
    }
    server.serve_stdio().await
}
