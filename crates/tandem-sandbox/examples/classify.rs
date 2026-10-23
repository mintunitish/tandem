//! Headless demo: classify shell commands given as arguments (or one per stdin line).
//!
//! cargo run -p tandem-sandbox --example classify -- 'rm -rf / ; true' 'cargo test'
use std::io::BufRead;
use tandem_core::Policy;
use tandem_sandbox::Classifier;

fn main() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let c = Classifier::new(Policy::default(), &cwd, &cwd);
    let mut cmds: Vec<String> = std::env::args().skip(1).collect();
    if cmds.is_empty() {
        cmds = std::io::stdin().lock().lines().map_while(Result::ok).collect();
    }
    for cmd in cmds {
        let (class, reason) = c.classify_command(&cmd);
        println!("{:<11} {:<60} {}", format!("{class:?}"), cmd.replace('\n', "\\n"), reason);
    }
}
