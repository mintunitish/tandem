//! ACP client: spawns an agent process, runs one session, streams [`AgentEvent`]s.
//!
//! The SDK connection runs on a dedicated thread with its own tokio current-thread
//! runtime, so [`AgentHandle`] works from any executor (tokio, gpui, ...).

use crate::map::{self, UpdateMapper};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1 as acp;
use agent_client_protocol::{Agent, Client, ConnectionTo, Lines, Responder};
use anyhow::{Context as _, Result, anyhow};
use futures::channel::oneshot;
use futures::{FutureExt as _, StreamExt as _};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;
use crate::launch::{self, SpawnOptions};
use tandem_core::{
    AgentDescriptor, AgentEvent, Decider, Direction, EventTx, McpServerSpec, PermissionClassifier,
    PermissionDecision, PermissionOption, PermissionOptionKind, PermissionRequest, RequestId,
    RiskClass, Role, SessionId, StopReason, TaskEvent, TaskId,
};
use tandem_sandbox::kill::{self, KillSwitch};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

/// Grace period between SIGTERM and SIGKILL when stopping an agent's process groups.
pub const AGENT_GRACE: Duration = Duration::from_secs(2);

/// Classifier that marks everything `Review` (tests, demos).
pub struct DefaultClassifier;
impl PermissionClassifier for DefaultClassifier {}

type Pending = Arc<Mutex<HashMap<RequestId, oneshot::Sender<String>>>>;

/// `PermissionResolved::option` for a request answered `cancelled` (no matching option).
pub const CANCELLED: &str = "cancelled";

enum Cmd {
    Prompt(String, oneshot::Sender<Result<StopReason>>),
    Cancel,
    Shutdown,
}

/// Cheap clonable event emitter for one task.
#[derive(Clone)]
struct Emit {
    tx: EventTx,
    task: TaskId,
}
impl Emit {
    async fn send(&self, event: AgentEvent) {
        // A closed receiver means nobody listens anymore; nothing useful to do.
        let _ = self
            .tx
            .send(TaskEvent {
                task: self.task.clone(),
                event,
            })
            .await;
    }
}

/// Entry point for spawning agents.
pub struct AcpAgent;

impl AcpAgent {
    /// Spawn `desc` per `opts`, run `initialize` + `session/new`, return a live handle.
    ///
    /// Enforced launch: env scrubbed ([`crate::launch::child_env`]), optionally inside
    /// Seatbelt, own process group registered with the global Kill Switch, and
    /// permission requests classified by `opts.classifier`. With
    /// `policy.auto_allow_safe`, `Safe` requests are answered by policy; `Review` and
    /// `Destructive` always go to the UI / decider. Dropping the returned future before it
    /// resolves (timeout) kills the agent's process group.
    pub async fn spawn(
        desc: &AgentDescriptor,
        opts: SpawnOptions,
        tx: EventTx,
        task: TaskId,
    ) -> Result<AgentHandle> {
        if KillSwitch::global().is_tripped() {
            return Err(anyhow!(
                "STOP is active; call tandem_acp::reset_stop() before starting agents"
            ));
        }
        // Defense in depth: callers sanitize custom descriptors when loading them; a
        // descriptor that is not a built-in never launches with unvalidated capabilities.
        let sanitized;
        let desc = if desc.trusted {
            desc
        } else {
            sanitized = tandem_sandbox::descriptor::sanitize(desc.clone(), tandem_sandbox::descriptor::Trust::User);
            &sanitized
        };
        let emit = Emit { tx, task };
        let cwd = opts
            .cwd
            .canonicalize()
            .with_context(|| format!("cwd {}", opts.cwd.display()))?;
        let opts = SpawnOptions { cwd, ..opts };
        let parent: HashMap<String, String> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        let env = launch::child_env(&parent, &opts.policy, desc, opts.strict_permissions, &opts.env_overrides);
        let (program, args) = launch::launch_command(desc, &opts, &env);
        let mut command = tokio::process::Command::new(&program);
        command
            .args(&args)
            .env_clear()
            .envs(&env)
            .current_dir(&opts.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        kill::configure(command.as_std_mut());

        let (cmd_tx, cmd_rx) = async_channel::unbounded();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let pending: Pending = Arc::default();
        // Exists before the handshake so a dropped spawn future still kills the group.
        let group = GroupGuard::default();
        let ctx = Session {
            pgid: group.0.clone(),
            agent: desc.id.clone(),
            cwd: opts.cwd,
            mcp_servers: opts.mcp_servers,
            emit: emit.clone(),
            pending: pending.clone(),
            classifier: opts.classifier,
            auto_allow_safe: opts.policy.auto_allow_safe,
        };
        std::thread::Builder::new()
            .name(format!("acp-{}", desc.id))
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(ctx.run(command, cmd_rx, ready_tx)),
                    Err(e) => {
                        let _ = ready_tx.send(Err(anyhow!("runtime: {e}")));
                    }
                }
                let _ = done_tx.send(());
            })
            .context("spawn acp thread")?;

        let session = ready_rx
            .await
            .map_err(|_| anyhow!("agent thread exited during startup"))??;
        Ok(AgentHandle {
            cmd_tx,
            session,
            pending,
            emit,
            done: done_rx,
            group,
        })
    }
}

