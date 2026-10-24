//! Small filesystem helpers shared by every crate: atomic writes, home dirs, PATH lookup.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Atomically replaces `path` with `bytes`: writes a uniquely named temp file in the same
/// directory, fsyncs it, then renames it over the target. Creates the parent directory.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_impl(path, bytes, None)
}

/// [`atomic_write`] with an explicit file mode (unix permission bits; ignored elsewhere).
pub fn atomic_write_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_impl(path, bytes, Some(mode))
}

fn write_impl(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("out");
    let tmp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let res = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(m) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(m);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        #[cfg(unix)]
        if let Some(m) = mode {
            // `mode` on open is filtered by the umask; set the exact bits.
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(m))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// The user's home directory (`$HOME`), or `/var/empty` when unset.
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/empty"))
}

/// Tandem's per-user state dir: `$TANDEM_HOME`, else `~/.tandem`.
pub fn tandem_home() -> PathBuf {
    std::env::var_os("TANDEM_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".tandem"))
}

/// Resolves an executable name against a PATH value. Names containing `/` are checked directly.
pub fn which_in(cmd: &str, path_var: &std::ffi::OsStr) -> Option<PathBuf> {
    if cmd.contains('/') {
        let p = PathBuf::from(cmd);
        return is_executable(&p).then_some(p);
    }
    std::env::split_paths(path_var).map(|d| d.join(cmd)).find(|p| is_executable(p))
}

/// True for a regular file with any execute bit set (any regular file off unix).
pub fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_sets_mode() {
        let dir = std::env::temp_dir().join(format!("tandem-core-fs-{}", uuid::Uuid::new_v4()));
        let p = dir.join("sub/a.json");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            atomic_write_mode(&p, b"three", 0o600).unwrap();
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let leftovers = std::fs::read_dir(p.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
