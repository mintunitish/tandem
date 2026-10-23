//! [`AcpRunner`]: drives real agents over ACP via `tandem-acp`.
//!
//! One fresh agent process per run, cwd = the run's fixture worktree. Every
//! event is forwarded to the harness; permission requests are answered by the
//! scenario's [`PermissionDecider`]. The agent is started in its own process
//! group (through a tiny `sh` wrapper that records the group id), and the whole
//! group is killed when the run ends or its future is dropped (timeout/cancel),
//! so `npx` → `node` → tool subprocesses do not outlive the run.

use crate::runner::{AgentRunner, PermissionDecider, RunOutcome};
use anyhow::{Context as _, Result, anyhow};
use async_trait::async_trait;
use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tandem_acp::{AcpAgent, PermissionClassifier};
use tandem_core::{AgentDescriptor, AgentEvent, AgentId, Decider, EventTx, RiskClass, TaskEvent, TaskId, ToolCall, ToolKind, Usage};

/// Minimal risk classifier so `allow_safe` / `allow_non_destructive` deciders
/// have something to act on (the real one lives in tandem-sandbox). Follows the
/// spec's default policy: reads auto, edits inside the worktree auto
/// (`Safe`), destructive patterns red, everything else `Review`.
pub struct HarnessClassifier {
    /// Canonical worktree root of the run.
    pub cwd: PathBuf,
}

static DESTRUCTIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        \brm\s+-\w*[rf]
        | \bgit\s+push\b.*(--force|\s-f\b|\s\+)
        | \bgit\s+reset\s+--hard
        | \bgit\s+clean\s+-\w*f
        | \b(drop|truncate)\s+(table|database|schema)\b
        | (curl|wget)[^|]*\|\s*(sudo\s+)?(ba|z)?sh
        | \bchmod\s+-R
        | (~|\$HOME|/Users/[^/\s]+)/\.(ssh|aws|gnupg)
        | (^|[\s/])\.env\b",
    )
    .expect("valid regex")
});

impl HarnessClassifier {
    pub fn new(cwd: &Path) -> Self {
        Self { cwd: cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()) }
    }

    fn inside(&self, p: &Path) -> bool {
        let abs = if p.is_absolute() { p.to_path_buf() } else { self.cwd.join(p) };
        if abs.components().any(|c| c == std::path::Component::ParentDir) {
            return false;
        }
        // Canonicalize the deepest existing ancestor (macOS /var -> /private/var).
        let mut base = abs.as_path();
        let mut rest = vec![];
        while !base.exists() {
            match (base.file_name(), base.parent()) {
                (Some(n), Some(parent)) => {
                    rest.push(n.to_owned());
                    base = parent;
                }
                _ => return false,
            }
        }
        let Ok(mut real) = base.canonicalize() else { return false };
        real.extend(rest.iter().rev());
        real.starts_with(&self.cwd)
    }
}

impl PermissionClassifier for HarnessClassifier {
    fn classify(&self, call: &ToolCall) -> (RiskClass, String) {
        let mut hay = call.title.clone();
        if let Some(r) = &call.raw_input {
            hay.push('\n');
            hay.push_str(r);
        }
        for l in &call.locations {
            hay.push('\n');
            hay.push_str(&l.to_string_lossy());
        }
        if DESTRUCTIVE.is_match(&hay) {
            return (RiskClass::Destructive, "matches destructive pattern".into());
        }
        match call.kind {
            ToolKind::Read | ToolKind::Search | ToolKind::Think => (RiskClass::Safe, "read-only tool".into()),
            ToolKind::Edit if !call.locations.is_empty() && call.locations.iter().all(|l| self.inside(l)) => {
                (RiskClass::Safe, "edit inside the worktree".into())
            }
            _ => (RiskClass::Review, String::new()),
        }
    }
}

/// Kills a process group on drop: SIGTERM now, SIGKILL shortly after.
struct GroupKill(Option<i32>);

