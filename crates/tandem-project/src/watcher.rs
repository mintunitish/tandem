//! Debounced filesystem watcher (FSEvents on macOS via `notify`'s recommended backend).

use crate::is_excluded_path;
use anyhow::{Context, Result};
use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tandem_core::{Receiver, Sender, unbounded};

/// Quiet period before a batch of raw events is flushed.
pub const DEBOUNCE: Duration = Duration::from_millis(100);
/// Upper bound on how long a continuous event storm can delay a flush.
const MAX_BATCH_LATENCY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FsEventKind {
    Created,
    Modified,
    Removed,
    /// `paths` is `[from, to]` when both sides are known, otherwise the single affected path.
    Renamed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsEvent {
    pub kind: FsEventKind,
    pub paths: Vec<PathBuf>,
}

/// Keeps the OS watcher alive; dropping it stops the debounce thread and closes the channel.
pub struct Watcher {
    _inner: RecommendedWatcher,
    rx: Receiver<FsEvent>,
    root: PathBuf,
}

impl Watcher {
    /// Watch `root` recursively. Events below `.git`, `.tandem`, `target` and `node_modules` are dropped.
    /// Paths in events are rooted at the canonicalized `root` (see [`Watcher::root`]).
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().canonicalize().with_context(|| format!("watch root {:?}", root.as_ref()))?;
        let (raw_tx, raw_rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut inner = notify::recommended_watcher(move |res| {
            let _ = raw_tx.send(res);
        })?;
        inner.watch(&root, RecursiveMode::Recursive)?;
        let (tx, rx) = unbounded();
        let thread_root = root.clone();
        std::thread::Builder::new()
            .name("tandem-fs-debounce".into())
            .spawn(move || debounce_loop(thread_root, raw_rx, tx))
            .context("spawn debounce thread")?;
        Ok(Self { _inner: inner, rx, root })
    }

    /// The canonical root all event paths are relative to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A clone of the event receiver.
    pub fn events(&self) -> Receiver<FsEvent> {
        self.rx.clone()
    }
}

fn debounce_loop(root: PathBuf, raw: mpsc::Receiver<notify::Result<notify::Event>>, out: Sender<FsEvent>) {
    let mut batch: Vec<FsEvent> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    loop {
        // Block for the first event of a batch (exits when the watcher is dropped).
        let first = match raw.recv() {
            Ok(r) => r,
            Err(_) => return,
        };
        push_raw(&root, first, &mut batch);
        let started = Instant::now();
        loop {
            let remaining = MAX_BATCH_LATENCY.saturating_sub(started.elapsed()).min(DEBOUNCE);
            if remaining.is_zero() {
                break;
            }
            match raw.recv_timeout(remaining) {
                Ok(r) => push_raw(&root, r, &mut batch),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    flush(&mut batch, &mut seen, &out);
                    return;
                }
            }
        }
        if !flush(&mut batch, &mut seen, &out) {
            return;
        }
    }
}

/// FSEvents flags are sticky: a later write to a file we already saw created (or that was created
/// long ago) can be reported as `Created` again. Downgrade those to `Modified`.
fn normalize(mut ev: FsEvent, seen: &mut HashSet<PathBuf>) -> FsEvent {
    match ev.kind {
        FsEventKind::Created => {
            if let Some(p) = ev.paths.first() {
                let old = std::fs::metadata(p)
                    .and_then(|m| m.created())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(2));
                if seen.contains(p) || old {
                    ev.kind = FsEventKind::Modified;
                } else {
                    if seen.len() > 100_000 {
                        seen.clear();
                    }
                    seen.insert(p.clone());
                }
            }
        }
        FsEventKind::Removed => ev.paths.iter().for_each(|p| {
            seen.remove(p);
        }),
        _ => {}
    }
    ev
}

fn flush(batch: &mut Vec<FsEvent>, seen: &mut HashSet<PathBuf>, out: &Sender<FsEvent>) -> bool {
    for ev in coalesce(std::mem::take(batch)) {
        let ev = normalize(ev, seen);
        if out.send_blocking(ev).is_err() {
            return false;
        }
    }
    true
}

fn push_raw(root: &Path, res: notify::Result<notify::Event>, batch: &mut Vec<FsEvent>) {
    let ev = match res {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("watcher error: {e}");
            return;
        }
    };
    let kind = match ev.kind {
        EventKind::Create(_) => FsEventKind::Created,
        EventKind::Remove(_) => FsEventKind::Removed,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => FsEventKind::Renamed,
        EventKind::Modify(ModifyKind::Name(_)) => FsEventKind::Renamed,
        EventKind::Modify(_) => FsEventKind::Modified,
        EventKind::Any | EventKind::Other => FsEventKind::Modified,
        EventKind::Access(_) => return,
    };
    let paths: Vec<PathBuf> = ev.paths.into_iter().filter(|p| !is_excluded_path(root, p)).collect();
    if paths.is_empty() {
        return;
    }
    batch.push(FsEvent { kind, paths });
}

/// Merge a raw batch into one event per path and pair up rename halves.
///
/// FSEvents flags are sticky (a deleted file can arrive as create+modify+remove), so the final
/// kind of a path is decided from the raw kinds seen *and* whether the path exists right now.
fn coalesce(batch: Vec<FsEvent>) -> Vec<FsEvent> {
    use FsEventKind::*;
    let mut order: Vec<PathBuf> = Vec::new();
    let mut kinds: std::collections::HashMap<PathBuf, (bool, bool)> = Default::default(); // (created, removed)
    let mut renames: Vec<FsEvent> = Vec::new();
    let mut half_renames: Vec<PathBuf> = Vec::new();
    for ev in batch {
        if ev.kind == Renamed {
            if ev.paths.len() >= 2 {
                renames.push(ev);
            } else {
                half_renames.extend(ev.paths);
            }
            continue;
        }
        for p in ev.paths {
            let e = kinds.entry(p.clone()).or_insert_with(|| {
                order.push(p);
                (false, false)
            });
            e.0 |= ev.kind == Created;
            e.1 |= ev.kind == Removed;
        }
    }
    half_renames.sort();
    half_renames.dedup();
    let (gone, present): (Vec<_>, Vec<_>) = half_renames.into_iter().partition(|p| !p.exists());
    let mut out = Vec::new();
    if gone.len() == 1 && present.len() == 1 {
        out.push(FsEvent { kind: FsEventKind::Renamed, paths: vec![gone[0].clone(), present[0].clone()] });
    } else {
        for p in gone.into_iter().chain(present) {
            out.push(FsEvent { kind: FsEventKind::Renamed, paths: vec![p] });
        }
    }
    out.extend(renames);
    for p in order {
        let (created, removed) = kinds[&p];
        let kind = if p.symlink_metadata().is_err() {
            Removed
        } else if created || removed {
            Created
        } else {
            Modified
        };
        out.push(FsEvent { kind, paths: vec![p] });
    }
    out
}
