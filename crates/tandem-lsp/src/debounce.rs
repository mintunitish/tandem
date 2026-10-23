//! Coalesces bursts of document changes into one `didChange` per file.

use crate::LspClient;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

pub const DEFAULT_DELAY: Duration = Duration::from_millis(50);
/// Upper bound on how long a continuous burst can delay a sync.
pub const DEFAULT_MAX_WAIT: Duration = Duration::from_millis(500);

enum Cmd {
    Change { path: PathBuf, version: i32, text: String },
    Flush(oneshot::Sender<()>),
}

/// Trailing-edge debouncer for full-text sync: the latest text per path is sent
/// `delay` after the last change (or `max_wait` after the first one of a burst).
/// Call [`flush`](Self::flush) before `did_save` or any request that must see the
/// newest text. Must be created inside a tokio runtime.
#[derive(Clone)]
pub struct ChangeDebouncer {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl ChangeDebouncer {
    pub fn new(client: LspClient) -> Self {
        Self::with_delay(client, DEFAULT_DELAY, DEFAULT_MAX_WAIT)
    }

    pub fn with_delay(client: LspClient, delay: Duration, max_wait: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(client, rx, delay, max_wait));
        Self { tx }
    }

    pub fn push(&self, path: &Path, version: i32, text: impl Into<String>) {
        let _ = self.tx.send(Cmd::Change { path: path.to_path_buf(), version, text: text.into() });
    }

    /// Send everything pending now and wait until it has been queued to the server.
    pub async fn flush(&self) {
        let (ack, done) = oneshot::channel();
        if self.tx.send(Cmd::Flush(ack)).is_ok() {
            let _ = done.await;
        }
    }
}

async fn run(client: LspClient, mut rx: mpsc::UnboundedReceiver<Cmd>, delay: Duration, max_wait: Duration) {
    let mut pending: HashMap<PathBuf, (i32, String)> = HashMap::new();
    let mut first: Option<Instant> = None;
    let mut last = Instant::now();
    let send_all = |pending: &mut HashMap<PathBuf, (i32, String)>| {
        for (path, (version, text)) in pending.drain() {
            if let Err(e) = client.did_change_full(&path, version, &text) {
                tracing::debug!("debounced didChange for {} failed: {e}", path.display());
            }
        }
    };
    loop {
        let cmd = match first {
            None => rx.recv().await,
            Some(start) => {
                let deadline = (last + delay).min(start + max_wait);
                tokio::select! {
                    c = rx.recv() => c,
                    _ = tokio::time::sleep_until(deadline) => {
                        send_all(&mut pending);
                        first = None;
                        continue;
                    }
                }
            }
        };
        match cmd {
            None => {
                send_all(&mut pending);
                return;
            }
            Some(Cmd::Change { path, version, text }) => {
                last = Instant::now();
                first.get_or_insert(last);
                pending.insert(path, (version, text));
            }
            Some(Cmd::Flush(ack)) => {
                send_all(&mut pending);
                first = None;
                let _ = ack.send(());
            }
        }
    }
}
