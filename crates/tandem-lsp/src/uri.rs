//! `file://` URI <-> filesystem path conversion with percent-encoding.

use crate::LspError;
use lsp_types::Uri;
use std::path::{Path, PathBuf};
use std::str::FromStr;

fn keep(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/')
}

/// Absolute path -> `file:///...` URI.
pub fn path_to_uri(path: &Path) -> Result<Uri, LspError> {
    if !path.is_absolute() {
        return Err(LspError::InvalidPath(path.to_path_buf()));
    }
    #[cfg(unix)]
    let raw: Vec<u8> = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let raw: Vec<u8> = {
        let s = path.to_str().ok_or_else(|| LspError::InvalidPath(path.to_path_buf()))?;
        let s = s.replace('\\', "/");
        let s = if s.starts_with('/') { s } else { format!("/{s}") };
        s.into_bytes()
    };
    let mut out = String::from("file://");
    for b in raw {
        if keep(b) || (cfg!(not(unix)) && b == b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Uri::from_str(&out).map_err(|_| LspError::InvalidUri(out))
}

/// `file://` URI -> path. Returns `None` for other schemes or malformed escapes.
pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    uri_str_to_path(uri.as_str())
}

pub fn uri_str_to_path(s: &str) -> Option<PathBuf> {
    let rest = s.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }
    #[cfg(not(unix))]
    {
        let s = String::from_utf8(out).ok()?;
        let s = s.strip_prefix('/').unwrap_or(&s).replace('/', "\\");
        Some(PathBuf::from(s))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for p in ["/tmp/a.ts", "/Users/me/My Project/src/ä 漢字#1.rs", "/x/100%/y?.py"] {
            let uri = path_to_uri(Path::new(p)).expect("uri");
            assert!(uri.as_str().starts_with("file:///"));
            assert_eq!(uri_to_path(&uri).expect("path"), PathBuf::from(p));
        }
        assert_eq!(
            path_to_uri(Path::new("/a b/c")).expect("uri").as_str(),
            "file:///a%20b/c"
        );
        assert!(path_to_uri(Path::new("rel/x")).is_err());
        assert_eq!(uri_str_to_path("file://localhost/a/b"), Some(PathBuf::from("/a/b")));
        assert_eq!(uri_str_to_path("untitled:foo"), None);
    }
}
