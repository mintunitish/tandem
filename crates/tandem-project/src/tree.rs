//! Lazily loaded project tree, WebStorm ordering (directories first, case-insensitive).

use crate::watcher::{FsEvent, FsEventKind};
use crate::COLLAPSED_DIRS;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One row of a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// Shown greyed out: matched by an ignore file but always visible (e.g. `node_modules`).
    pub ignored: bool,
    /// Icon key for the closed/file state; see [`icon_for_entry`].
    pub icon: &'static str,
}

/// A flattened visible row for rendering the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub entry: Entry,
    pub depth: usize,
    pub expanded: bool,
}

pub struct FileTree {
    root: PathBuf,
    loaded: HashMap<PathBuf, Vec<Entry>>,
    expanded: HashSet<PathBuf>,
}

impl FileTree {
    /// Create a tree rooted at `root`. The root is loaded and expanded immediately.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut t = Self { root: root.clone(), loaded: HashMap::new(), expanded: HashSet::new() };
        t.expand(&root);
        t
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Load (if needed) and mark `path` expanded. Returns its children.
    pub fn expand(&mut self, path: &Path) -> &[Entry] {
        self.expanded.insert(path.to_path_buf());
        self.children(path)
    }

    pub fn collapse(&mut self, path: &Path) {
        self.expanded.remove(path);
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Children of `path`, loading them on first access. Unreadable directories yield `[]`.
    pub fn children(&mut self, path: &Path) -> &[Entry] {
        if !self.loaded.contains_key(path) {
            let list = list_dir(path);
            self.loaded.insert(path.to_path_buf(), list);
        }
        &self.loaded[path]
    }

    /// Children if `path` was already loaded (no I/O).
    pub fn loaded_children(&self, path: &Path) -> Option<&[Entry]> {
        self.loaded.get(path).map(Vec::as_slice)
    }

    /// Re-read `path` from disk. Directories that vanished are dropped with their subtree.
    pub fn refresh(&mut self, path: &Path) {
        if !path.is_dir() {
            self.loaded.retain(|k, _| !k.starts_with(path));
            self.expanded.retain(|k| !k.starts_with(path));
            return;
        }
        let list = list_dir(path);
        // Drop state of subdirectories that no longer exist.
        let dirs: HashSet<&Path> = list.iter().filter(|e| e.is_dir).map(|e| e.path.as_path()).collect();
        self.loaded.retain(|k, _| k == path || !k.starts_with(path) || is_under_any(k, path, &dirs));
        self.expanded.retain(|k| k == path || !k.starts_with(path) || is_under_any(k, path, &dirs));
        self.loaded.insert(path.to_path_buf(), list);
    }

    /// Consume a watcher event. Returns the directories that were refreshed.
    pub fn apply_event(&mut self, ev: &FsEvent) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if ev.kind == FsEventKind::Modified {
            // Content changes do not alter listings (ignore-file edits do, handled below).
            if !ev.paths.iter().any(|p| is_ignore_file(p)) {
                return Vec::new();
            }
        }
        for p in &ev.paths {
            if is_ignore_file(p) {
                // Ignore rules changed: every loaded directory may list differently.
                dirs.extend(self.loaded.keys().cloned());
                continue;
            }
            if let Some(parent) = p.parent() {
                dirs.push(parent.to_path_buf());
            }
            if self.loaded.contains_key(p.as_path()) {
                dirs.push(p.clone());
            }
        }
        dirs.sort();
        dirs.dedup();
        let mut refreshed = Vec::new();
        for d in dirs {
            if self.loaded.contains_key(&d) {
                self.refresh(&d);
                refreshed.push(d);
            }
        }
        refreshed
    }

