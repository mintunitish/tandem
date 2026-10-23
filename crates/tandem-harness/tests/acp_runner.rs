//! AcpRunner against the scripted `tandem-acp-fake-agent` binary from tandem-acp.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tandem_core::*;
use tandem_harness::runner::{AllowAll, RejectAll};
use tandem_harness::{AcpRunner, AgentRunner, PermissionDecider};

/// Path of the fake agent next to this test's target dir, building it if needed.
fn fake_agent_bin() -> PathBuf {
    static BUILD: std::sync::Once = std::sync::Once::new();
    // <target>/<profile>/deps/acp_runner-<hash>
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let bin = profile_dir.join("tandem-acp-fake-agent");
    BUILD.call_once(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = std::process::Command::new(cargo)
            .args(["build", "-p", "tandem-acp", "--bin", "tandem-acp-fake-agent"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("run cargo build");
        assert!(status.success(), "building tandem-acp-fake-agent failed");
    });
    assert!(bin.is_file(), "missing {}", bin.display());
    bin
}

fn descriptor(command: String, args: Vec<String>) -> AgentDescriptor {
    AgentDescriptor {
        id: "fake".into(),
        display_name: "Fake".into(),
        vendor: "test".into(),
        command,
        args,
        env: BTreeMap::new(),
        install_hint: String::new(),
        acp: true,
        auth_note: String::new(),
    }
}

async fn run(runner: &AcpRunner, cwd: &Path, prompt: &str, decider: Arc<dyn PermissionDecider>) -> (anyhow::Result<tandem_harness::RunOutcome>, Vec<AgentEvent>) {
    let (tx, rx) = async_channel::unbounded();
    let agent: AgentId = "fake".into();
    let res = runner.run(&agent, cwd, prompt, decider, tx).await;
    let mut events = vec![];
    while let Ok(te) = rx.try_recv() {
        events.push(te.event);
    }
    (res, events)
}

fn tool_text(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallUpdate(tc) => tc.content.clone(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn forwards_events_answers_permissions_and_sums_usage() {
    let runner = AcpRunner::with_descriptors([descriptor(fake_agent_bin().to_string_lossy().into(), vec![])]);
    let dir = tempfile::tempdir().unwrap();

    let (res, events) = run(&runner, dir.path(), "clean the build", Arc::new(AllowAll)).await;
    let out = res.unwrap();
    assert_eq!(out.stop, StopReason::EndTurn);
    assert_eq!((out.usage.input_tokens, out.usage.output_tokens), (10, 20));
    assert_eq!(out.usage.cost_usd, Some(0.25));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::SessionStarted { .. })));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::PermissionRequested(_))));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::PermissionResolved { option, by: Decider::Harness, .. } if option == "allow")));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Done { reason: StopReason::EndTurn })));
    assert!(tool_text(&events).contains("permission=allow"), "{}", tool_text(&events));
    assert!(dir.path().join("out.txt").is_file());

    let (res, events) = run(&runner, dir.path(), "clean the build", Arc::new(RejectAll)).await;
    res.unwrap();
    assert!(tool_text(&events).contains("permission=reject"), "{}", tool_text(&events));
}

#[tokio::test]
async fn unknown_agent_is_an_error() {
    let runner = AcpRunner::with_descriptors([]);
    let dir = tempfile::tempdir().unwrap();
    let (tx, _rx) = async_channel::unbounded();
    let agent: AgentId = "goose".into();
    let err = runner.run(&agent, dir.path(), "hi", Arc::new(RejectAll), tx).await.unwrap_err();
    assert!(err.to_string().contains("not installed"), "{err}");
}

#[tokio::test]
async fn dropping_the_run_kills_the_agent_process() {
    let marker_dir = tempfile::tempdir().unwrap();
    let marker = marker_dir.path().join("pid");
    // `exec` keeps the pid, so the marker holds the fake agent's own pid.
    let script = format!(r#"echo $$ > "{}"; exec "{}""#, marker.display(), fake_agent_bin().display());
    let runner = AcpRunner::with_descriptors([descriptor("/bin/sh".into(), vec!["-c".into(), script])]);
    let dir = tempfile::tempdir().unwrap();

    let (tx, _rx) = async_channel::unbounded();
    let agent: AgentId = "fake".into();
    let fut = runner.run(&agent, dir.path(), "hang please", Arc::new(RejectAll), tx);
    assert!(tokio::time::timeout(Duration::from_secs(2), fut).await.is_err(), "hang prompt should not finish");

    let pid = std::fs::read_to_string(&marker).unwrap().trim().to_string();
    let alive = || std::process::Command::new("kill").args(["-0", &pid]).stderr(std::process::Stdio::null()).status().unwrap().success();
    for _ in 0..50 {
        if !alive() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("fake agent {pid} still alive after the run future was dropped");
}