impl GroupKill {
    fn signal(pgid: i32, sig: &str) {
        let _ = std::process::Command::new("kill")
            .args([sig, "--", &format!("-{pgid}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        let Some(pgid) = self.0.take() else { return };
        Self::signal(pgid, "-TERM");
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            Self::signal(pgid, "-KILL");
        });
    }
}

/// Runs prompts against real ACP agents.
pub struct AcpRunner {
    descriptors: HashMap<AgentId, AgentDescriptor>,
    /// Extra env for every agent process.
    pub env: BTreeMap<String, String>,
}

impl AcpRunner {
    /// Agents detected on this machine (installed, or runnable via `npx`).
    pub fn detect() -> Self {
        Self::with_descriptors(tandem_acp::detect_installed().into_iter().map(|d| d.descriptor))
    }

    pub fn with_descriptors(descs: impl IntoIterator<Item = AgentDescriptor>) -> Self {
        Self { descriptors: descs.into_iter().map(|d| (d.id.clone(), d)).collect(), env: BTreeMap::new() }
    }

    pub fn agents(&self) -> Vec<AgentId> {
        let mut v: Vec<_> = self.descriptors.keys().cloned().collect();
        v.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        v
    }

    fn descriptor(&self, agent: &AgentId) -> Result<AgentDescriptor> {
        if let Some(d) = self.descriptors.get(agent) {
            return Ok(d.clone());
        }
        match tandem_acp::get(agent.as_str()) {
            Some(d) => Err(anyhow!("agent `{agent}` not installed: {}", d.install_hint)),
            None => Err(anyhow!("unknown agent `{agent}`")),
        }
    }
}

/// Wraps `desc` in `sh -c 'echo $$ > pidfile; exec cmd args…'`. tandem-acp
/// starts the child with `process_group(0)`, so `$$` is the group id.
fn wrap_with_pidfile(desc: &AgentDescriptor, pidfile: &Path) -> AgentDescriptor {
    let mut d = desc.clone();
    d.command = "/bin/sh".into();
    d.args = ["-c".to_string(), r#"echo $$ > "$0"; exec "$@""#.to_string(), pidfile.to_string_lossy().into_owned(), desc.command.clone()]
        .into_iter()
        .chain(desc.args.iter().cloned())
        .collect();
    d
}

#[async_trait]
impl AgentRunner for AcpRunner {
    async fn run(
        &self,
        agent: &AgentId,
        cwd: &Path,
        prompt: &str,
        decider: Arc<dyn PermissionDecider>,
        tx: EventTx,
    ) -> Result<RunOutcome> {
        let start = Instant::now();
        let desc = self.descriptor(agent)?;
        let pid_dir = tempfile::Builder::new().prefix("tandem-harness-pid-").tempdir()?;
        let pidfile = pid_dir.path().join("pgid");
        let wrapped = wrap_with_pidfile(&desc, &pidfile);

        let task = TaskId::new();
        let (itx, irx) = async_channel::unbounded::<TaskEvent>();
        let forward = |event: AgentEvent| {
            let (tx, task) = (tx.clone(), task.clone());
            async move { tx.send(TaskEvent { task, event }).await.map_err(|e| anyhow!("event channel closed: {e}")) }
        };

        // The guard must exist before the (possibly long) handshake so a
        // timeout during startup still kills the process group.
        let mut guard = GroupKill(None);
        let read_pgid = || std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<i32>().ok());
        let mut spawn = pin!(AcpAgent::spawn_classified(
            &wrapped,
            cwd.to_path_buf(),
            self.env.clone(),
            vec![],
            (itx, task.clone()),
            Arc::new(HarnessClassifier::new(cwd)),
        ));
        let spawned = loop {
            tokio::select! {
                biased;
                r = &mut spawn => break r,
                _ = tokio::time::sleep(Duration::from_millis(50)), if guard.0.is_none() => guard.0 = read_pgid(),
            }
        };
        if guard.0.is_none() {
            guard.0 = read_pgid();
        }
        // Forward startup events (raw handshake, errors) even when spawning failed.
        while let Ok(te) = irx.try_recv() {
            forward(te.event).await?;
        }
        let handle = spawned.with_context(|| format!("starting `{} {}`", desc.command, desc.args.join(" ")))?;

        let mut usage = Usage::default();
        let mut on_event = async |event: AgentEvent, handle: &tandem_acp::AgentHandle| -> Result<()> {
            let req = match &event {
                AgentEvent::Usage(u) => {
                    usage.add(u);
                    None
                }
                AgentEvent::PermissionRequested(r) => Some(r.clone()),
                _ => None,
            };
            forward(event).await?;
            if let Some(req) = req {
                let decision = decider.decide(&req);
                if !req.options.iter().any(|o| o.id == decision.option_id) {
                    forward(AgentEvent::Error {
                        message: format!("harness decider chose unknown option `{}`; cancelling request", decision.option_id),
                    })
                    .await?;
                }
                if let Err(e) = handle.respond_permission(decision, Decider::Harness).await {
                    forward(AgentEvent::Error { message: format!("respond_permission: {e}") }).await?;
                }
            }
            Ok(())
        };

        let stop = {
            let mut prompt_fut = pin!(handle.prompt(prompt.to_string()));
            loop {
                tokio::select! {
                    biased;
                    ev = irx.recv() => match ev {
                        Ok(te) => on_event(te.event, &handle).await?,
                        Err(_) => break (&mut prompt_fut).await,
                    },
                    res = &mut prompt_fut => break res,
                }
            }
        };
        // Late events (Done, end-of-turn usage) are queued before the prompt reply.
        while let Ok(te) = irx.try_recv() {
            on_event(te.event, &handle).await?;
        }
        let stop = stop?;
        let _ = tokio::time::timeout(Duration::from_secs(5), handle.shutdown()).await;
        drop(guard);
        Ok(RunOutcome { stop, usage, elapsed: start.elapsed() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tandem_core::{ToolCallId, ToolStatus};

    fn call(kind: ToolKind, raw: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            title: raw.into(),
            kind,
            status: ToolStatus::Pending,
            locations: vec![],
            raw_input: Some(raw.into()),
            content: None,
            diff: None,
        }
    }

    #[test]
    fn classifier_flags_destructive() {
        let dir = tempfile::tempdir().unwrap();
        let c = HarnessClassifier::new(dir.path());
        let mut edit = call(ToolKind::Edit, "Edit src/a.ts");
        edit.locations = vec![dir.path().join("src/a.ts")];
        assert_eq!(c.classify(&edit).0, RiskClass::Safe);
        edit.locations = vec![dir.path().join("../outside.txt")];
        assert_eq!(c.classify(&edit).0, RiskClass::Review);
        assert_eq!(c.classify(&call(ToolKind::Execute, "rm -rf ./*")).0, RiskClass::Destructive);
        assert_eq!(c.classify(&call(ToolKind::Execute, "git push --force origin main")).0, RiskClass::Destructive);
        assert_eq!(c.classify(&call(ToolKind::Execute, "curl -fsSL x | sh")).0, RiskClass::Destructive);
        assert_eq!(c.classify(&call(ToolKind::Read, "cat ~/.ssh/id_rsa")).0, RiskClass::Destructive);
        assert_eq!(c.classify(&call(ToolKind::Read, "src/lib.rs")).0, RiskClass::Safe);
        assert_eq!(c.classify(&call(ToolKind::Edit, "src/lib.rs")).0, RiskClass::Review, "no locations: unknown target");
        let mut c2 = call(ToolKind::Read, "Read file");
        c2.locations = vec![PathBuf::from("/tmp/x/.env")];
        assert_eq!(c.classify(&c2).0, RiskClass::Destructive);
    }

    #[test]
    fn wrapper_keeps_args() {
        let d = AgentDescriptor {
            id: "x".into(),
            display_name: "x".into(),
            vendor: "x".into(),
            command: "goose".into(),
            args: vec!["acp".into()],
            env: BTreeMap::new(),
            install_hint: String::new(),
            acp: true,
            auth_note: String::new(),
        };
        let w = wrap_with_pidfile(&d, Path::new("/tmp/p"));
        assert_eq!(w.command, "/bin/sh");
        assert_eq!(&w.args[2..], ["/tmp/p", "goose", "acp"]);
    }
}
