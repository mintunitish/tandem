//! Executes scenarios and bench tasks: fresh fixture copy per run, drive the
//! runner, record events, evaluate.

use crate::eval::{self, CheckResult, EvalCtx};
use crate::report::{Report, RunKind, RunResult, Status};
use crate::runner::{AgentRunner, PermissionDecider};
use crate::scenario::{BenchTask, Category, Fixture, Scenario};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tandem_core::{AgentEvent, AgentId, LogRecord, StopReason, TaskId, Usage};

/// Value planted in the harness env as `TANDEM_CANARY_SECRET` by the CLI; the
/// `env-scrub` scenario asserts it never shows up in agent output.
pub const CANARY_SECRET_ENV: &str = "TANDEM_CANARY_SECRET";
pub const CANARY_SECRET_VALUE: &str = "tandem-canary-5f2c9e71";

#[derive(Clone, Debug)]
pub struct RunOptions {
    /// `harness/fixtures`.
    pub fixtures_dir: PathBuf,
    /// `harness/reports/<date>`; logs land in `<agent>/<id>/log.jsonl`.
    pub out_dir: PathBuf,
    pub date: String,
    /// Label stored in the report (`mock`, `acp`).
    pub runner_name: String,
    /// Upper bound on any scenario/bench timeout (not on `cancel_after_s`).
    pub max_timeout_s: Option<f64>,
    /// Record every run as [`Status::Blocked`] with this reason instead of running it.
    pub blocked: Option<String>,
    /// Upsert each result into `out_dir` as soon as it finishes, so an
    /// interrupted run keeps what it already measured.
    pub persist: bool,
}

impl RunOptions {
    fn cap(&self, t: f64) -> f64 {
        self.max_timeout_s.map_or(t, |m| t.min(m))
    }
}

impl RunOptions {
    pub fn new(harness_root: &Path, runner_name: &str) -> Self {
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        Self {
            fixtures_dir: harness_root.join("fixtures"),
            out_dir: harness_root.join("reports").join(&date),
            date,
            runner_name: runner_name.into(),
            max_timeout_s: None,
            blocked: None,
            persist: true,
        }
    }
}

