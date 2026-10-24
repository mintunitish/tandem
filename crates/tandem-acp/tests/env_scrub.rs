//! The agent never sees host secrets: spawn the fake agent with a canary in the parent
//! environment and have it dump its own `std::env::vars()`.

use std::time::Duration;
use tandem_acp::{AcpAgent, SpawnOptions};
use tandem_core::{AgentDescriptor, AgentEvent, Policy, TaskId};

/// The built-in claude descriptor (vendor rules: CLAUDE_* kept, CLAUDE_CODE_* must not be)
/// running the fake agent binary.
fn fake() -> AgentDescriptor {
    AgentDescriptor {
        command: env!("CARGO_BIN_EXE_tandem-acp-fake-agent").into(),
        args: vec![],
        ..tandem_acp::get("claude").unwrap()
    }
}

fn sandbox_works() -> bool {
    std::process::Command::new("/usr/bin/sandbox-exec")
        .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
        .status()
        .is_ok_and(|s| s.success())
}

async fn dumped_env(sandbox: bool) -> String {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = SpawnOptions::new(dir.path(), dir.path(), Policy::default());
    opts.sandbox = sandbox;
    opts.env_overrides.insert("TANDEM_OVERRIDE".into(), "yes".into());
    let (tx, rx) = tandem_core::unbounded();
    let h = AcpAgent::spawn(&fake(), opts, tx, TaskId::new())
        .await
        .expect("spawn");
    let stop = tokio::time::timeout(Duration::from_secs(20), h.prompt("dump-env".into()))
        .await
        .expect("timeout")
        .expect("prompt");
    assert_eq!(stop, tandem_core::StopReason::EndTurn);
    h.shutdown().await;
    let mut out = String::new();
    while let Ok(te) = rx.try_recv() {
        if let AgentEvent::ToolCall(c) = te.event
            && let Some(text) = c.content
        {
            out.push_str(&text);
        }
    }
    out
}

#[tokio::test]
async fn canary_and_session_vars_never_reach_the_agent() {
    // SAFETY: the only test in this binary; set before any agent thread exists.
    unsafe {
        std::env::set_var("TANDEM_CANARY_SECRET", "x-canary-31337");
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_CODE_ENTRYPOINT", "cli");
        std::env::set_var("SOME_SERVICE_TOKEN", "tok-31337");
        std::env::set_var("CLAUDE_CONFIG_DIR", "/tmp/claude-cfg");
    }
    let mut modes = vec![false];
    if sandbox_works() {
        modes.push(true);
    } else {
        eprintln!("skipping sandboxed variant: sandbox-exec unavailable");
    }
    for sandbox in modes {
        let env = dumped_env(sandbox).await;
        assert!(env.contains("PATH="), "sandbox={sandbox}: no env dumped: {env}");
        assert!(env.contains("TANDEM_OVERRIDE=yes"), "sandbox={sandbox}");
        assert!(env.contains("CLAUDE_CONFIG_DIR=/tmp/claude-cfg"), "own vendor vars kept");
        for leaked in ["x-canary-31337", "CLAUDECODE=", "CLAUDE_CODE_ENTRYPOINT", "tok-31337"] {
            assert!(!env.contains(leaked), "sandbox={sandbox}: leaked {leaked}:\n{env}");
        }
    }
}