/// SIGTERMs (then SIGKILLs) the agent's process group on drop, on the shared reaper
/// thread. The session resets the PGID to 0 once it has reaped the leader, so a finished
/// agent's guard is a no-op and a recycled PGID is never signalled.
#[derive(Default)]
struct GroupGuard(Arc<AtomicI32>);

impl GroupGuard {
    fn pgid(&self) -> Option<i32> {
        Some(self.0.load(Ordering::SeqCst)).filter(|p| *p > 1)
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pgid) = self.pgid() {
            kill::stop_group_in_background(pgid, AGENT_GRACE);
        }
    }
}

/// Live agent session. Dropping it kills the agent's whole process group.
pub struct AgentHandle {
    cmd_tx: async_channel::Sender<Cmd>,
    session: SessionId,
    pending: Pending,
    emit: Emit,
    done: oneshot::Receiver<()>,
    group: GroupGuard,
}

impl AgentHandle {
    /// Process group of the agent (== its pid), once started.
    pub fn pgid(&self) -> Option<i32> {
        self.group.pgid()
    }

    /// Stop the agent's process group and the groups its tools moved into: SIGTERM now,
    /// SIGKILL survivors after `grace`. Blocks for at most `grace`.
    pub fn kill_group(&self, grace: Duration) {
        if let Some(pgid) = self.pgid() {
            kill::stop_group(pgid, grace);
        }
    }