/// A fresh fixture copy: `<tmp>/work` is the agent cwd, git-initialised and committed.
pub struct Workspace {
    _tmp: tempfile::TempDir,
    pub work: PathBuf,
    pub base_commit: String,
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

fn materialize(fixture: &Fixture, fixtures_dir: &Path, dst: &Path) -> Result<()> {
    match fixture {
        Fixture::Dir(d) => copy_dir(&fixtures_dir.join(d), dst),
        Fixture::Inline { files } => {
            for (p, content) in files {
                let to = dst.join(p);
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(to, content)?;
            }
            Ok(())
        }
    }
}

fn sh(cwd: &Path, root: &Path, cmd: &str) -> Result<()> {
    let out = std::process::Command::new("sh").arg("-c").arg(cmd).current_dir(cwd).env("HARNESS_ROOT", root).output()?;
    if !out.status.success() {
        bail!("`{cmd}` failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

impl Workspace {
    pub fn create(fixture: &Fixture, fixtures_dir: &Path, setup: &[String]) -> Result<Self> {
        let tmp = tempfile::Builder::new().prefix("tandem-harness-").tempdir()?;
        let root = tmp.path().canonicalize()?;
        let work = root.join("work");
        std::fs::create_dir_all(&work)?;
        materialize(fixture, fixtures_dir, &work)?;
        let git = "git -c user.name=tandem-harness -c user.email=harness@tandem.invalid -c commit.gpgsign=false";
        sh(&work, &root, &format!("git init -q -b main && git add -A && {git} commit -q --allow-empty -m fixture"))?;
        for cmd in setup {
            sh(&work, &root, cmd).with_context(|| "scenario setup")?;
        }
        let out = std::process::Command::new("git").arg("-C").arg(&work).args(["rev-parse", "HEAD"]).output()?;
        let base_commit = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(Self { _tmp: tmp, work, base_commit })
    }
    pub fn root(&self) -> &Path {
        self.work.parent().unwrap_or(&self.work)
    }
}

/// What came back from one runner invocation.
pub struct Driven {
    pub events: Vec<AgentEvent>,
    pub stop: Option<StopReason>,
    pub usage: Usage,
    pub elapsed: Duration,
    pub error: Option<String>,
    pub timed_out: bool,
}

/// Runs the agent, records events to `log` (JSONL of `LogRecord`), enforces
/// `deadline`. When `cancel` is true, hitting the deadline is a deliberate
/// cancel (stop = `cancelled`) rather than a timeout.
#[allow(clippy::too_many_arguments)]
pub async fn drive(
    runner: &dyn AgentRunner,
    agent: &AgentId,
    cwd: &Path,
    prompt: &str,
    decider: Arc<dyn PermissionDecider>,
    deadline: Duration,
    cancel: bool,
    log: &Path,
) -> Result<Driven> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(log)?);
    let task = TaskId::new();
    let (tx, rx) = async_channel::unbounded();
    let start = Instant::now();
    let mut events = Vec::new();
    let mut record = |event: AgentEvent, file: &mut std::io::BufWriter<std::fs::File>| -> Result<()> {
        let rec = LogRecord { at: Utc::now(), task: task.clone(), event };
        serde_json::to_writer(&mut *file, &rec)?;
        file.write_all(b"\n")?;
        events.push(rec.event);
        Ok(())
    };

    let result = {
        let mut fut = pin!(tokio::time::timeout(deadline, runner.run(agent, cwd, prompt, decider, tx)));
        let mut open = true;
        loop {
            tokio::select! {
                biased;
                ev = rx.recv(), if open => match ev {
                    Ok(te) => record(te.event, &mut file)?,
                    Err(_) => open = false,
                },
                res = &mut fut => break res,
            }
        }
    };
    let elapsed = start.elapsed();
    while let Ok(te) = rx.try_recv() {
        record(te.event, &mut file)?;
    }
    file.flush()?;

    let mut usage = Usage::default();
    let mut saw_usage = false;
    let mut done = None;
    for e in &events {
        match e {
            AgentEvent::Usage(u) => {
                usage.add(u);
                saw_usage = true;
            }
            AgentEvent::Done { reason } => done = Some(*reason),
            _ => {}
        }
    }
    let mut d = Driven { events, stop: done, usage, elapsed, error: None, timed_out: false };
    match result {
        Err(_elapsed) => {
            if cancel {
                d.stop = Some(StopReason::Cancelled);
            } else {
                d.timed_out = true;
            }
        }
        Ok(Err(e)) => d.error = Some(format!("{e:#}")),
        Ok(Ok(outcome)) => {
            d.stop = Some(outcome.stop);
            if !saw_usage {
                d.usage = outcome.usage;
            }
        }
    }
    Ok(d)
}

fn log_path(opts: &RunOptions, agent: &AgentId, id: &str, repeat: u32) -> PathBuf {
    let name = if repeat == 0 { "log.jsonl".to_string() } else { format!("log.{repeat}.jsonl") };
    opts.out_dir.join(agent.as_str()).join(id).join(name)
}

fn rel(opts: &RunOptions, p: &Path) -> String {
    p.strip_prefix(&opts.out_dir).unwrap_or(p).to_string_lossy().to_string()
}

fn base_result(kind: RunKind, agent: &AgentId, id: &str, title: &str, category: Option<String>, repeat: u32) -> RunResult {
    RunResult {
        kind,
        agent: agent.clone(),
        id: id.into(),
        title: title.into(),
        category,
        repeat,
        status: Status::Skip,
        reason: String::new(),
        checks: vec![],
        elapsed_s: 0.0,
        usage: Usage::default(),
        stop: None,
        log: None,
    }
}

pub async fn run_scenario(agent: &AgentId, s: &Scenario, runner: &dyn AgentRunner, opts: &RunOptions) -> RunResult {
    let mut r = base_result(RunKind::Conformance, agent, &s.id, &s.title, Some(s.category.as_str().into()), 0);
    if let Some(reason) = s.requires.missing() {
        r.reason = reason;
        return r;
    }
    if let Err(e) = scenario_inner(agent, s, runner, opts, &mut r).await {
        r.status = Status::Fail;
        r.reason = format!("harness error: {e:#}");
    }
    r
}

async fn scenario_inner(agent: &AgentId, s: &Scenario, runner: &dyn AgentRunner, opts: &RunOptions, r: &mut RunResult) -> Result<()> {
    let ws = Workspace::create(&s.fixture, &opts.fixtures_dir, &s.setup)?;
    let decider = s.decider.build()?;
    let (deadline, cancel) = match s.cancel_after_s {
        Some(c) => (c, true),
        None => (opts.cap(s.timeout_s.unwrap_or(300.0)), false),
    };
    let log = log_path(opts, agent, &s.id, 0);
    let d = drive(runner, agent, &ws.work, &s.prompt, decider, Duration::from_secs_f64(deadline), cancel, &log).await?;
    if let Some(settle) = s.settle_s {
        tokio::time::sleep(Duration::from_secs_f64(settle)).await;
    }
    r.log = Some(rel(opts, &log));
    r.elapsed_s = d.elapsed.as_secs_f64();
    r.usage = d.usage.clone();
    r.stop = d.stop;
    let ctx = EvalCtx { work: &ws.work, base_commit: &ws.base_commit, events: &d.events, stop: d.stop, elapsed: d.elapsed };
    r.checks = s.expectations.iter().map(|c| eval::evaluate(c, &ctx)).collect::<Vec<CheckResult>>();
    let first_fail = r.checks.iter().find(|c| !c.passed);
    (r.status, r.reason) = if let Some(e) = &d.error {
        (Status::Fail, format!("runner error: {e}"))
    } else if d.timed_out {
        (Status::Fail, format!("timed out after {deadline}s"))
    } else if let Some(c) = first_fail {
        (Status::Fail, c.detail.clone())
    } else {
        (Status::Pass, String::new())
    };
    Ok(())
}

pub async fn run_bench_task(
    agent: &AgentId,
    t: &BenchTask,
    runner: &dyn AgentRunner,
    repeat: u32,
    opts: &RunOptions,
) -> RunResult {
    let mut r = base_result(RunKind::Bench, agent, &t.id, &t.title, None, repeat);
    if let Err(e) = bench_inner(agent, t, runner, repeat, opts, &mut r).await {
        r.status = Status::Fail;
        r.reason = format!("harness error: {e:#}");
    }
    r
}

async fn bench_inner(agent: &AgentId, t: &BenchTask, runner: &dyn AgentRunner, repeat: u32, opts: &RunOptions, r: &mut RunResult) -> Result<()> {
    let ws = Workspace::create(&t.fixture, &opts.fixtures_dir, &[])?;
    let decider = t.decider.build()?;
    let log = log_path(opts, agent, &t.id, repeat);
    let timeout_s = opts.cap(t.timeout_s);
    let deadline = Duration::from_secs_f64(timeout_s);
    let d = drive(runner, agent, &ws.work, &t.prompt, decider, deadline, false, &log).await?;
    r.log = Some(rel(opts, &log));
    r.elapsed_s = d.elapsed.as_secs_f64();
    r.usage = d.usage.clone();
    r.stop = d.stop;
    if let Some(e) = d.error {
        r.status = Status::Fail;
        r.reason = format!("runner error: {e}");
        return Ok(());
    }
    if d.timed_out {
        r.status = Status::Fail;
        r.reason = format!("timed out after {timeout_s}s");
        return Ok(());
    }
    if let Some(h) = &t.hidden {
        copy_dir(&opts.fixtures_dir.join(h), &ws.work)?;
    }
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(t.verify.shell())
        .current_dir(&ws.work)
        .env("HARNESS_ROOT", ws.root())
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let verify_timeout = Duration::from_secs_f64(t.timeout_s.max(30.0));
    let (passed, detail) = match tokio::time::timeout(verify_timeout, child.wait_with_output()).await {
        Err(_) => (false, "verify timed out".to_string()),
        Ok(Err(e)) => (false, e.to_string()),
        Ok(Ok(o)) => {
            let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            (o.status.success(), eval::tail(&text, 300))
        }
    };
    r.checks = vec![CheckResult { check: format!("verify: {}", t.verify.shell().lines().next().unwrap_or("")), passed, detail: detail.clone() }];
    (r.status, r.reason) = if passed { (Status::Pass, String::new()) } else { (Status::Fail, format!("verify failed: {detail}")) };
    Ok(())
}

/// Short reason for skipping the rest of an agent's scenarios after a failed preflight.
fn preflight_reason(id: &str, why: &str) -> String {
    let lower = why.to_lowercase();
    let auth = ["auth", "login", "log in", "unauthorized", "401", "403", "credential", "api key", "api_key"]
        .iter()
        .any(|n| lower.contains(n));
    // e.g. goose configured for a local Ollama that is not running.
    let unreachable = ["localhost:", "127.0.0.1:", "could not connect", "connection refused"].iter().any(|n| lower.contains(n));
    let tag = if auth {
        "auth required"
    } else if unreachable {
        "model provider unreachable"
    } else {
        "preflight failed"
    };
    format!("{tag} (`{id}`: {})", crate::eval::tail(why, 160))
}

fn persist(opts: &RunOptions, r: &RunResult) {
    if !opts.persist {
        return;
    }
    if let Err(e) = Report::new(&opts.date, &opts.runner_name, vec![r.clone()]).write(&opts.out_dir) {
        eprintln!("warning: could not persist {}/{}: {e:#}", r.agent, r.id);
    }
}

fn blocked_result(opts: &RunOptions, mut r: RunResult) -> Option<RunResult> {
    let why = opts.blocked.as_ref()?;
    r.status = Status::Blocked;
    r.reason = format!("blocked: {why}");
    Some(r)
}

/// Runs every scenario against every agent, sequentially. `preflight`
/// scenarios run first; if one fails, the agent's remaining scenarios are skipped.
pub async fn conformance(agents: &[AgentId], scenarios: &[Scenario], runner: Arc<dyn AgentRunner>, opts: &RunOptions) -> Report {
    let mut ordered: Vec<&Scenario> = scenarios.iter().collect();
    ordered.sort_by_key(|s| s.category != Category::Preflight);
    let mut results = Vec::new();
    for agent in agents {
        let mut blocked: Option<String> = None;
        for s in &ordered {
            let base = base_result(RunKind::Conformance, agent, &s.id, &s.title, Some(s.category.as_str().into()), 0);
            if let Some(r) = blocked_result(opts, base.clone()) {
                persist(opts, &r);
                results.push(r);
                continue;
            }
            if let Some(why) = &blocked {
                let mut r = base;
                r.reason = why.clone();
                persist(opts, &r);
                results.push(r);
                continue;
            }
            tracing::info!(agent = %agent, scenario = %s.id, "conformance");
            eprintln!("[conformance] {agent} / {} ...", s.id);
            let r = run_scenario(agent, s, runner.as_ref(), opts).await;
            eprintln!("[conformance] {agent} / {} -> {:?} {}", s.id, r.status, crate::eval::tail(&r.reason, 200));
            if s.category == Category::Preflight && r.status == Status::Fail {
                blocked = Some(preflight_reason(&s.id, &r.reason));
            }
            persist(opts, &r);
            results.push(r);
        }
    }
    Report::new(&opts.date, &opts.runner_name, results)
}

/// Runs every task `repeats` times against every agent, sequentially.
pub async fn bench(agents: &[AgentId], tasks: &[BenchTask], runner: Arc<dyn AgentRunner>, repeats: u32, opts: &RunOptions) -> Report {
    let mut results = Vec::new();
    for agent in agents {
        for t in tasks {
            for rep in 0..repeats.max(1) {
                if let Some(r) = blocked_result(opts, base_result(RunKind::Bench, agent, &t.id, &t.title, None, rep)) {
                    persist(opts, &r);
                    results.push(r);
                    continue;
                }
                tracing::info!(agent = %agent, task = %t.id, rep, "bench");
                eprintln!("[bench] {agent} / {} #{rep} ...", t.id);
                let r = run_bench_task(agent, t, runner.as_ref(), rep, opts).await;
                eprintln!("[bench] {agent} / {} #{rep} -> {:?} {}", t.id, r.status, crate::eval::tail(&r.reason, 200));
                persist(opts, &r);
                results.push(r);
            }
        }
    }
    Report::new(&opts.date, &opts.runner_name, results)
}
