//! Project services behind the Project tree, Search Everywhere (files + symbols), Find in Files,
//! Structure and Recent Files: a background `tandem_project::FileIndex` + `SymbolIndex`, a live
//! `Watcher` that keeps both in sync, and the persisted `RecentFiles` ring.

use crate::editor::{EditorArea, EditorAreaEvent};
use gpui_kit::*;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tandem_project::{FileIndex, FsEvent, RecentFiles, SymbolIndex, Watcher};

pub enum ProjectModelEvent {
    /// A debounced filesystem change (already applied to the indexes).
    Fs(FsEvent),
    /// The file and/or symbol index finished building.
    Indexed,
}

pub struct ProjectModel {
    pub root: PathBuf,
    pub files: Arc<RwLock<FileIndex>>,
    pub symbols: Arc<RwLock<SymbolIndex>>,
    pub recent: RecentFiles,
    pub files_ready: bool,
    pub symbols_ready: bool,
    _watcher: Option<Watcher>,
    _tasks: Vec<Task<()>>,
    _sub: Subscription,
}

impl EventEmitter<ProjectModelEvent> for ProjectModel {}

impl ProjectModel {
    pub fn new(root: PathBuf, editor: &Entity<EditorArea>, cx: &mut Context<Self>) -> Self {
        let files = Arc::new(RwLock::new(FileIndex::default()));
        let symbols = Arc::new(RwLock::new(SymbolIndex::default()));
        let recent = RecentFiles::load(&root);
        let mut tasks = Vec::new();

        let (r, f) = (root.clone(), files.clone());
        let recent_list = recent.list();
        let build_files = cx.background_spawn(async move {
            let mut idx = FileIndex::build(r);
            idx.set_recent(&recent_list);
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

        let watcher = Watcher::new(&root).ok();
        if let Some(w) = &watcher {
            let rx = w.events();
            let (f, s) = (files.clone(), symbols.clone());
            let executor = cx.background_executor().clone();
            tasks.push(cx.spawn(async move |this, cx| {
                while let Ok(ev) = rx.recv().await {
                    let (f, s, ev2) = (f.clone(), s.clone(), ev.clone());
                    executor
                        .spawn(async move {
                            f.write().apply_event(&ev2);
                            s.write().apply_event(&ev2);
                        })
                        .await;
                    if this.update(cx, |_, cx| cx.emit(ProjectModelEvent::Fs(ev))).is_err() {
                        break;
                    }
                }
            }));
        }

        let sub = cx.subscribe(editor, |this: &mut Self, _, ev: &EditorAreaEvent, _| {
            if let EditorAreaEvent::ActiveChanged(Some(path)) = ev {
                this.touch_recent(path);
            }
        });
        Self { root, files, symbols, recent, files_ready: false, symbols_ready: false, _watcher: watcher, _tasks: tasks, _sub: sub }
    }

    pub fn touch_recent(&mut self, path: &Path) {
        self.recent.touch(path);
        let list = self.recent.list();
        self.files.write().set_recent(&list);
    }

    /// Recent files (absolute, most recent first) that still exist.
    pub fn recent_files(&self) -> Vec<PathBuf> {
        self.recent.list().into_iter().filter(|p| p.exists()).collect()
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

/// Every indexed file (relative, `/`-separated). Kept for callers that only need a flat list.
pub fn index_files(root: &Path, limit: usize) -> Vec<PathBuf> {
    FileIndex::build(root).paths().iter().take(limit).map(PathBuf::from).collect()
}
