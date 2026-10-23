//! Guard: every file read in this crate goes through `safe_fs`, which refuses credential
//! stores; and no source mentions vendor OAuth usage endpoints.

use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn reads_only_through_safe_fs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    sources(&root.join("examples"), &mut files);
    let banned = ["File::open", "fs::read(", "fs::read_to_string", "read_to_string(\"", "Connection::open(", "OpenOptions::new().read"];
    let endpoints = ["api/oauth/usage", "retrieveUserQuota", "cloudcode-pa.googleapis", "security find-generic-password"];
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let is_safe_fs = f.ends_with("src/safe_fs.rs");
        for b in banned {
            assert!(is_safe_fs || !text.contains(b), "{} uses {b}; use safe_fs instead", f.display());
        }
        for e in endpoints {
            assert!(!text.contains(e), "{} references vendor usage endpoint {e}", f.display());
        }
    }
}

#[test]
fn credential_paths_are_refused() {
    let home = dirs::home_dir().unwrap();
    for rel in [".claude/.credentials.json", ".codex/auth.json", ".gemini/oauth_creds.json", "Library/Keychains/login.keychain-db"] {
        let p = home.join(rel);
        assert!(tandem_quota::safe_fs::is_forbidden(&p), "{rel}");
        let err = tandem_quota::safe_fs::safe_open(&p).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{rel}");
        assert!(tandem_quota::safe_fs::safe_read_tail(&p, 10).is_err());
    }
    // Walking ~/.codex never yields auth.json.
    let mut found = Vec::new();
    tandem_quota::safe_fs::walk_files(&home.join(".codex"), "json", &mut found);
    assert!(found.iter().all(|p| !p.ends_with("auth.json")));
}
