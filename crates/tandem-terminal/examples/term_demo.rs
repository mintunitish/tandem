//! Run a command in a PTY and dump the final grid as text.
//!
//! `cargo run -p tandem-terminal --example term_demo -- [cmd args...]`
//! (default: `ls -la --color=auto` through `/bin/sh`).

use std::time::Duration;

use tandem_terminal::{SpawnOptions, Terminal};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let script = if args.is_empty() { "ls -la; printf '\\033[1;32mdone\\033[0m \\xf0\\x9f\\x98\\x80\\n'".to_string() } else { args.join(" ") };
    let term = Terminal::spawn(SpawnOptions {
        shell: Some("/bin/sh".into()),
        args: vec!["-c".into(), script],
        cols: 100,
        rows: 30,
        ..Default::default()
    })?;
    let code = term.wait_exit(Duration::from_secs(30));
    let snap = term.snapshot();
    println!("{}", snap.text());
    println!(
        "--- {}x{} cursor=({},{}) scrollback={} title={:?} exit={:?}",
        snap.cols, snap.rows, snap.cursor.row, snap.cursor.col, snap.scrollback_len, snap.title, code
    );
    Ok(())
}
