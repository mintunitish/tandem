//! Bridge between gpui's foreground executor and a shared tokio runtime.
//!
//! Agent processes, database drivers and harness runs are tokio-based; gpui runs
//! its own executor. `spawn_bridge` runs a future on tokio and hands back an
//! `async_channel::Receiver` that gpui tasks can `.recv().await` on.

use std::future::Future;
use std::sync::OnceLock;

/// Lazily-built multi-thread tokio runtime shared by the whole app.
pub fn tokio() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("tandem-tokio")
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    })
}

/// Spawn `fut` on the tokio runtime; its output arrives on the returned receiver.
pub fn spawn_bridge<T: Send + 'static>(
    fut: impl Future<Output = T> + Send + 'static,
) -> async_channel::Receiver<T> {
    let (tx, rx) = async_channel::bounded(1);
    tokio().spawn(async move {
        let out = fut.await;
        let _ = tx.send(out).await;
    });
    rx
}
