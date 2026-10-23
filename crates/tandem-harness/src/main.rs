use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tandem_core::AgentId;
use tandem_harness::{AcpRunner, AgentRunner, CANARY_SECRET_ENV, CANARY_SECRET_VALUE, MockRunner, RunOptions};

#[derive(Parser)]
#[command(name = "tandem-harness", about = "Agent conformance + benchmark runner")]
struct Cli {
    /// Root containing scenarios/, bench/, fixtures/, reports/.
    #[arg(long, global = true, default_value = "harness")]
    harness_dir: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum RunnerKind {
    Mock,
    Acp,
}

#[derive(clap::Args)]
struct Common {
    /// Comma-separated agent ids, e.g. claude,codex; `auto` = every agent detected
    /// on this machine (installed, or runnable via npx).
    #[arg(long, value_delimiter = ',', required = true)]
    agents: Vec<String>,
    #[arg(long, value_enum, default_value = "mock")]
    runner: RunnerKind,
    /// YAML list of `MockScript`s for `--runner mock`.
    #[arg(long)]
    mock_script: Option<PathBuf>,
    /// Only run these ids (comma-separated).
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
    /// Report directory (default: <harness-dir>/reports/<date>).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Cap every scenario/bench timeout at this many seconds.
    #[arg(long)]
    timeout_s: Option<f64>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run conformance scenarios.
    Conformance {
        #[command(flatten)]
        common: Common,
        #[arg(long)]
        scenario_dir: Option<PathBuf>,
    },
    /// Run benchmark tasks.
    Bench {
        #[command(flatten)]
        common: Common,
        #[arg(long)]
        suite_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        repeats: u32,
    },
    /// Re-render report.md / leaderboard.svg from the JSON in a report dir.
    Report { dir: PathBuf },
    /// Lint every scenario and bench YAML.
    Validate,
}

fn make_runner(c: &Common) -> Result<Arc<dyn AgentRunner>> {
    match c.runner {
        RunnerKind::Acp => Ok(Arc::new(AcpRunner::detect())),
        RunnerKind::Mock => Ok(Arc::new(match &c.mock_script {
            Some(p) => MockRunner::from_yaml(&std::fs::read_to_string(p)?)?,
            None => MockRunner::new(),
        })),
    }
}

fn opts(cli_root: &Path, c: &Common, runner: &str) -> RunOptions {
    let mut o = RunOptions::new(cli_root, runner);
    if let Some(out) = &c.out {
        o.out_dir = out.clone();
    }
    o.max_timeout_s = c.timeout_s;
    o
}

fn agents(c: &Common) -> Vec<AgentId> {
    if c.agents.iter().any(|a| a == "auto") {
        let found = AcpRunner::detect().agents();
        eprintln!("--agents auto: {}", found.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(","));
        return found;
    }
    c.agents.iter().map(|a| AgentId::from(a.as_str())).collect()
}

/// Strips variables from the harness's own env (inherited by every agent) that
/// belong to an enclosing agent session (nested `claude` refuses to start) or
/// look like session tokens; vendor API keys are kept so agents can auth. The env
/// canary is kept on purpose: `env-scrub` checks whether the launcher hides it.
/// Must run before any thread exists.
fn strip_host_session_env() {
    let drop: Vec<_> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| {
            let u = k.to_uppercase();
            k != CANARY_SECRET_ENV
                && (u == "CLAUDECODE"
                    || u.starts_with("CLAUDE_CODE_")
                    || u == "CLAUDE_PID"
                    || ["TOKEN", "SECRET", "PASSWORD", "PRIVATE_KEY"].iter().any(|n| u.contains(n)))
        })
        .collect();
    for k in drop {
        // SAFETY: called from `main` before the runtime or any thread exists.
        unsafe { std::env::remove_var(k) };
    }
}

fn runner_name(c: &Common) -> &'static str {
    match c.runner {
        RunnerKind::Mock => "mock",
        RunnerKind::Acp => "acp",
    }
}

fn finish(report: tandem_harness::Report, dir: &Path) -> Result<()> {
    let merged = report.write(dir)?;
    let failed = report.results.iter().filter(|r| r.status == tandem_harness::Status::Fail).count();
    println!("{}", merged.to_markdown());
    println!("wrote {}", dir.join("report.md").display());
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn main() -> Result<()> {
    // Plant the env canary before any thread exists, so every spawned agent
    // inherits it unless the launcher scrubs its environment.
    // SAFETY: single-threaded at this point (runtime not built yet).
    unsafe { std::env::set_var(CANARY_SECRET_ENV, CANARY_SECRET_VALUE) };
    let cli = Cli::parse();
    if matches!(&cli.cmd, Cmd::Conformance { common, .. } | Cmd::Bench { common, .. } if matches!(common.runner, RunnerKind::Acp)) {
        strip_host_session_env();
    }
    // RUST_LOG=tandem_acp::stderr=debug shows agent stderr (auth errors etc.).
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .with_writer(std::io::stderr)
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let root = cli.harness_dir.clone();
    match cli.cmd {
        Cmd::Validate => {
            let errs = tandem_harness::validate(&root);
            if errs.is_empty() {
                let s = tandem_harness::load_scenarios(&root.join("scenarios"))?.len();
                let b = tandem_harness::load_bench(&root.join("bench"))?.len();
                println!("ok: {s} scenarios, {b} bench tasks");
                Ok(())
            } else {
                for e in &errs {
                    eprintln!("error: {e}");
                }
                bail!("{} problem(s)", errs.len())
            }
        }
        Cmd::Report { dir } => {
            let r = tandem_harness::render_dir(&dir)?;
            println!("{}", r.to_markdown());
            Ok(())
        }
        Cmd::Conformance { common, scenario_dir } => {
            let runner = make_runner(&common)?;
            let dir = scenario_dir.unwrap_or_else(|| root.join("scenarios"));
            let mut scenarios = tandem_harness::load_scenarios(&dir)?;
            if !common.only.is_empty() {
                scenarios.retain(|s| common.only.contains(&s.id));
            }
            let o = opts(&root, &common, runner_name(&common));
            let report = rt.block_on(tandem_harness::conformance(&agents(&common), &scenarios, runner, &o));
            finish(report, &o.out_dir)
        }
        Cmd::Bench { common, suite_dir, repeats } => {
            let runner = make_runner(&common)?;
            let dir = suite_dir.unwrap_or_else(|| root.join("bench"));
            let mut tasks = tandem_harness::load_bench(&dir)?;
            if !common.only.is_empty() {
                tasks.retain(|t| common.only.contains(&t.id));
            }
            let o = opts(&root, &common, runner_name(&common));
            let report = rt.block_on(tandem_harness::bench(&agents(&common), &tasks, runner, repeats, &o));
            finish(report, &o.out_dir)
        }
    }
}
