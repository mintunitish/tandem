//! Agent Blame lookups over `<root>/.tandem/blame.json` (written by Review → Apply accepted).
//!
//! ```ignore
//! if let Some(e) = tandem_app::tasks::blame::lookup(&root, "src/lib.rs", 42) {
//!     println!("{} via task {}: {}", e.agent, e.task, e.prompt_excerpt);
//! }
//! ```

use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tandem_core::BlameEntry;
use tandem_review::BlameIndex;

type Cache = Option<(PathBuf, Option<SystemTime>, BlameIndex)>;
static CACHE: Mutex<Cache> = Mutex::new(None);

/// Blame entry covering 1-based `line` of `path` (absolute or relative to `root`).
/// Cached per file mtime, so it is cheap enough for a gutter.
pub fn lookup(root: &Path, path: impl AsRef<Path>, line: u32) -> Option<BlameEntry> {
    let file = tandem_review::blame::blame_path(root);
    let mtime = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
    let rel = path.as_ref().strip_prefix(root).unwrap_or(path.as_ref()).to_path_buf();
    let mut c = CACHE.lock();
    let fresh = matches!(&*c, Some((p, m, _)) if *p == file && *m == mtime);
    if !fresh {
        let idx = BlameIndex::load_or_default(&file).unwrap_or_default();
        *c = Some((file, mtime, idx));
    }
    c.as_ref().and_then(|(_, _, idx)| idx.lookup(&rel, line).cloned())
}

/// Forget the cached index (after writing blame.json).
pub fn invalidate() {
    *CACHE.lock() = None;
}
