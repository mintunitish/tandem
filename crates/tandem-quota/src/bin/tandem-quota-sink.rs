//! `tandem-quota-sink <file>`: Claude Code status-line command. Reads the status-line
//! JSON from stdin and atomically writes it to `<file>` (0600). Prints nothing, so the
//! user's status bar stays empty. Never reads anything but stdin.

use std::io::Read;
use std::path::PathBuf;

fn main() {
    let Some(out) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: tandem-quota-sink <output-file>");
        std::process::exit(2);
    };
    let mut buf = Vec::new();
    if std::io::stdin().read_to_end(&mut buf).is_err() {
        return;
    }
    if serde_json::from_slice::<serde_json::Value>(&buf).is_err() {
        return; // never clobber a good snapshot with garbage
    }
    if let Err(e) = tandem_quota::safe_fs::atomic_write(&out, &buf) {
        eprintln!("tandem-quota-sink: {e}");
    }
}