    /// Send a prompt; resolves when the turn ends. The prompt itself is emitted first as
    /// `TextDelta { role: User }`; the turn's events stream on the task channel meanwhile.
    pub async fn prompt(&self, text: String) -> Result<StopReason> {
        self.emit.send(AgentEvent::TextDelta { role: Role::User, text: text.clone() }).await;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Prompt(text, tx))
            .await
            .map_err(|_| anyhow!("agent connection closed"))?;
        rx.await.map_err(|_| anyhow!("agent connection closed"))?
    }

    /// Send `session/cancel`; pending permission requests resolve as cancelled.
    pub async fn cancel(&self) {
        let _ = self.cmd_tx.send(Cmd::Cancel).await;
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session
    }

    /// True while `request` awaits [`AgentHandle::respond_permission`]. False for
    /// requests already answered by policy (`Safe` + `auto_allow_safe`).
    pub fn is_pending(&self, request: &RequestId) -> bool {
        self.pending.lock().contains_key(request)
    }

    /// Answer a `PermissionRequested` event.
    pub async fn respond_permission(
        &self,
        decision: PermissionDecision,
        by: Decider,
    ) -> Result<()> {
        let sender = self
            .pending
            .lock()
            .remove(&decision.request)
            .ok_or_else(|| anyhow!("unknown permission request {}", decision.request))?;
        // Emit first so the resolution precedes any follow-up update from the agent.
        self.emit
            .send(AgentEvent::PermissionResolved {
                request: decision.request,
                option: decision.option_id.clone(),
                by,
            })
            .await;
        sender
            .send(decision.option_id)
            .map_err(|_| anyhow!("permission request no longer pending"))
    }

    /// Answer `req` with its option of `kind` (or a narrower same-polarity fallback, see
    /// [`PermissionRequest::option_for`]). With no such option the request is cancelled:
    /// `PermissionResolved { option: "cancelled" }` is emitted so the UI clears it, then
    /// the agent is answered `cancelled`.
    pub async fn respond_choice(&self, req: &PermissionRequest, kind: PermissionOptionKind, by: Decider) -> Result<()> {
        match req.option_for(kind) {
            Some(o) => self.respond_permission(PermissionDecision { request: req.id.clone(), option_id: o.id.clone() }, by).await,
            None => {
                let sender = self.pending.lock().remove(&req.id);
                if sender.is_some() {
                    self.emit
                        .send(AgentEvent::PermissionResolved { request: req.id.clone(), option: CANCELLED.into(), by })
                        .await;
                }
                // Dropping the sender answers the agent with `cancelled`.
                drop(sender);
                Err(anyhow!("request {} has no {kind:?} option; cancelled", req.id))
            }
        }
    }

    /// Close the connection and wait for the agent process to be gone.
    pub async fn shutdown(self) {
        let _ = self.cmd_tx.send(Cmd::Shutdown).await;
        let _ = self.done.await;
    }
}

struct Session {
    pgid: Arc<AtomicI32>,
    agent: tandem_core::AgentId,
    cwd: PathBuf,
    mcp_servers: Vec<McpServerSpec>,
    emit: Emit,
    pending: Pending,
    classifier: Arc<dyn PermissionClassifier>,
    auto_allow_safe: bool,
}

