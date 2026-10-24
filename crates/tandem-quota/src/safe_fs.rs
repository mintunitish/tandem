//! The only place in this crate that opens files for reading.
//!
//! [`safe_open`] refuses vendor credential stores (and anything that resolves to one
//! through a symlink). Every reader in the crate goes through these helpers; a test
//! scans the sources to keep it that way.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

/// Credential locations relative to `$HOME`. Never opened, never watched.
pub const FORBIDDEN_HOME_FILES: &[&str] = &[
    ".claude/.credentials.json",
    ".codex/auth.json",
    ".gemini/oauth_creds.json",
];

/// Credential directories relative to `$HOME` (macOS Keychain).
pub const FORBIDDEN_HOME_DIRS: &[&str] = &["Library/Keychains"];

/// File names refused anywhere, whatever the parent directory.
const FORBIDDEN_NAMES: &[&str] = &[".credentials.json", "auth.json", "oauth_creds.json"];

#[derive(Debug, thiserror::Error)]
#[error("refusing to open credential path {0}")]
pub struct ForbiddenPath(pub PathBuf);

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn matches_forbidden(p: &Path, home: Option<&Path>) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if FORBIDDEN_NAMES.contains(&name) {
        return true;
    }
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.starts_with("keychain") {
        return true;
    }
    if let Some(home) = home
        && let Ok(rel) = p.strip_prefix(home) {
            if FORBIDDEN_HOME_FILES.iter().any(|f| rel == Path::new(f)) {
                return true;
            }
            if FORBIDDEN_HOME_DIRS.iter().any(|d| rel.starts_with(d)) {
                return true;
            }
        }
    // Keychains outside $HOME (system keychains).
    p.components().any(|c| c.as_os_str() == "Keychains")
}

/// True when `path` (lexically, or after resolving symlinks) is a credential store.
pub fn is_forbidden(path: &Path) -> bool {
    let home = dirs::home_dir();
    let home = home.as_deref();
    if matches_forbidden(&lexical_normalize(path), home) {
        return true;
    }
    if let Ok(real) = std::fs::canonicalize(path) {
        let real_home = home.and_then(|h| std::fs::canonicalize(h).ok());
        if matches_forbidden(&real, real_home.as_deref().or(home)) {
            return true;
        }
    }
    false
}

/// Checks `path` and returns it unchanged when allowed.
pub fn check(path: &Path) -> io::Result<&Path> {
    if is_forbidden(path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            ForbiddenPath(path.to_path_buf()),
        ));
    }
    Ok(path)
}

/// Opens a file for reading, refusing credential stores.
pub fn safe_open(path: &Path) -> io::Result<File> {
    File::open(check(path)?)
}

pub fn safe_read_to_string(path: &Path) -> io::Result<String> {
    let mut s = String::new();
    safe_open(path)?.read_to_string(&mut s)?;
    Ok(s)
}

/// Reads at most the last `max_bytes` of a file, starting at a line boundary.
pub fn safe_read_tail(path: &Path, max_bytes: u64) -> io::Result<String> {
    let mut f = safe_open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        match s.find('\n') {
            Some(i) => s.drain(..=i),
            None => s.drain(..),
        };
    }
    Ok(s)
}

/// Recursively lists files with the given extension, skipping forbidden paths.
pub fn walk_files(root: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if is_forbidden(&p) {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk_files(&p, ext, out);
        } else if ft.is_file() && p.extension().and_then(|x| x.to_str()) == Some(ext) {
            out.push(p);
        }
    }
}

/// Atomically writes `bytes` to `path` (temp file in the same dir + rename), mode 0600.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    check(path)?;
    tandem_core::fs::atomic_write_mode(path, bytes, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_credential_paths() {
        let home = dirs::home_dir().unwrap();
        for f in FORBIDDEN_HOME_FILES {
            assert!(is_forbidden(&home.join(f)), "{f}");
            assert!(safe_open(&home.join(f)).is_err());
        }
        assert!(is_forbidden(&home.join("Library/Keychains/login.keychain-db")));
        assert!(is_forbidden(&home.join(".codex/sessions/../auth.json")));
        assert!(is_forbidden(Path::new("/anywhere/oauth_creds.json")));
        assert!(!is_forbidden(&home.join(".codex/sessions/2026/x.jsonl")));
        assert!(!is_forbidden(&home.join(".claude/projects/a/b.jsonl")));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_to_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("auth.json");
        std::fs::write(&target, "{}").unwrap();
        let link = dir.path().join("innocent.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(safe_open(&link).is_err());
    }

    #[test]
    fn tail_starts_at_line_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.jsonl");
        std::fs::write(&p, "aaaa\nbbbb\ncccc\n").unwrap();
        assert_eq!(safe_read_tail(&p, 7).unwrap(), "cccc\n");
        assert_eq!(safe_read_tail(&p, 100).unwrap(), "aaaa\nbbbb\ncccc\n");
    }
}
