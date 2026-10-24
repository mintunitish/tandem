//! Recent Files (⌘E): a most-recent-first ring persisted at `<root>/.tandem/recent.json`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

pub const DEFAULT_CAPACITY: usize = 100;

#[derive(Serialize, Deserialize)]
struct Disk {
    version: u32,
    /// Project-relative when inside the root, absolute otherwise; most recent first.
    files: Vec<String>,
}

pub struct RecentFiles {
    root: PathBuf,
    capacity: usize,
    items: VecDeque<PathBuf>,
}

impl RecentFiles {
    /// An empty ring (nothing read from disk).
    pub fn new(root: impl Into<PathBuf>, capacity: usize) -> Self {
        Self { root: root.into(), capacity: capacity.max(1), items: VecDeque::new() }
    }

    /// Load `.tandem/recent.json`; a missing or corrupt file yields an empty ring.
    pub fn load(root: impl Into<PathBuf>) -> Self {
        Self::load_with_capacity(root, DEFAULT_CAPACITY)
    }

    pub fn load_with_capacity(root: impl Into<PathBuf>, capacity: usize) -> Self {
        let mut me = Self::new(root, capacity);
        let Ok(data) = std::fs::read_to_string(me.file()) else { return me };
        match serde_json::from_str::<Disk>(&data) {
            Ok(d) => {
                for f in d.files.into_iter().take(me.capacity) {
                    me.items.push_back(me.abs(Path::new(&f)));
                }
            }
            Err(e) => tracing::warn!("ignoring corrupt recent.json: {e}"),
        }
        me
    }

    fn file(&self) -> PathBuf {
        self.root.join(".tandem").join("recent.json")
    }

    fn abs(&self, p: &Path) -> PathBuf {
        if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) }
    }

    /// Mark `path` as just opened (moves it to the front) and persist (best effort).
    pub fn touch(&mut self, path: impl AsRef<Path>) {
        let abs = self.abs(path.as_ref());
        self.items.retain(|x| x != &abs);
        self.items.push_front(abs);
        self.items.truncate(self.capacity);
        if let Err(e) = self.save() {
            tracing::warn!("saving recent files failed: {e:#}");
        }
    }

    pub fn remove(&mut self, path: impl AsRef<Path>) {
        let abs = self.abs(path.as_ref());
        self.items.retain(|x| x != &abs);
        let _ = self.save();
    }

    /// Most recent first (absolute paths).
    pub fn list(&self) -> Vec<PathBuf> {
        self.items.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Write atomically (temp file + rename).
    pub fn save(&self) -> Result<()> {
        let file = self.file();
        let files = self
            .items
            .iter()
            .map(|p| p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().into_owned())
            .collect();
        let json = serde_json::to_string_pretty(&Disk { version: 1, files })?;
        tandem_core::fs::atomic_write(&file, json.as_bytes(), true)?;
        Ok(())
    }
}
