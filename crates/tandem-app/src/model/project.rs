//! In-crate stand-in for `tandem-project`: lazy directory listing for the Project tree and a
//! flat file index for Search Everywhere. Swap these two functions for the backend crate later.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::{Path, PathBuf};

/// Names never shown in the tree.
pub fn is_always_hidden_name(name: &str) -> bool {
    matches!(name, ".git" | ".tandem" | ".DS_Store")
}

/// Directories shown, but collapsed and dimmed ("excluded" in JetBrains terms).
pub fn is_excluded_name(name: &str) -> bool {
    matches!(name, "node_modules" | "target" | "dist" | "build" | ".idea" | ".next")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// Gitignored or a well-known build/output dir. Rendered dimmed.
    pub excluded: bool,
}

/// Root-level `.gitignore` matcher, built once per project.
pub struct IgnoreRules {
    gitignore: Gitignore,
}

impl IgnoreRules {
    pub fn new(root: &Path) -> Self {
        let mut b = GitignoreBuilder::new(root);
        b.add(root.join(".gitignore"));
        let gitignore = b.build().unwrap_or_else(|_| Gitignore::empty());
        Self { gitignore }
    }

    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.gitignore.matched_path_or_any_parents(path, is_dir).is_ignore()
    }
}

/// List one directory: directories first, then files, both case-insensitively sorted.
/// Integration point: `tandem_project::tree::list_dir(root, dir)`.
pub fn list_dir(dir: &Path, rules: &IgnoreRules) -> Vec<DirEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<DirEntry> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if is_always_hidden_name(&name) {
                return None;
            }
            let path = e.path();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let excluded = (is_dir && is_excluded_name(&name)) || rules.is_ignored(&path, is_dir);
            Some(DirEntry { path, name, is_dir, excluded })
        })
        .collect();
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// Every non-ignored file under `root`, relative paths. Runs on a background thread.
/// Integration point: `tandem_project::fuzzy::FileIndex::build(root)`.
pub fn index_files(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !is_always_hidden_name(&name) && !is_excluded_name(&name)
        })
        .build();
    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|t| t.is_file()) {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                files.push(rel.to_path_buf());
            }
            if files.len() >= limit {
                break;
            }
        }
    }
    files.sort();
    files
}

/// Current branch from `.git/HEAD` (cheap, no git2). Integration point: `tandem_git::current_branch`.
pub fn current_branch(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(root.join(".git/HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(b) => b.to_string(),
        None => head.chars().take(8).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirs_first_and_hidden_names() {
        let tmp = std::env::temp_dir().join(format!("tandem-app-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("b_dir")).unwrap();
        std::fs::create_dir_all(tmp.join(".git")).unwrap();
        std::fs::create_dir_all(tmp.join("node_modules")).unwrap();
        std::fs::write(tmp.join("a.rs"), "").unwrap();
        let rules = IgnoreRules::new(&tmp);
        let entries = list_dir(&tmp, &rules);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["b_dir", "node_modules", "a.rs"]);
        assert!(entries[1].excluded);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
