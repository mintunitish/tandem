//! Project services behind the Project tree, Search Everywhere (files + symbols), Find in Files,
//! Structure and Recent Files: a background `tandem_project::FileIndex` + `SymbolIndex`, a live
//! `Watcher` that keeps both in sync, and the persisted `RecentFiles` ring.

use crate::editor::{EditorArea, EditorAreaEvent};
use gpui_kit::*;
use parking_lot::{Mutex, RwLock};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tandem_project::{FileIndex, FsEvent, FsEventKind, RecentFiles, SymbolIndex, Watcher};

/// Recent Files are written to disk this long after the last change.
const RECENT_SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

pub enum ProjectModelEvent {
    /// One batch of debounced filesystem changes (already applied to the indexes).
    Fs(Arc<[FsEvent]>),
    /// The file and/or symbol index finished building.
    Indexed,
}

pub struct ProjectModel {
    pub root: PathBuf,
    pub files: Arc<RwLock<FileIndex>>,
    pub symbols: Arc<RwLock<SymbolIndex>>,
    /// The persisted ring; only touched off the UI thread.
    recent: Arc<Mutex<RecentFiles>>,
    /// In-memory copy of the ring (absolute, most recent first).
    recent_list: Vec<PathBuf>,
    /// Touches not yet written to `.tandem/recent.json`.
    recent_pending: Vec<PathBuf>,
    pub files_ready: bool,
    pub symbols_ready: bool,
    _watcher: Option<Watcher>,
    _tasks: Vec<Task<()>>,
    _recent_save: Task<()>,
    _sub: Subscription,
}

impl EventEmitter<ProjectModelEvent> for ProjectModel {}

impl ProjectModel {
    pub fn new(root: PathBuf, editor: &Entity<EditorArea>, cx: &mut Context<Self>) -> Self {
        let files = Arc::new(RwLock::new(FileIndex::default()));
        let symbols = Arc::new(RwLock::new(SymbolIndex::default()));
        let recent = RecentFiles::load(&root);
        let recent_list = recent.list();
        let mut tasks = Vec::new();

        let (r, f) = (root.clone(), files.clone());
        let recent_for_index = recent_list.clone();
        let build_files = cx.background_spawn(async move {
            let mut idx = FileIndex::build(r);
            idx.set_recent(&recent_for_index);
            *f.write() = idx;
        });
        tasks.push(cx.spawn(async move |this, cx| {
            build_files.await;
            let _ = this.update(cx, |m, cx| {
                m.files_ready = true;
                cx.emit(ProjectModelEvent::Indexed);
                cx.notify();
            });
        }));
        let (r, s) = (root.clone(), symbols.clone());
        let build_symbols = cx.background_spawn(async move {
            let idx = SymbolIndex::build(r);
            *s.write() = idx;
        });
        tasks.push(cx.spawn(async move |this, cx| {
            build_symbols.await;
            let _ = this.update(cx, |m, cx| {
                m.symbols_ready = true;
                cx.emit(ProjectModelEvent::Indexed);
                cx.notify();
            });
        }));

        // The one project watcher: every queued event is applied to the indexes as one batch,
        // then re-emitted for the Project tree, open editors, Commit and Structure.
        let watcher = Watcher::new(&root).ok();
        if let Some(w) = &watcher {
            let rx = w.events();
            let (f, s) = (files.clone(), symbols.clone());
            let executor = cx.background_executor().clone();
            tasks.push(cx.spawn(async move |this, cx| {
                while let Ok(first) = rx.recv().await {
                    let mut batch = vec![first];
                    while let Ok(e) = rx.try_recv() {
                        batch.push(e);
                    }
                    let batch: Arc<[FsEvent]> = batch.into();
                    let (f, s, evs) = (f.clone(), s.clone(), batch.clone());
                    executor
                        .spawn(async move {
                            let (mut f, mut s) = (f.write(), s.write());
                            for ev in evs.iter() {
                                f.apply_event(ev);
                                s.apply_event(ev);
                            }
                        })
                        .await;
                    let alive = this.update(cx, |m, cx| {
                        let removed: Vec<&PathBuf> = batch.iter().filter(|e| e.kind == FsEventKind::Removed).flat_map(|e| &e.paths).collect();
                        if !removed.is_empty() {
                            m.recent_list.retain(|p| !removed.iter().any(|r| p.starts_with(r)));
                        }
                        cx.emit(ProjectModelEvent::Fs(batch));
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            }));
        }

        let sub = cx.subscribe(editor, |this: &mut Self, _, ev: &EditorAreaEvent, cx| {
            if let EditorAreaEvent::ActiveChanged(Some(path)) = ev {
                this.touch_recent(path, cx);
            }
        });
        Self {
            root,
            files,
            symbols,
            recent: Arc::new(Mutex::new(recent)),
            recent_list,
            recent_pending: vec![],
            files_ready: false,
            symbols_ready: false,
            _watcher: watcher,
            _tasks: tasks,
            _recent_save: Task::ready(()),
            _sub: sub,
        }
    }

    /// Move `path` to the front of Recent Files; the ring is persisted off the UI thread,
    /// debounced.
    pub fn touch_recent(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.recent_list.retain(|p| p != path);
        self.recent_list.insert(0, path.to_path_buf());
        self.recent_list.truncate(tandem_project::recent::DEFAULT_CAPACITY);
        self.files.write().set_recent(&self.recent_list);
        self.recent_pending.retain(|p| p != path);
        self.recent_pending.push(path.to_path_buf());
        let recent = self.recent.clone();
        self._recent_save = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RECENT_SAVE_DEBOUNCE).await;
            let Ok(pending) = this.update(cx, |m, _| std::mem::take(&mut m.recent_pending)) else { return };
            cx.background_spawn(async move {
                let mut r = recent.lock();
                for p in pending {
                    r.touch(p);
                }
            })
            .await;
        });
    }

    /// Write pending Recent Files touches now (quit / window close).
    pub fn flush_recent(&mut self) {
        self._recent_save = Task::ready(());
        let pending = std::mem::take(&mut self.recent_pending);
        if pending.is_empty() {
            return;
        }
        let mut r = self.recent.lock();
        for p in pending {
            r.touch(p);
        }
    }

    /// Recent files (absolute, most recent first).
    pub fn recent_files(&self) -> &[PathBuf] {
        &self.recent_list
    }
}

/// Current branch name (or short detached HEAD id), via libgit2.
pub fn current_branch(root: &Path) -> Option<String> {
    let repo = tandem_git::Repo::open(root).ok()?;
    match repo.current_branch() {
        Ok(Some(b)) => Some(b),
        _ => repo.head_id().ok().flatten().map(|id| id.chars().take(8).collect()),
    }
}
