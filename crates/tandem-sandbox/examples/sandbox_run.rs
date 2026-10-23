//! Headless demo: run a command under the Tandem Seatbelt profile with a scrubbed env,
//! in its own process group, using the current directory as project and worktree.
//!
//! cargo run -p tandem-sandbox --example sandbox_run -- codex git status
//! TANDEM_PRINT_PROFILE=1 cargo run -p tandem-sandbox --example sandbox_run -- claude true
use std::collections::HashMap;
use std::process::Command;
use tandem_core::{AgentDescriptor, Policy};
use tandem_sandbox::{env, kill, seatbelt};

fn main() {
    let mut args = std::env::args().skip(1);
    let agent_id = args.next().unwrap_or_else(|| "codex".into());
    let cmd = args.next().unwrap_or_else(|| "true".into());
    let rest: Vec<String> = args.collect();
    let cwd = std::env::current_dir().expect("cwd");
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut spec = seatbelt::SandboxSpec::new(&cwd, &cwd, &home);
    spec.agent_home_dirs = seatbelt::SandboxSpec::default_agent_home_dirs(&agent_id, &home);
    if std::env::var_os("TANDEM_PRINT_PROFILE").is_some() {
        println!("{}", seatbelt::profile(&spec));
    }
    let agent = AgentDescriptor {
        id: agent_id.as_str().into(),
        display_name: agent_id.clone(),
        vendor: String::new(),
        command: cmd.clone(),
        args: rest.clone(),
        env: Default::default(),
        install_hint: String::new(),
        acp: false,
        auth_note: String::new(),
    };
    let parent: HashMap<String, String> = std::env::vars().collect();
    let scrubbed = env::scrub(&parent, &Policy::default(), &agent);
    let (prog, wrapped) = seatbelt::wrap_command(&spec, &cmd, &rest);
    let mut c = Command::new(prog);
    c.args(wrapped).current_dir(&cwd);
    env::apply(&mut c, &scrubbed);
    kill::configure(&mut c);
    let status = c.status().expect("spawn sandbox-exec");
    std::process::exit(status.code().unwrap_or(1));
}
