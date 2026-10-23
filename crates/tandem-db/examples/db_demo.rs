//! `cargo run -p tandem-db --example db_demo -- --sqlite <file> --sql "<query>" [--write]`

use clap::Parser;
use tandem_db::{ConnectOptions, ConnectionConfig, Db, QueryOptions};

#[derive(Parser)]
struct Args {
    /// SQLite database file.
    #[arg(long)]
    sqlite: std::path::PathBuf,
    /// SQL to run.
    #[arg(long)]
    sql: String,
    /// Open read-write (default read-only).
    #[arg(long)]
    write: bool,
    #[arg(long, default_value_t = 100)]
    max_rows: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cfg = ConnectionConfig::sqlite("demo", &args.sqlite);
    let opts = if args.write { ConnectOptions::read_write() } else { ConnectOptions::default() };
    let db = Db::connect(&cfg, None, opts).await?;
    println!("class: {}", tandem_db::classify_for(db.kind(), &args.sql));
    let r = db.query(&args.sql, &[], &QueryOptions { max_rows: args.max_rows, ..Default::default() }).await?;
    print!("{}", r.to_markdown(80));
    println!("({} ms)", r.elapsed_ms);
    Ok(())
}
