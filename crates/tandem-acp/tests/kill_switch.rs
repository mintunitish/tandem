//! `tandem_acp::stop_all()` kills every running agent and blocks new spawns until reset.

use std::collections::BTreeMap;
use std::time::Duration;
use tandem_acp::{AcpAgent, SpawnOptions};
use tandem_core::{AgentDescriptor, Policy, TaskId};

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

#[tokio::test]
async fn stop_all_kills_agents_and_refuses_spawns() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = SpawnOptions::new(dir.path(), dir.path(), Policy::default());
    opts.sandbox = false;
    let (tx, _rx) = tandem_core::unbounded();
    let h = AcpAgent::spawn(&fake(), opts.clone(), tx.clone(), TaskId::new())
        .await
        .expect("spawn");
    assert_eq!(tandem_sandbox::KillSwitch::global().entries().len(), 1);
    let prompt = h.prompt("please hang".into());
    let stop = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        tokio::task::spawn_blocking(tandem_acp::stop_all).await.unwrap()
    };
    let (res, report) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(prompt, stop)
    })
    .await
    .expect("agent must die on STOP");
    assert_eq!(report.terminated.len(), 1);
    assert!(res.is_err() || res.unwrap() != tandem_core::StopReason::EndTurn);
    h.shutdown().await;

    let err = AcpAgent::spawn(&fake(), opts.clone(), tx.clone(), TaskId::new())
        .await
        .err()
        .expect("refused while tripped");
    assert!(err.to_string().contains("STOP"), "{err}");
    tandem_acp::reset_stop();
    let h = AcpAgent::spawn(&fake(), opts, tx, TaskId::new())
        .await
        .expect("spawn after reset");
    h.shutdown().await;
    assert!(tandem_sandbox::KillSwitch::global().entries().is_empty(), "unregistered on exit");
}
