//! `tandem-db-mcp --connection <name> [--allow-write] [--allow-ddl] [--allow-dangerous]`
//!
//! Stdio MCP server for one saved connection. Logs go to stderr only.

use clap::Parser;
use tandem_db::{ConnectOptions, ConnectionStore, Db, McpPolicy, TandemDbServer};

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
    /// Allow destructive statements; also requires TANDEM_DB_CONFIRM=<connection>.
    #[arg(long)]
    allow_dangerous: bool,
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
    let store = match &args.store {
        Some(p) => ConnectionStore::open(p.clone(), Box::new(tandem_db::KeychainStore)),
        None => ConnectionStore::open_default()?,
    };
    let cfg = store
        .get(&args.connection)?
        .ok_or_else(|| anyhow::anyhow!("unknown connection '{}'", args.connection))?;
    let policy = McpPolicy::from_flags(&cfg.name, args.allow_write, args.allow_ddl, args.allow_dangerous);
    if args.allow_dangerous && !policy.dangerous_allowed() {
        tracing::warn!("--allow-dangerous ignored: TANDEM_DB_CONFIRM does not match the connection name");
    }
    let password = store.password(&cfg)?;
    let opts = ConnectOptions { read_only: !policy.needs_writable_connection(), ..ConnectOptions::default() };
    let db = Db::connect(&cfg, password.as_ref(), opts).await?;
    drop(password);
    tracing::info!(connection = %cfg, read_only = db.is_read_only(), "serving MCP on stdio");
    TandemDbServer::new(db, policy).serve_stdio().await
}
