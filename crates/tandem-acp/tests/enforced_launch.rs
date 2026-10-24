//! Seatbelt wrapping of agent launches and the sandbox classifier on permission requests.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use tandem_acp::{AcpAgent, SpawnOptions, launch};
use tandem_core::{AgentDescriptor, AgentEvent, Decider, EventRx, Policy, RiskClass, TaskId};
use tandem_sandbox::seatbelt::{self, SANDBOX_EXEC};

fn fake() -> AgentDescriptor {
    AgentDescriptor {
        id: "fake".into(),
        display_name: "Fake".into(),
        vendor: "test".into(),
        command: env!("CARGO_BIN_EXE_tandem-acp-fake-agent").into(),
        args: vec![],
        env: BTreeMap::new(),
        install_hint: String::new(),
        acp: true,
        auth_note: String::new(),
        ..Default::default()
    }
}

fn sandbox_works() -> bool {
    Path::new(SANDBOX_EXEC).exists()
        && std::process::Command::new(SANDBOX_EXEC)
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .status()
            .is_ok_and(|s| s.success())
}

fn sh(spec: &tandem_sandbox::SandboxSpec, script: &str) -> std::process::Output {
    let (prog, args) = seatbelt::wrap_command(spec, "/bin/sh", &["-c".into(), script.into()]);
    std::process::Command::new(prog)
        .args(args)
        .current_dir(&spec.worktree_root)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin")
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .output()
        .unwrap()
}

#[test]
fn seatbelt_denies_outside_file_allows_inside() {
    if !sandbox_works() {
        eprintln!("skipping: sandbox-exec unavailable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let project = root.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("inside.txt"), "inside-ok").unwrap();
    std::fs::write(project.join(".env"), "K=canary-env").unwrap();
    let outside = root.join("outside.txt");
    std::fs::write(&outside, "outside-secret").unwrap();

    let mut opts = SpawnOptions::new(&project, &project, Policy::default());
    opts.sandbox = true;
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
    let mut spec = launch::sandbox_spec(&fake(), &opts, &home, None);
    spec.temp_dirs.clear(); // `outside` lives under $TMPDIR

    let out = sh(&spec, &format!("cat {}", project.join("inside.txt").display()));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "inside-ok");

    let out = sh(&spec, &format!("cat {}", outside.display()));
    assert!(!out.status.success(), "outside read must fail");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("outside-secret"));

    let out = sh(&spec, &format!("cat {}", project.join(".env").display()));
    assert!(!out.status.success(), ".env read must fail");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("canary-env"));
}

/// npx-launched adapters need the npm cache readable and writable inside the profile.
#[test]
fn npm_cache_usable_inside_agent_profile() {
    if !sandbox_works() {
        eprintln!("skipping: sandbox-exec unavailable");
        return;
    }
    let Ok(home) = std::env::var("HOME") else { return };
    let tmp = tempfile::tempdir().unwrap();
    let mut d = fake();
    d.id = "codex".into();
    let mut opts = SpawnOptions::new(tmp.path(), tmp.path(), Policy::default());
    opts.sandbox = true;
    let spec = launch::sandbox_spec(&d, &opts, Path::new(&home), None);
    let probe = format!("{home}/.npm/_tandem_sandbox_probe_{}", std::process::id());
    let out = sh(
        &spec,
        &format!("mkdir -p '{probe}' && echo ok > '{probe}/f' && cat '{probe}/f' && rm -rf '{probe}'"),
    );
    assert!(out.status.success(), "npm cache not writable: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    let cache = format!("{home}/.cache/_tandem_sandbox_probe_{}", std::process::id());
    let out = sh(&spec, &format!("mkdir -p '{cache}' && rmdir '{cache}'"));
    assert!(out.status.success(), "~/.cache not writable: {}", String::from_utf8_lossy(&out.stderr));
    // Real npm, when present: it must start, read its config and resolve its cache.
    if Path::new("/opt/homebrew/bin/npm").exists() || Path::new("/usr/local/bin/npm").exists() {
        let out = sh(&spec, "npm config get cache && npx --version");
        assert!(
            out.status.success(),
            "npm inside sandbox failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

async fn next(rx: &EventRx) -> AgentEvent {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timeout")
            .expect("closed")
            .event;
        if !matches!(ev, AgentEvent::Raw { .. }) {
            return ev;
        }
    }
}

async fn start(sandbox: bool) -> (tandem_acp::AgentHandle, EventRx, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = SpawnOptions::new(dir.path(), dir.path(), Policy::default());
    opts.sandbox = sandbox;
    let (tx, rx) = tandem_core::unbounded();
    let h = AcpAgent::spawn(&fake(), opts, tx, TaskId::new())
        .await
        .expect("spawn");
    assert!(matches!(next(&rx).await, AgentEvent::SessionStarted { .. }));
    (h, rx, dir)
}

#[tokio::test]
async fn safe_is_auto_allowed_by_policy() {
    let (h, rx, _dir) = start(false).await;
    let prompt = h.prompt("safe-read".into());
    let drive = async {
        assert!(matches!(next(&rx).await, AgentEvent::TextDelta { role: tandem_core::Role::User, .. }));
        assert!(matches!(next(&rx).await, AgentEvent::ToolCall(_)));
        let AgentEvent::PermissionRequested(p) = next(&rx).await else { panic!("permission") };
        assert_eq!(p.risk, RiskClass::Safe, "{}", p.reason);
        assert!(!p.reason.is_empty());
        assert!(!h.is_pending(&p.id), "answered by policy");
        assert!(matches!(next(&rx).await, AgentEvent::PermissionResolved { by: Decider::Policy, ref option, .. } if option == "allow"));
        let AgentEvent::ToolCallUpdate(u) = next(&rx).await else { panic!("update") };
        assert_eq!(u.content.as_deref(), Some("permission=allow"));
    };
    let (r, ()) = tokio::join!(prompt, drive);
    r.unwrap();
    h.shutdown().await;
}

#[tokio::test]
async fn destructive_is_never_auto_answered() {
    let sandbox = sandbox_works();
    let (h, rx, _dir) = start(sandbox).await;
    let prompt = h.prompt("do it".into());
    let drive = async {
        let p = loop {
            if let AgentEvent::PermissionRequested(p) = next(&rx).await {
                break p;
            }
        };
        assert_eq!(p.risk, RiskClass::Destructive, "rm -rf: {}", p.reason);
        assert!(!p.reason.is_empty());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(h.is_pending(&p.id), "Destructive must wait for a human even with auto_allow_safe");
        h.respond_permission(
            tandem_core::PermissionDecision { request: p.id, option_id: "reject".into() },
            Decider::User,
        )
        .await
        .unwrap();
    };
    let (r, ()) = tokio::join!(prompt, drive);
    r.unwrap();
    h.shutdown().await;
}
