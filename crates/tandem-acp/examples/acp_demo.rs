//! Headless ACP demo.
//!
//! cargo run -p tandem-acp --example acp_demo -- --agent codex --cwd . --prompt "Reply with pong" [--auto-approve]

use std::path::PathBuf;
use tandem_acp::{AcpAgent, SpawnOptions, registry};
use tandem_core::{AgentEvent, Decider, PermissionOptionKind, TaskId};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (mut agent, mut cwd, mut prompt, mut auto, mut raw) =
        (None, PathBuf::from("."), None, false, false);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--agent" => agent = args.next(),
            "--cwd" => cwd = args.next().map(PathBuf::from).unwrap_or(cwd),
            "--prompt" => prompt = args.next(),
            "--auto-approve" => auto = true,
            "--raw" => raw = true,
            _ => anyhow::bail!(
                "usage: acp_demo --agent <id> --cwd <dir> --prompt <text> [--auto-approve] [--raw]"
            ),
        }
    }
    let (Some(id), Some(prompt)) = (agent, prompt) else {
        anyhow::bail!("--agent and --prompt are required")
    };
    let desc = registry::detect_installed()
        .into_iter()
        .find(|d| d.descriptor.id.as_str() == id)
        .map(|d| d.descriptor)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "agent {id} not installed; {}",
                registry::get(&id)
                    .map(|d| d.install_hint)
                    .unwrap_or_default()
            )
        })?;
    eprintln!("launching {} {:?}", desc.command, desc.args);

    let (tx, rx) = tandem_core::unbounded();
    let handle = AcpAgent::spawn(&desc, SpawnOptions::unsandboxed(cwd), tx, TaskId::new()).await?;
    let printer = async {
        while let Ok(ev) = rx.recv().await {
            match ev.event {
                AgentEvent::Raw { .. } if !raw => {}
                AgentEvent::TextDelta { text, .. } => print!("{text}"),
                AgentEvent::PermissionRequested(p) => {
                    println!(
                        "\n[permission] {} {:?} options={:?}",
                        p.tool_call.title,
                        p.tool_call.raw_input,
                        p.options.iter().map(|o| &o.id).collect::<Vec<_>>()
                    );
                    let kind = if auto { PermissionOptionKind::AllowOnce } else { PermissionOptionKind::RejectOnce };
                    println!("[permission] {}", if auto { "auto-approve" } else { "reject" });
                    if let Err(e) = handle.respond_choice(&p, kind, Decider::User).await {
                        eprintln!("respond failed: {e}");
                        handle.cancel().await;
                    }
                }
                other => println!("\n{}", serde_json::to_string(&other).unwrap_or_default()),
            }
        }
    };
    let turn = async {
        let r = handle.prompt(prompt).await;
        println!("\n[stop] {r:?}");
    };
    tokio::select! { _ = turn => {}, _ = printer => {} }
    handle.shutdown().await;
    Ok(())
}
