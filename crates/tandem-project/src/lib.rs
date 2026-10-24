//! tandem-project: the project model behind the Project tool window, Find in Files,
//! Search Everywhere (files + symbols), Structure and Recent Files.
//!
//! * [`tree`]    lazily loaded, ignore-aware file tree + icon keys
//! * [`watcher`] debounced `notify` watcher producing [`FsEvent`]s
//! * [`search`]  Find in Files / Replace in Files (ripgrep engine)
//! * [`fuzzy`]   WebStorm-style fuzzy file finder (camel humps, path segments, recents)
//! * [`symbols`] tree-sitter symbol index (Go to Symbol, Structure)
//! * [`recent`]  Recent Files ring persisted at `.tandem/recent.json`

pub mod fuzzy;
pub mod recent;
pub mod search;
pub mod symbols;
pub mod tree;
pub mod watcher;

pub use fuzzy::{FileIndex, FileMatch, score};
pub use recent::RecentFiles;
pub use search::{
    CancelToken, Match, ReplaceItem, ReplaceReport, SearchOptions, apply_replacements, find_in_files,
    find_in_files_collect, replace_preview,
};
pub use symbols::{Span, Symbol, SymbolIndex, SymbolKind, SymbolMatch};
pub use tree::{Entry, FileTree, Row, icon_for, icon_for_entry};
pub use watcher::{FsEvent, FsEventKind, Watcher};

use std::path::Path;

/// Directory names that are never listed, watched, searched or indexed.
pub const ALWAYS_HIDDEN: &[&str] = &[".git", ".tandem", "target"];
/// Directories that are shown in the tree (collapsed, greyed) but never watched, searched or indexed.
pub const COLLAPSED_DIRS: &[&str] = &["node_modules"];

/// True if `name` is in the always-hidden set.
pub fn is_always_hidden_name(name: &str) -> bool {
    ALWAYS_HIDDEN.contains(&name)
}

/// True if `name` must be skipped by watcher / search / indexes (hidden set + node_modules).
pub fn is_excluded_name(name: &str) -> bool {
    is_always_hidden_name(name) || COLLAPSED_DIRS.contains(&name)
}

/// True if any component of `path` below `root` is excluded (see [`is_excluded_name`]).
pub fn is_excluded_path(root: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().any(|c| c.as_os_str().to_str().is_some_and(is_excluded_name))
}

/// The one directory walker every module uses: honours `.gitignore`/`.ignore` (even outside a
/// git repo, not the global gitignore), includes dotfiles, never follows symlinks and skips
/// [`is_excluded_name`] directories below `dir`.
pub(crate) fn walker(dir: &Path) -> ignore::WalkBuilder {
    let mut wb = ignore::WalkBuilder::new(dir);
    wb.hidden(false).require_git(false).git_global(false).follow_links(false);
    wb.filter_entry(|e| e.depth() == 0 || e.file_name().to_str().is_none_or(|n| !is_excluded_name(n)));
    wb
}
