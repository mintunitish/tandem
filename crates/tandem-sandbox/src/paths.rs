//! Path normalization and the built-in protected (secret) path list shared by the
//! classifier and the Seatbelt profile generator.

use std::path::{Component, Path, PathBuf};

/// Home-relative paths agents may never read or write (credentials, tokens, other
/// agents' logins). Directories match their whole subtree.
pub const HOME_PROTECTED: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".config/gcloud",
    ".kube",
    ".docker/config.json",
    ".npmrc",
    ".netrc",
    ".claude",
    ".claude.json",
    ".codex",
    ".gemini",
    ".config/goose",
    ".config/gh",
    ".git-credentials",
    ".pypirc",
    ".azure",
    ".vault-token",
    ".password-store",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".gem/credentials",
    ".local/share/keyrings",
    "Library/Keychains",
];

/// Absolute paths that are always protected.
pub const ABS_PROTECTED: &[&str] = &["/Library/Keychains", "/private/var/db/SystemKey"];

/// Basename rules (case-insensitive): `prefix*` or `*suffix`.
pub const NAME_PREFIX_PROTECTED: &[&str] = &[".env", "id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
pub const NAME_SUFFIX_PROTECTED: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".keychain", ".keychain-db"];

/// Lexically normalize: resolve `.` and `..` without touching the filesystem.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() && !out.has_root() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() { PathBuf::from(".") } else { out }
}

fn lower(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

/// Case-insensitive (APFS default) "is `p` equal to or under `root`".
pub fn is_under(p: &Path, root: &Path) -> bool {
    let p = lower(p);
    let r = lower(root);
    let r = r.trim_end_matches('/');
    if r.is_empty() {
        return p.starts_with('/');
    }
    p == r || (p.starts_with(r) && p.as_bytes().get(r.len()) == Some(&b'/'))
}

/// The forms a path may take: lexical, plus the symlink-resolved one when the path
/// (or its parent) exists. Callers evaluate every form and keep the worst result,
/// which defeats `ln -s ~/.ssh/id_rsa x; cat x` when the link already exists.
pub fn forms(p: &Path) -> Vec<PathBuf> {
    let n = normalize(p);
    let mut v = vec![n.clone()];
    if let Ok(c) = std::fs::canonicalize(&n) {
        if c != n {
            v.push(c);
        }
    } else if let (Some(parent), Some(name)) = (n.parent(), n.file_name())
        && let Ok(c) = std::fs::canonicalize(parent)
    {
        let c = c.join(name);
        if c != n {
            v.push(c);
        }
    }
    v
}

/// A root, plus its canonical spelling (e.g. `/tmp/x` and `/private/tmp/x`).
#[derive(Clone, Debug)]
pub struct Root(pub Vec<PathBuf>);

impl Root {
    pub fn new(p: &Path) -> Self {
        let n = normalize(p);
        let mut v = vec![n.clone()];
        if let Ok(c) = std::fs::canonicalize(&n)
            && c != n
        {
            v.push(c);
        }
        Root(v)
    }
    pub fn contains(&self, p: &Path) -> bool {
        self.0.iter().any(|r| is_under(p, r))
    }
    pub fn primary(&self) -> &Path {
        &self.0[0]
    }
}

/// Protected-path matcher: built-ins + `Policy.protected_paths`.
#[derive(Clone, Debug)]
pub struct Protected {
    dirs: Vec<PathBuf>,
    name_globs: Vec<String>,
}

impl Protected {
    /// `extra` entries: `~/x` and absolute paths protect a subtree; relative paths
    /// containing `/` are relative to `project_root`; bare names are basename globs
    /// supporting `*` (e.g. `*.secret`, `credentials.json`).
    pub fn new(home: &Path, project_root: &Path, extra: &[String]) -> Self {
        let mut dirs: Vec<PathBuf> = HOME_PROTECTED.iter().map(|r| home.join(r)).collect();
        dirs.extend(ABS_PROTECTED.iter().map(PathBuf::from));
        let mut name_globs = Vec::new();
        for e in extra {
            let e = e.trim();
            if e.is_empty() {
                continue;
            }
            if let Some(rest) = e.strip_prefix("~/") {
                dirs.push(home.join(rest));
            } else if e == "~" {
                dirs.push(home.to_path_buf());
            } else if e.starts_with('/') {
                dirs.push(PathBuf::from(e));
            } else if e.contains('/') {
                dirs.push(project_root.join(e));
            } else {
                name_globs.push(e.to_lowercase());
            }
        }
        let mut more = Vec::new();
        for d in &dirs {
            for f in forms(d) {
                if !dirs.contains(&f) {
                    more.push(f);
                }
            }
        }
        dirs.extend(more);
        Protected { dirs, name_globs }
    }

    /// Protected subtrees (absolute), for the Seatbelt generator.
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }
    pub fn name_globs(&self) -> &[String] {
        &self.name_globs
    }

    pub fn matches(&self, p: &Path) -> bool {
        forms(p).iter().any(|f| self.matches_one(f))
    }

    fn matches_one(&self, p: &Path) -> bool {
        if self.dirs.iter().any(|d| is_under(p, d)) {
            return true;
        }
        // Basename rules apply to every component, so `.env/x` and `keys.pem/..` count.
        p.components().any(|c| match c {
            Component::Normal(n) => protected_name(&n.to_string_lossy(), &self.name_globs),
            _ => false,
        })
    }
}