impl Session {
    async fn run(
        self,
        mut command: tokio::process::Command,
        cmd_rx: async_channel::Receiver<Cmd>,
        ready: oneshot::Sender<Result<SessionId>>,
    ) {
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = ready.send(Err(anyhow!(
                    "failed to start {:?}: {e}",
                    command.as_std().get_program()
                )));
                return;
            }
        };
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = ready.send(Err(anyhow!("agent stdio not piped")));
            return;
        };
        let pid = child.id();
        if let Some(pid) = pid {
            self.pgid.store(pid as i32, Ordering::SeqCst);
            KillSwitch::global().register(pid, format!("acp:{}", self.agent));
        }
        let agent = self.agent.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "tandem_acp::stderr", agent = %agent, "{line}");
            }
        });

        let transport = tee_transport(stdin, stdout, self.emit.clone());
        let ready = Arc::new(Mutex::new(Some(ready)));
        let result = self.connect(transport, cmd_rx, ready.clone()).await;
        if let Err(e) = &result {
            tracing::warn!(agent = %self.agent, "acp connection ended: {e}");
            let msg = e.to_string();
            let ready = ready.lock().take();
            match ready {
                Some(r) => {
                    let _ = r.send(Err(anyhow!(msg)));
                }
                None => self.emit.send(AgentEvent::Error { message: msg }).await,
            }
        }
        self.pending.lock().clear();
        // Stop the whole group (and tool groups) while the unreaped leader still pins the
        // PGID, then reap the leader and forget the PGID so nothing signals it again.
        if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
            let _ = tokio::task::spawn_blocking(move || kill::stop_group(pid, AGENT_GRACE)).await;
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
        self.pgid.store(0, Ordering::SeqCst);
        if let Some(pid) = pid {
            KillSwitch::global().unregister(pid);
        }
    }

    async fn connect(
        &self,
        transport: impl agent_client_protocol::ConnectTo<Client> + 'static,
        cmd_rx: async_channel::Receiver<Cmd>,
        ready: Arc<Mutex<Option<oneshot::Sender<Result<SessionId>>>>>,
    ) -> Result<()> {
        let mapper = Arc::new(Mutex::new(UpdateMapper::default()));
        let (emit_n, mapper_n) = (self.emit.clone(), mapper.clone());
        let (emit_p, mapper_p, pending, classifier, auto_safe) = (
            self.emit.clone(),
            mapper,
            self.pending.clone(),
            self.classifier.clone(),
            self.auto_allow_safe,
        );
        let (cwd_r, cwd_w) = (self.cwd.clone(), self.cwd.clone());

        Client
            .builder()
            .name("tandem")
            .on_receive_notification(
                async move |n: acp::SessionNotification, _cx| {
                    let event = mapper_n.lock().map(&n.update);
                    if let Some(event) = event {
                        emit_n.send(event).await;
                    }
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |req: acp::RequestPermissionRequest,
                            responder: Responder<acp::RequestPermissionResponse>,
                            cx: ConnectionTo<Agent>| {
                    let call = mapper_p.lock().permission_call(&req.tool_call);
                    let (risk, reason) = classifier.classify(&call);
                    let id = RequestId::new();
                    let options: Vec<PermissionOption> = req
                        .options
                        .iter()
                        .map(|o| PermissionOption {
                            id: o.option_id.to_string(),
                            label: o.name.clone(),
                            kind: map::option_kind(&o.kind),
                        })
                        .collect();
                    // Only `Safe` is ever answered by policy; `Destructive` always waits
                    // for a human (or the harness decider), whatever the policy says.
                    let auto = (auto_safe && risk == RiskClass::Safe)
                        .then(|| {
                            options
                                .iter()
                                .find(|o| o.kind == PermissionOptionKind::AllowOnce)
                                .map(|o| o.id.clone())
                        })
                        .flatten();
                    let (tx, rx) = oneshot::channel();
                    if auto.is_none() {
                        pending.lock().insert(id.clone(), tx);
                    }
                    emit_p
                        .send(AgentEvent::PermissionRequested(PermissionRequest {
                            id: id.clone(),
                            tool_call: call,
                            options,
                            risk,
                            reason,
                        }))
                        .await;
                    if let Some(option) = auto {
                        emit_p
                            .send(AgentEvent::PermissionResolved {
                                request: id,
                                option: option.clone(),
                                by: Decider::Policy,
                            })
                            .await;
                        return responder.respond(acp::RequestPermissionResponse::new(
                            acp::RequestPermissionOutcome::Selected(
                                acp::SelectedPermissionOutcome::new(option),
                            ),
                        ));
                    }
                    // Wait off the dispatch loop so other messages keep flowing.
                    cx.spawn(async move {
                        let outcome = match rx.await {
                            Ok(option) => acp::RequestPermissionOutcome::Selected(
                                acp::SelectedPermissionOutcome::new(option),
                            ),
                            Err(_) => acp::RequestPermissionOutcome::Cancelled,
                        };
                        responder.respond(acp::RequestPermissionResponse::new(outcome))
                    })
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |req: acp::ReadTextFileRequest,
                            responder: Responder<acp::ReadTextFileResponse>,
                            _cx| {
                    match read_text_file(&cwd_r, &req.path, req.line, req.limit).await {
                        Ok(content) => responder.respond(acp::ReadTextFileResponse::new(content)),
                        Err(e) => responder.respond_with_error(
                            agent_client_protocol::Error::invalid_params().data(e.to_string()),
                        ),
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |req: acp::WriteTextFileRequest,
                            responder: Responder<acp::WriteTextFileResponse>,
                            _cx| {
                    match write_text_file(&cwd_w, &req.path, &req.content).await {
                        Ok(()) => responder.respond(acp::WriteTextFileResponse::new()),
                        Err(e) => responder.respond_with_error(
                            agent_client_protocol::Error::invalid_params().data(e.to_string()),
                        ),
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(transport, async |cx: ConnectionTo<Agent>| {
                let session = self.handshake(&cx).await?;
                let sid = SessionId(session.to_string());
                if let Some(r) = ready.lock().take() {
                    let _ = r.send(Ok(sid.clone()));
                }
                self.emit
                    .send(AgentEvent::SessionStarted {
                        session: sid,
                        agent: self.agent.clone(),
                        cwd: self.cwd.clone(),
                    })
                    .await;
                self.serve(&cx, session, cmd_rx).await
            })
            .await
            .map_err(|e| anyhow!("{e}"))
    }

    async fn handshake(
        &self,
        cx: &ConnectionTo<Agent>,
    ) -> agent_client_protocol::Result<acp::SessionId> {
        let caps = acp::ClientCapabilities::new()
            .fs(acp::FileSystemCapabilities::new()
                .read_text_file(true)
                .write_text_file(true))
            .terminal(false);
        cx.send_request(
            acp::InitializeRequest::new(ProtocolVersion::V1)
                .client_capabilities(caps)
                .client_info(acp::Implementation::new(
                    "tandem",
                    env!("CARGO_PKG_VERSION"),
                )),
        )
        .block_task()
        .await?;
        let servers = self
            .mcp_servers
            .iter()
            .map(|s| {
                acp::McpServer::Stdio(
                    acp::McpServerStdio::new(s.name.clone(), s.command.clone())
                        .args(s.args.clone())
                        .env(
                            s.env
                                .iter()
                                .map(|(k, v)| acp::EnvVariable::new(k.clone(), v.clone()))
                                .collect(),
                        ),
                )
            })
            .collect();
        let resp = cx
            .send_request(acp::NewSessionRequest::new(self.cwd.clone()).mcp_servers(servers))
            .block_task()
            .await?;
        Ok(resp.session_id)
    }

    async fn serve(
        &self,
        cx: &ConnectionTo<Agent>,
        session: acp::SessionId,
        cmd_rx: async_channel::Receiver<Cmd>,
    ) -> agent_client_protocol::Result<()> {
        let closed = cx.incoming_closed().fuse();
        futures::pin_mut!(closed);
        loop {
            let cmd = futures::select! {
                c = cmd_rx.recv().fuse() => c,
                _ = closed => {
                    self.emit.send(AgentEvent::Error { message: "agent closed its output".into() }).await;
                    return Ok(());
                }
            };
            match cmd {
                Ok(Cmd::Prompt(text, reply)) => {
                    let req = acp::PromptRequest::new(
                        session.clone(),
                        vec![acp::ContentBlock::Text(acp::TextContent::new(text))],
                    );
                    let (cx2, emit) = (cx.clone(), self.emit.clone());
                    cx.spawn(async move {
                        let result = cx2.send_request(req).block_task().await;
                        let out = match result {
                            Ok(r) => {
                                if let Some(u) = &r.usage {
                                    emit.send(AgentEvent::Usage(map::end_turn_usage(u))).await;
                                }
                                Ok(map::stop_reason(&r.stop_reason))
                            }
                            Err(e) => {
                                emit.send(AgentEvent::Error {
                                    message: e.to_string(),
                                })
                                .await;
                                Err(anyhow!("prompt failed: {e}"))
                            }
                        };
                        let reason = *out.as_ref().unwrap_or(&StopReason::Error);
                        emit.send(AgentEvent::Done { reason }).await;
                        let _ = reply.send(out);
                        Ok(())
                    })?;
                }
                Ok(Cmd::Cancel) => {
                    cx.send_notification(acp::CancelNotification::new(session.clone()))?;
                    // Dropping senders answers outstanding permission requests with `cancelled`.
                    self.pending.lock().clear();
                }
                Ok(Cmd::Shutdown) | Err(_) => return Ok(()),
            }
        }
    }
}

/// Line transport over the child's stdio that mirrors every JSON-RPC line as `AgentEvent::Raw`.
fn tee_transport(
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    emit: Emit,
) -> impl agent_client_protocol::ConnectTo<Client> {
    let emit_in = emit.clone();
    let incoming = futures::stream::unfold(
        (BufReader::new(stdout).lines(), emit_in),
        async |(mut lines, emit)| match lines.next_line().await {
            Ok(Some(line)) => {
                emit.send(AgentEvent::Raw {
                    direction: Direction::FromAgent,
                    json: line.clone(),
                })
                .await;
                Some((Ok(line), (lines, emit)))
            }
            Ok(None) => None,
            Err(e) => Some((Err(e), (lines, emit))),
        },
    )
    .boxed();
    let outgoing = Box::pin(futures::sink::unfold(
        (stdin, emit),
        async |(mut w, emit), line: String| {
            emit.send(AgentEvent::Raw {
                direction: Direction::ToAgent,
                json: line.clone(),
            })
            .await;
            w.write_all(line.as_bytes()).await?;
            w.write_all(b"\n").await?;
            w.flush().await?;
            Ok::<_, std::io::Error>((w, emit))
        },
    ));
    Lines::new(outgoing, incoming)
}

/// Resolve `path` (absolute or cwd-relative) and require it to stay inside `cwd` (already canonical).
pub(crate) fn resolve_inside(cwd: &Path, path: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    // Canonicalize the deepest existing ancestor so symlinks cannot escape and new files are allowed.
    let mut existing = joined.as_path();
    let mut rest = Vec::new();
    while !existing.exists() {
        rest.push(
            existing
                .file_name()
                .ok_or_else(|| anyhow!("invalid path {}", joined.display()))?,
        );
        existing = existing
            .parent()
            .ok_or_else(|| anyhow!("invalid path {}", joined.display()))?;
    }
    let mut resolved = existing.canonicalize()?;
    for part in rest.into_iter().rev() {
        if part == ".." {
            return Err(anyhow!(
                "path {} escapes the task directory",
                joined.display()
            ));
        }
        resolved.push(part);
    }
    if resolved.starts_with(cwd) {
        Ok(resolved)
    } else {
        Err(anyhow!(
            "path {} is outside the task directory",
            joined.display()
        ))
    }
}

async fn read_text_file(
    cwd: &Path,
    path: &Path,
    line: Option<u32>,
    limit: Option<u32>,
) -> Result<String> {
    let p = resolve_inside(cwd, path)?;
    let text = tokio::fs::read_to_string(&p).await?;
    if line.is_none() && limit.is_none() {
        return Ok(text);
    }
    let skip = line.map_or(0, |l| l.saturating_sub(1) as usize);
    let take = limit.map_or(usize::MAX, |l| l as usize);
    Ok(text.split_inclusive('\n').skip(skip).take(take).collect())
}

async fn write_text_file(cwd: &Path, path: &Path, content: &str) -> Result<()> {
    let p = resolve_inside(cwd, path)?;
    if let Some(parent) = p.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&p, content).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rejects_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        assert!(resolve_inside(&cwd, Path::new("a/b.txt")).is_ok());
        assert!(resolve_inside(&cwd, &cwd.join("x.txt")).is_ok());
        assert!(resolve_inside(&cwd, Path::new("/etc/passwd")).is_err());
        assert!(resolve_inside(&cwd, Path::new("new/../../x")).is_err());
        std::fs::create_dir(cwd.join("sub")).unwrap();
        assert!(resolve_inside(&cwd, Path::new("sub/../../x")).is_err());
    }
}
