use std::collections::BTreeMap;
use std::time::Duration;
use tandem_acp::{AcpAgent, AgentHandle, ReplayLog};
use tandem_core::{
    AgentDescriptor, AgentEvent, Decider, EventRx, PermissionDecision, Role, StopReason, TaskId,
    ToolKind, ToolStatus,
};

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
    }
}

async fn start(dir: &std::path::Path) -> (AgentHandle, EventRx, TaskId) {
    let (tx, rx) = tandem_core::unbounded();
    let task = TaskId::from("task-1");
    let h = AcpAgent::spawn(
        &fake(),
        dir.to_path_buf(),
        BTreeMap::new(),
        vec![],
        tx,
        task.clone(),
    )
    .await
    .expect("spawn");
    (h, rx, task)
}

/// Next non-raw event, recording everything into `log`.
async fn next(rx: &EventRx, log: &mut Vec<AgentEvent>) -> AgentEvent {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timeout")
            .expect("closed")
            .event;
        log.push(ev.clone());
        if !matches!(ev, AgentEvent::Raw { .. }) {
            return ev;
        }
    }
}

async fn run_turn(allow: bool) -> (Vec<AgentEvent>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (h, rx, _task) = start(dir.path()).await;
    assert_eq!(h.session_id().as_str(), "fake-session");
    let mut all = Vec::new();
    assert!(matches!(
        next(&rx, &mut all).await,
        AgentEvent::SessionStarted { .. }
    ));

    let prompt = {
        let h = &h;
        async move { h.prompt("do it".into()).await }
    };
    let drive = async {
        assert!(
            matches!(next(&rx, &mut all).await, AgentEvent::TextDelta { role: Role::Assistant, ref text } if text == "Hello")
        );
        assert!(
            matches!(next(&rx, &mut all).await, AgentEvent::Plan { ref entries } if entries.len() == 1)
        );
        let AgentEvent::ToolCall(c) = next(&rx, &mut all).await else {
            panic!("tool call")
        };
        assert_eq!(
            (c.kind, c.raw_input.as_deref()),
            (ToolKind::Execute, Some("rm -rf build"))
        );
        let AgentEvent::PermissionRequested(p) = next(&rx, &mut all).await else {
            panic!("permission")
        };
        assert_eq!(
            p.tool_call.title, "Remove build dir",
            "permission call merged with known state"
        );
        assert_eq!(p.risk, tandem_core::RiskClass::Review);
        let option_id = if allow { "allow" } else { "reject" }.to_string();
        h.respond_permission(
            PermissionDecision {
                request: p.id.clone(),
                option_id,
            },
            Decider::Harness,
        )
        .await
        .unwrap();
        assert!(matches!(
            next(&rx, &mut all).await,
            AgentEvent::PermissionResolved {
                by: Decider::Harness,
                ..
            }
        ));
        let AgentEvent::ToolCallUpdate(u) = next(&rx, &mut all).await else {
            panic!("update")
        };
        let expected = if allow {
            "permission=allow"
        } else {
            "permission=reject"
        };
        let content = u.content.unwrap();
        assert!(content.starts_with(expected), "{content}");
        assert!(
            content.contains(r#"write_ok=true read="b\n" outside_rejected=true"#),
            "{content}"
        );
        assert_eq!(
            u.status,
            if allow {
                ToolStatus::Completed
            } else {
                ToolStatus::Failed
            }
        );
        assert!(
            matches!(next(&rx, &mut all).await, AgentEvent::Usage(ref u) if u.cost_usd == Some(0.25))
        );
        assert!(
            matches!(next(&rx, &mut all).await, AgentEvent::Usage(ref u) if u.output_tokens == 20)
        );
        assert!(matches!(
            next(&rx, &mut all).await,
            AgentEvent::Done {
                reason: StopReason::EndTurn
            }
        ));
    };
    let (reason, ()) = tokio::join!(prompt, drive);
    assert_eq!(reason.unwrap(), StopReason::EndTurn);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
        "a\nb\nc\n"
    );
    h.shutdown().await;
    while let Ok(ev) = rx.try_recv() {
        all.push(ev.event);
    }
    (all, dir)
}

#[tokio::test]
async fn full_turn_with_permission_allow_and_replay_log() {
    let (events, dir) = run_turn(true).await;
    let raws = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Raw { .. }))
        .count();
    assert!(raws >= 10, "raw JSON-RPC lines are mirrored, got {raws}");
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Raw { direction: tandem_core::Direction::ToAgent, json } if json.contains("\"initialize\""))));

    let task = TaskId::from("task-1");
    let log_dir = dir.path().join(".tandem/tasks/task-1");
    let mut log = ReplayLog::open(&log_dir).unwrap();
    for e in &events {
        log.record(&task, e).unwrap();
    }
    let back = ReplayLog::read(&log_dir).unwrap();
    assert_eq!(back.len(), events.len());
    assert_eq!(
        serde_json::to_string(&back[3].event).unwrap(),
        serde_json::to_string(&events[3]).unwrap()
    );
}

#[tokio::test]
async fn permission_reject() {
    run_turn(false).await;
}

#[tokio::test]
async fn cancel_mid_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (h, rx, _) = start(dir.path()).await;
    let mut all = Vec::new();
    let prompt = {
        let h = &h;
        async move { h.prompt("please hang".into()).await }
    };
    let drive = async {
        loop {
            if let AgentEvent::TextDelta { text, .. } = next(&rx, &mut all).await {
                assert_eq!(text, "working");
                break;
            }
        }
        h.cancel().await;
        assert!(matches!(
            next(&rx, &mut all).await,
            AgentEvent::Done {
                reason: StopReason::Cancelled
            }
        ));
    };
    let (reason, ()) = tokio::join!(prompt, drive);
    assert_eq!(reason.unwrap(), StopReason::Cancelled);
    h.shutdown().await;
}

#[tokio::test]
async fn missing_binary_is_an_error() {
    let mut d = fake();
    d.command = "/nonexistent/agent-binary".into();
    let (tx, _rx) = tandem_core::unbounded();
    let err = AcpAgent::spawn(
        &d,
        std::env::temp_dir(),
        BTreeMap::new(),
        vec![],
        tx,
        TaskId::new(),
    )
    .await
    .err()
    .expect("error");
    assert!(err.to_string().contains("failed to start"), "{err}");
}