    /// Flatten the expanded part of the tree into render rows (root's children at depth 0).
    pub fn visible_rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        self.walk_rows(&self.root, 0, &mut out);
        out
    }

    fn walk_rows(&self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        let Some(children) = self.loaded.get(dir) else { return };
        for e in children {
            let expanded = e.is_dir && self.expanded.contains(&e.path);
            out.push(Row { entry: e.clone(), depth, expanded });
            if expanded {
                self.walk_rows(&e.path, depth + 1, out);
            }
        }
    }
}

fn is_under_any(k: &Path, base: &Path, dirs: &HashSet<&Path>) -> bool {
    // `k` lives below `base`; keep it if its first component below base is a live directory.
    let Ok(rel) = k.strip_prefix(base) else { return true };
    match rel.components().next() {
        Some(c) => dirs.contains(base.join(c.as_os_str()).as_path()),
        None => true,
    }
}

fn sort_key(name: &str) -> String {
    name.to_lowercase()
}

/// List one directory honoring `.gitignore`/`.ignore` and the always-hidden set.
fn list_dir(dir: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // The walker drops excluded names; collapsed dirs are re-added below.
    for dent in crate::walker(dir).max_depth(Some(1)).build().flatten() {
        if dent.depth() == 0 {
            continue;
        }
        let Some(name) = dent.file_name().to_str() else { continue };
        let path = dent.path().to_path_buf();
        let is_dir = is_dir_following(&path, dent.file_type().is_some_and(|t| t.is_dir()));
        seen.insert(name.to_string());
        out.push(Entry { name: name.to_string(), icon: icon_for_entry(name, is_dir, false), path, is_dir, ignored: false });
    }
    // Collapsed-but-visible directories that an ignore file removed from the walk.
    for name in COLLAPSED_DIRS {
        let p = dir.join(name);
        if !seen.contains(*name) && p.is_dir() {
            out.push(Entry { name: (*name).to_string(), icon: "folder", path: p, is_dir: true, ignored: true });
        }
    }
    for e in &mut out {
        if e.is_dir && COLLAPSED_DIRS.contains(&e.name.as_str()) {
            e.ignored = true;
        }
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| sort_key(&a.name).cmp(&sort_key(&b.name)))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

fn is_dir_following(path: &Path, is_dir: bool) -> bool {
    is_dir || (path.is_symlink() && std::fs::metadata(path).is_ok_and(|m| m.is_dir()))
}

fn is_ignore_file(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == ".gitignore" || n == ".ignore")
}

/// Icon key for a path. Directories give `"folder"`; for open state use [`icon_for_entry`].
pub fn icon_for(path: &Path) -> &'static str {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    icon_for_entry(name, path.is_dir(), false)
}

/// Icon key from a file name (pure; no I/O).
///
/// Keys: `ts tsx js jsx rs py go json md html css sh git lock image config folder folder-open file`.
pub fn icon_for_entry(name: &str, is_dir: bool, open: bool) -> &'static str {
    if is_dir {
        return match (name, open) {
            (".git", _) => "git",
            (_, true) => "folder-open",
            _ => "folder",
        };
    }
    let lower = name.to_ascii_lowercase();
    if lower.starts_with(".git") && lower != ".github" {
        return "git";
    }
    if lower.ends_with(".lock")
        || matches!(lower.as_str(), "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "bun.lockb" | "go.sum" | "poetry.lock")
    {
        return "lock";
    }
    if lower == ".env" || lower.starts_with(".env.") || lower == ".editorconfig" || lower == "dockerfile" {
        return "config";
    }
    if lower.contains(".config.") || lower.ends_with("rc") && lower.starts_with('.') {
        return "config";
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "ts" | "mts" | "cts" => "ts",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "js",
        "jsx" => "jsx",
        "rs" => "rs",
        "py" | "pyi" => "py",
        "go" | "mod" => "go",
        "json" | "jsonc" | "json5" => "json",
        "md" | "mdx" | "markdown" => "md",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" | "less" => "css",
        "sh" | "bash" | "zsh" => "sh",
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" | "avif" => "image",
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "env" | "properties" => "config",
        _ => "file",
    }
}