/// Public CA bundles that match `*.pem` but hold no secrets; TLS breaks without them
/// (`/etc/ssl/cert.pem` for curl, `certifi/cacert.pem` for Python).
pub const PUBLIC_PEM_NAMES: &[&str] = &["cacert.pem", "cert.pem", "ca-bundle.pem", "ca-certificates.pem", "tls-ca-bundle.pem"];

/// Built-in basename rules plus user globs.
pub fn protected_name(name: &str, globs: &[String]) -> bool {
    let n = name.to_lowercase();
    if PUBLIC_PEM_NAMES.contains(&n.as_str()) {
        return globs.iter().any(|g| glob_match(g, &n));
    }
    NAME_PREFIX_PROTECTED.iter().any(|p| n.starts_with(p))
        || NAME_SUFFIX_PROTECTED.iter().any(|s| n.ends_with(s))
        || globs.iter().any(|g| glob_match(g, &n))
}

/// Minimal `*` glob (no `?`, no classes).
pub fn glob_match(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(idx) => rest = &rest[idx + part.len()..],
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_works() {
        assert_eq!(normalize(Path::new("/a/b/../c/./d")), PathBuf::from("/a/c/d"));
        assert_eq!(normalize(Path::new("/../..")), PathBuf::from("/"));
    }

    #[test]
    fn protected_matching() {
        let p = Protected::new(Path::new("/Users/u"), Path::new("/proj"), &["*.secret".into(), "conf/creds".into()]);
        assert!(p.matches(Path::new("/Users/u/.ssh/id_rsa")));
        assert!(p.matches(Path::new("/Users/u/.SSH/config")));
        assert!(p.matches(Path::new("/proj/.env")));
        assert!(p.matches(Path::new("/proj/.env.local")));
        assert!(p.matches(Path::new("/proj/certs/server.PEM")));
        assert!(p.matches(Path::new("/proj/x.secret")));
        assert!(p.matches(Path::new("/proj/conf/creds/a")));
        assert!(p.matches(Path::new("/Users/u/Library/Keychains/login.keychain-db")));
        assert!(!p.matches(Path::new("/proj/src/main.rs")));
        assert!(!p.matches(Path::new("/Users/u/.sshx")));
        assert!(!p.matches(Path::new("/proj/environment.rs")));
        assert!(!p.matches(Path::new("/private/etc/ssl/cert.pem")));
        assert!(!p.matches(Path::new("/proj/.venv/lib/site-packages/certifi/cacert.pem")));
        assert!(p.matches(Path::new("/Users/u/.ssh/cert.pem")));
    }

    #[test]
    fn glob() {
        assert!(glob_match("*.secret", "a.secret"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b", "ac"));
        assert!(glob_match("x", "x"));
    }
}
