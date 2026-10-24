//! Terminal sessions requested from the cockpit (agent sign-in). The terminal panel owns the
//! UI: it drains [`take_pending`] and shows each session as a tab.

use parking_lot::Mutex;
use std::path::PathBuf;
use tandem_terminal::{SpawnOptions, Terminal};

pub struct TerminalRequest {
    pub title: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

static PENDING: Mutex<Vec<(TerminalRequest, Terminal)>> = Mutex::new(Vec::new());

/// Spawn `req` in a PTY and queue it for the terminal panel.
pub fn open_terminal_with(req: TerminalRequest) -> anyhow::Result<()> {
    let term = Terminal::spawn(SpawnOptions {
        shell: Some(req.program.clone()),
        args: req.args.clone(),
        cwd: req.cwd.clone(),
        cols: 120,
        rows: 32,
        ..Default::default()
    })?;
    PENDING.lock().push((req, term));
    Ok(())
}

/// Sessions spawned since the last call, oldest first.
pub fn take_pending() -> Vec<(TerminalRequest, Terminal)> {
    std::mem::take(&mut *PENDING.lock())
}
