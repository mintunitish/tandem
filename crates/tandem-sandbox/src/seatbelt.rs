//! macOS Seatbelt (`sandbox-exec`) profile generation.
//!
//! The profile is deny-by-default and mirrors two proven, open-source profiles:
//!
//! - OpenAI Codex CLI, `codex-rs/sandboxing/src/seatbelt_base_policy.sbpl` and
//!   `seatbelt_network_policy.sbpl` (itself derived from Chromium's
//!   `sandbox/policy/mac/common.sb`): process/sysctl/mach/PTY rules.
//!   <https://github.com/openai/codex/tree/main/codex-rs/sandboxing/src>
//! - Anthropic `sandbox-runtime`, `src/sandbox/macos-sandbox-utils.ts`: the mach-lookup
//!   allow-list, IOKit/IPC rules, "deny after allow" ordering (Seatbelt is
//!   last-match-wins), directory-only `file-read-metadata`, and the mandatory write
//!   denies for `.git/hooks`, `.git/config`, `.vscode`, `.idea`, `.mcp.json`,
//!   `.claude/{commands,agents}`.
//!   <https://github.com/anthropics/sandbox-runtime/blob/main/src/sandbox/macos-sandbox-utils.ts>
//!
//! Differences from both: reads are *not* globally allowed. Only system trees, the
//! project, toolchains/caches and the agent's own config dir are readable; then the
//! protected list (~/.ssh, ~/.aws, `.env*`, `*.pem`, other agents' logins, keychains...)
//! is denied on top, so it stays unreadable even under an allowed tree.

use crate::paths::{self, NAME_PREFIX_PROTECTED, NAME_SUFFIX_PROTECTED, Protected};
use std::path::{Path, PathBuf};

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Everything the profile generator needs. Build with [`SandboxSpec::new`] and adjust.
#[derive(Clone, Debug)]
pub struct SandboxSpec {
    pub project_root: PathBuf,
    pub worktree_root: PathBuf,
    pub home: PathBuf,
    /// The agent's own config dirs (e.g. `~/.codex` for codex, `~/.claude` for claude).
    /// Readable and writable, and exempted from the protected list.
    pub agent_home_dirs: Vec<PathBuf>,
    /// Writable scratch dirs (`/tmp`, `$TMPDIR`, the Darwin user cache dir).
    pub temp_dirs: Vec<PathBuf>,
    /// Extra read-only roots (system trees and toolchains are pre-filled).
    pub read_roots: Vec<PathBuf>,
    /// Extra writable roots (package caches are pre-filled).
    pub write_roots: Vec<PathBuf>,
    /// The repository's git dir (`<project>/.git`) so commits from the worktree work.
    /// Only `objects/`, `worktrees/<task_worktree>/`, `refs/heads/tandem/`,
    /// `logs/refs/heads/tandem/` and `packed-refs.lock` are writable; hooks, config, the
    /// main index and every other ref stay read-only. `None` keeps it all read-only.
    pub git_common_dir: Option<PathBuf>,
    /// Name of the task's git worktree (`<git_common_dir>/worktrees/<name>`), normally
    /// the worktree directory's name. `None` (the worktree is the project) grants no
    /// per-worktree admin dir.
    pub task_worktree: Option<String>,
    /// `Policy.protected_paths`.
    pub protected_extra: Vec<String>,
    /// Allow outbound network (agents need their vendor API).
    pub network: bool,
}

const SYSTEM_READ: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/opt/homebrew",
    "/usr/local",
    "/Library",
    "/System",
    "/Applications/Xcode.app",
    "/private/etc",
    "/private/var/db/timezone",
    "/private/var/select",
    "/nix/store",
];

/// Home-relative toolchain and config locations CLIs read.
const HOME_READ: &[&str] = &[
    ".gitconfig",
    ".config/git",
    ".npm",
    ".cache",
    "Library/Caches",
    ".cargo",
    ".rustup",
    ".nvm",
    ".bun",
    ".deno",
    ".local",
    ".volta",
    ".asdf",
    ".pyenv",
    ".yarn",
    ".yarnrc",
    ".yarnrc.yml",
    "Library/pnpm",
    "go",
    ".gradle",
    ".m2",
];

/// Home-relative caches CLIs write.
const HOME_WRITE: &[&str] = &[
    ".npm",
    ".cache",
    "Library/Caches",
    ".cargo/registry",
    ".cargo/git",
    ".bun/install/cache",
    "Library/pnpm",
    ".local/share/pnpm",
    ".yarn/berry/cache",
    "go/pkg/mod",
    ".gradle/caches",
    ".m2/repository",
];

impl SandboxSpec {
    /// Defaults: system + toolchain reads, cache writes, `/tmp` and `$TMPDIR`,
    /// network on, `<project>/.git` writable (minus hooks/config) if it exists.
    pub fn new(project_root: impl Into<PathBuf>, worktree_root: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        let project_root = project_root.into();
        let home = home.into();
        let mut temp_dirs = vec![PathBuf::from("/tmp"), PathBuf::from("/private/tmp")];
        if let Some(t) = std::env::var_os("TMPDIR") {
            let t = PathBuf::from(t);
            // Darwin per-user dirs: .../T (temp) and its sibling .../C (cache).
            if t.file_name().is_some_and(|n| n == "T")
                && let Some(parent) = t.parent()
            {
                temp_dirs.push(parent.join("C"));
            }
            temp_dirs.push(t);
        }
        let git = project_root.join(".git");
        let worktree_root: PathBuf = worktree_root.into();
        let task_worktree = (worktree_root != project_root)
            .then(|| worktree_root.file_name().map(|n| n.to_string_lossy().into_owned()))
            .flatten();
        SandboxSpec {
            git_common_dir: git.is_dir().then_some(git),
            task_worktree,
            project_root,
            worktree_root,
            agent_home_dirs: vec![],
            temp_dirs,
            read_roots: SYSTEM_READ.iter().map(PathBuf::from).chain(HOME_READ.iter().map(|r| home.join(r))).collect(),
            write_roots: HOME_WRITE.iter().map(|r| home.join(r)).collect(),
            home,
            protected_extra: vec![],
            network: true,
        }
    }

    /// Paths inside [`SandboxSpec::git_common_dir`] a task may write: what `git commit` on
    /// the task's `tandem/<id>` branch touches, nothing that runs code or moves other refs.
    pub fn git_write_paths(&self) -> Vec<PathBuf> {
        let Some(g) = &self.git_common_dir else { return vec![] };
        let mut v = vec![g.join("objects"), g.join("refs/heads/tandem"), g.join("logs/refs/heads/tandem")];
        if let Some(name) = self.task_worktree.as_deref().filter(|n| !n.is_empty() && *n != "." && *n != ".." && !n.contains('/')) {
            v.push(g.join("worktrees").join(name));
        }
        v
    }

    /// The agent's own config dirs (`AgentDescriptor::home_dirs`) under `home`, e.g. `~/.codex`.
    pub fn default_agent_home_dirs(agent: &tandem_core::AgentDescriptor, home: &Path) -> Vec<PathBuf> {
        agent.home_dirs.iter().map(|r| home.join(r)).collect()
    }
}

/// SBPL string literal.
fn q(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                o.push('\\');
                o.push(c);
            }
            '\n' | '\r' | '\0' => o.push('?'),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Lexical + canonical spellings (`/tmp` vs `/private/tmp`; Seatbelt sees real paths).
fn spellings(p: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for f in paths::forms(p) {
        if !v.contains(&f) {
            v.push(f);
        }
    }
    v
}

/// Case-insensitive regex fragment for a literal (APFS is case-insensitive).
fn ci_regex(lit: &str) -> String {
    let mut o = String::new();
    for c in lit.chars() {
        if c.is_ascii_alphabetic() {
            o.push('[');
            o.push(c.to_ascii_lowercase());
            o.push(c.to_ascii_uppercase());
            o.push(']');
        } else if c == '*' {
            o.push_str("[^/]*");
        } else if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/' {
            o.push(c);
        } else if c == '"' {
            o.push('.');
        } else {
            o.push('\\');
            o.push(c);
        }
    }
    o
}

fn subpaths(ps: &[PathBuf]) -> String {
    let mut out = String::new();
    let mut seen = Vec::new();
    for p in ps {
        for s in spellings(p) {
            if !seen.contains(&s) {
                out.push_str(&format!("\n  (subpath {})", q(&s)));
                seen.push(s);
            }
        }
    }
    out
}

/// Generate the Seatbelt profile.
pub fn profile(spec: &SandboxSpec) -> String {
    let mut p = String::new();
    p.push_str(
        r#"(version 1)
; Tandem agent sandbox. Deny by default.
(deny default)

; --- processes (codex seatbelt_base_policy.sbpl; sandbox-runtime)
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow mach-priv-task-port (target same-sandbox))
(allow user-preference-read)

; --- mach services (sandbox-runtime allow-list + codex network policy)
(allow mach-lookup
  (global-name "com.apple.audio.systemsoundserver")
  (global-name "com.apple.distributed_notifications@Uv3")
  (global-name "com.apple.FontObjectsServer")
  (global-name "com.apple.fonts")
  (global-name "com.apple.logd")
  (global-name "com.apple.lsd.mapdb")
  (global-name "com.apple.PowerManagement.control")
  (global-name "com.apple.system.logger")
  (global-name "com.apple.system.notification_center")
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.system.opendirectoryd.membership")
  (global-name "com.apple.bsd.dirhelper")
  (global-name "com.apple.securityd.xpc")
  (global-name "com.apple.SecurityServer")
  (global-name "com.apple.coreservices.launchservicesd")
  (global-name "com.apple.cfprefsd.agent")
  (global-name "com.apple.cfprefsd.daemon")
)

(allow ipc-posix-shm)
(allow ipc-posix-sem)
(allow iokit-open
  (iokit-registry-entry-class "IOSurfaceRootUserClient")
  (iokit-registry-entry-class "RootDomainUserClient")
  (iokit-user-client-class "IOSurfaceSendRight")
)
(allow iokit-get-properties)
(allow system-socket (require-all (socket-domain AF_SYSTEM) (socket-protocol 2)))
(allow system-socket (socket-domain AF_UNIX))
(allow distributed-notification-post)

; --- sysctls (codex + sandbox-runtime)
(allow sysctl-read
  (sysctl-name "hw.activecpu") (sysctl-name "hw.busfrequency_compat") (sysctl-name "hw.byteorder")
  (sysctl-name "hw.cacheconfig") (sysctl-name "hw.cachelinesize_compat") (sysctl-name "hw.cpufamily")
  (sysctl-name "hw.cpufrequency") (sysctl-name "hw.cpufrequency_compat") (sysctl-name "hw.cputype")
  (sysctl-name "hw.l1dcachesize_compat") (sysctl-name "hw.l1icachesize_compat")
  (sysctl-name "hw.l2cachesize_compat") (sysctl-name "hw.l3cachesize_compat")
  (sysctl-name "hw.logicalcpu") (sysctl-name "hw.logicalcpu_max") (sysctl-name "hw.machine")
  (sysctl-name "hw.memsize") (sysctl-name "hw.model") (sysctl-name "hw.ncpu") (sysctl-name "hw.nperflevels")
  (sysctl-name "hw.packages") (sysctl-name "hw.pagesize_compat") (sysctl-name "hw.pagesize")
  (sysctl-name "hw.physicalcpu") (sysctl-name "hw.physicalcpu_max") (sysctl-name "hw.tbfrequency_compat")
  (sysctl-name "hw.vectorunit") (sysctl-name "kern.argmax") (sysctl-name "kern.bootargs")
  (sysctl-name "kern.hostname") (sysctl-name "kern.maxfiles") (sysctl-name "kern.maxfilesperproc")
  (sysctl-name "kern.maxproc") (sysctl-name "kern.ngroups") (sysctl-name "kern.osproductversion")
  (sysctl-name "kern.osrelease") (sysctl-name "kern.ostype") (sysctl-name "kern.osvariant_status")
  (sysctl-name "kern.osversion") (sysctl-name "kern.secure_kernel") (sysctl-name "kern.sysv.semmns")
  (sysctl-name "kern.tcsm_available") (sysctl-name "kern.tcsm_enable") (sysctl-name "kern.usrstack64")
  (sysctl-name "kern.version") (sysctl-name "kern.willshutdown") (sysctl-name "machdep.cpu.brand_string")
  (sysctl-name "machdep.ptrauth_enabled") (sysctl-name "security.mac.lockdown_mode_state")
  (sysctl-name "sysctl.proc_cputype") (sysctl-name "vm.loadavg")
  (sysctl-name-prefix "hw.optional.arm") (sysctl-name-prefix "hw.optional.armv8_")
  (sysctl-name-prefix "hw.perflevel") (sysctl-name-prefix "kern.proc.all")
  (sysctl-name-prefix "kern.proc.pgrp.") (sysctl-name-prefix "kern.proc.pid.")
  (sysctl-name-prefix "machdep.cpu.") (sysctl-name-prefix "net.routetable.")
)
(allow sysctl-write (sysctl-name "kern.tcsm_enable") (sysctl-name "kern.grade_cputype"))

; --- terminals and devices (codex: openpty)
(allow pseudo-tty)
(allow file-read* file-write* file-ioctl (literal "/dev/ptmx"))
(allow file-read* file-write* file-ioctl (regex #"^/dev/ttys[0-9]+$"))
(allow file-read* file-write* file-ioctl
  (literal "/dev/null") (literal "/dev/zero") (literal "/dev/tty") (literal "/dev/dtracehelper")
  (literal "/dev/stdin") (literal "/dev/stdout") (literal "/dev/stderr") (subpath "/dev/fd"))
(allow file-read* (literal "/dev/random") (literal "/dev/urandom"))

; --- filesystem: traversal (sandbox-runtime: directories only, never file contents)
(allow file-read* (literal "/"))
(allow file-read-metadata (vnode-type DIRECTORY))
(allow file-read* (literal "/tmp") (literal "/var") (literal "/etc"))
"#,
    );

    // Reads.
    let mut reads: Vec<PathBuf> = spec.read_roots.clone();
    reads.push(spec.project_root.clone());
    reads.push(spec.worktree_root.clone());
    reads.extend(spec.agent_home_dirs.iter().cloned());
    reads.extend(spec.temp_dirs.iter().cloned());
    p.push_str(&format!("\n; --- filesystem: reads\n(allow file-read*{})\n", subpaths(&reads)));

    // Writes.
    let mut writes: Vec<PathBuf> = vec![spec.worktree_root.clone()];
    writes.extend(spec.temp_dirs.iter().cloned());
    writes.extend(spec.agent_home_dirs.iter().cloned());
    writes.extend(spec.write_roots.iter().cloned());
    writes.extend(spec.git_write_paths());
    p.push_str(&format!("\n; --- filesystem: writes\n(allow file-read* file-write*{})\n", subpaths(&writes)));
    if let Some(g) = &spec.git_common_dir {
        let lock = g.join("packed-refs.lock");
        p.push_str(&format!("(allow file-write*{})\n", spellings(&lock).iter().map(|l| format!(" (literal {})", q(l))).collect::<String>()));
    }

    // Tandem's control directory stays read-only even when the worktree is the project.
    let tandem = spec.project_root.join(".tandem");
    p.push_str(&format!("\n; --- tandem state is never agent-writable\n(deny file-write*{})\n", subpaths(std::slice::from_ref(&tandem))));
    if paths::is_under(&spec.worktree_root, &tandem) && spec.worktree_root != tandem {
        p.push_str(&format!("(allow file-write*{})\n", subpaths(std::slice::from_ref(&spec.worktree_root))));
    }

    // Protected subtrees, minus the agent's own dirs (and their ancestors).
    let protected = Protected::new(&spec.home, &spec.project_root, &spec.protected_extra);
    let agent_dirs: Vec<PathBuf> = spec.agent_home_dirs.iter().flat_map(|d| spellings(d)).collect();
    let denied: Vec<PathBuf> = protected
        .dirs()
        .iter()
        .filter(|d| !agent_dirs.iter().any(|a| paths::is_under(a, d)))
        .cloned()
        .collect();

    // Basename rules anywhere: `.env*`, `id_rsa*`, `*.pem`, `*.key`, ...
    let mut name_res: Vec<String> = NAME_PREFIX_PROTECTED.iter().map(|n| format!("/{}[^/]*(/|$)", ci_regex(n))).collect();
    name_res.extend(NAME_SUFFIX_PROTECTED.iter().map(|n| format!("/[^/]*{}(/|$)", ci_regex(n))));
    name_res.extend(protected.name_globs().iter().map(|g| format!("/{}(/|$)", ci_regex(g))));
    p.push_str("(deny file-read* file-write*");
    for r in &name_res {
        p.push_str(&format!("\n  (regex #\"{r}\")"));
    }
    p.push_str(")\n");
    // Public CA bundles (read-only), only where reads were already allowed.
    p.push_str(&format!(
        "(allow file-read* (require-all (regex #\"/({})$\") (require-any{})))\n",
        paths::PUBLIC_PEM_NAMES.iter().map(|n| ci_regex(n)).collect::<Vec<_>>().join("|"),
        subpaths(&reads)
    ));

    // System trust stores (read-only): `SystemRootCertificates.keychain` matches
    // `*.keychain`, and TLS fails with "UnknownIssuer" without it. Protected subtrees
    // below (the login keychain, `/Library/Keychains`) are still denied afterwards.
    let mut trust = String::new();
    for r in paths::TRUST_STORE_ROOTS {
        match r.strip_suffix('*') {
            Some(prefix) => {
                for s in spellings(Path::new(prefix)) {
                    trust.push_str(&format!("\n  (regex #\"^{}[^/]*(/|$)\")", ci_regex(&s.to_string_lossy())));
                }
            }
            None => trust.push_str(&subpaths(&[PathBuf::from(r)])),
        }
    }
    p.push_str(&format!("\n; --- system trust stores are readable\n(allow file-read*{trust})\n"));

    p.push_str(&format!("\n; --- protected paths: denied even under allowed trees (deny after allow)\n(deny file-read* file-write*{})\n", subpaths(&denied)));

    // Mandatory write denies (sandbox-runtime DANGEROUS_FILES / dirs, git hooks).
    p.push_str(
        r#"
; --- code-execution vectors are never writable (sandbox-runtime mandatory denies)
(deny file-write*
  (regex #"/\.git/hooks(/|$)")
  (regex #"/\.git/config$")
  (regex #"/\.(bashrc|bash_profile|zshrc|zprofile|zshenv|profile|gitconfig|gitmodules|ripgreprc|mcp\.json)$")
  (regex #"/\.(vscode|idea)(/|$)")
  (regex #"/\.claude/(commands|agents)(/|$)")
  (regex #"/\.claude/settings(\.local)?\.json$")
"#,
    );
    // ...but the agent's own home (e.g. ~/.claude) may update its own settings.
    p.push_str(")\n");
    if !spec.agent_home_dirs.is_empty() {
        let own_settings: Vec<PathBuf> = spec.agent_home_dirs.iter().filter(|d| d.ends_with(".claude")).cloned().collect();
        if !own_settings.is_empty() {
            p.push_str(&format!("(allow file-write*{})\n", subpaths(&own_settings)));
        }
    }
    // Keep secrets denied inside agent dirs too (last match wins).
    let mut late: Vec<PathBuf> = denied
        .iter()
        .filter(|d| agent_dirs.iter().any(|a| paths::is_under(d, a)))
        .cloned()
        .collect();
    late.retain(|d| !agent_dirs.contains(d));
    if !late.is_empty() {
        p.push_str(&format!("(deny file-read* file-write*{})\n", subpaths(&late)));
    }

    if spec.network {
        p.push_str(
            r#"
; --- network: agents need their vendor API (codex seatbelt_network_policy.sbpl)
(allow network-outbound)
(allow network-bind (local ip "*:*"))
(allow network-inbound (local ip "localhost:*"))
(allow mach-lookup
  (global-name "com.apple.networkd")
  (global-name "com.apple.ocspd")
  (global-name "com.apple.trustd.agent")
  (global-name "com.apple.SystemConfiguration.DNSConfiguration")
  (global-name "com.apple.SystemConfiguration.configd")
)
"#,
        );
    }
    p
}

/// `sandbox-exec -p <profile> cmd args...`
pub fn wrap_command(spec: &SandboxSpec, cmd: &str, args: &[String]) -> (String, Vec<String>) {
    let mut a = vec!["-p".to_string(), profile(spec), cmd.to_string()];
    a.extend(args.iter().cloned());
    (SANDBOX_EXEC.to_string(), a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn spec(dir: &Path) -> SandboxSpec {
        let project = dir.join("proj");
        let wt = project.join(".tandem/worktrees/t1");
        std::fs::create_dir_all(&wt).unwrap();
        let mut s = SandboxSpec::new(&project, &wt, dir.join("home"));
        s.temp_dirs.clear(); // the test's "outside" files live under $TMPDIR
        s
    }

    #[test]
    fn profile_shape() {
        let s = SandboxSpec::new("/p", "/p/.tandem/worktrees/a", "/Users/u");
        let mut s = s;
        let codex = tandem_core::AgentDescriptor { id: "codex".into(), home_dirs: vec![".codex".into()], ..Default::default() };
        s.agent_home_dirs = SandboxSpec::default_agent_home_dirs(&codex, Path::new("/Users/u"));
        let p = profile(&s);
        assert!(p.starts_with("(version 1)"));
        assert!(p.contains("(deny default)"));
        assert!(p.contains("(subpath \"/Users/u/.ssh\")"));
        assert!(p.contains("(subpath \"/Users/u/.claude\")"), "other agents' dirs stay denied");
        // codex's own dir is not in the deny list
        let deny_block = p.split("protected paths").nth(1).unwrap();
        assert!(!deny_block.split("(regex").next().unwrap().contains("/Users/u/.codex\""));
        // deny comes after allow
        assert!(p.find("filesystem: reads").unwrap() < p.find("protected paths").unwrap());
        assert!(p.find("require-any").unwrap() < p.find("protected paths").unwrap());
        let (prog, args) = wrap_command(&s, "echo", &["hi".into()]);
        assert_eq!(prog, SANDBOX_EXEC);
        assert_eq!(args[0], "-p");
        assert_eq!(args[2], "echo");
        assert_eq!(args[3], "hi");
    }

    #[test]
    fn quoting() {
        assert_eq!(q(Path::new("/a\"b\\c")), "\"/a\\\"b\\\\c\"");
        assert_eq!(ci_regex(".env"), "\\.[eE][nN][vV]");
    }

    fn sandbox_works() -> bool {
        if !Path::new(SANDBOX_EXEC).exists() {
            return false;
        }
        Command::new(SANDBOX_EXEC)
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run(spec: &SandboxSpec, cmd: &str, args: &[&str]) -> std::process::Output {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (prog, a) = wrap_command(spec, cmd, &args);
        Command::new(prog).args(a).env_clear().env("PATH", "/usr/bin:/bin").output().unwrap()
    }

    #[test]
    fn tls_works_and_login_keychain_denied() {
        if !sandbox_works() {
            eprintln!("skipping: sandbox-exec unavailable or nested sandbox not permitted");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let mut s = spec(&dir);
        let real_home = PathBuf::from(std::env::var_os("HOME").unwrap());
        s.home = real_home.clone();

        // The user's login keychain stays unreadable (never print its contents).
        let login = real_home.join("Library/Keychains/login.keychain-db");
        if login.exists() {
            let out = run(&s, "/bin/cat", &[login.to_str().unwrap()]);
            assert!(!out.status.success(), "login keychain must be denied");
            assert!(out.stdout.is_empty());
        }

        // TLS verification against the system trust store (skip when offline).
        let online = Command::new("/usr/bin/curl")
            .args(["-sI", "--max-time", "10", "-o", "/dev/null", "https://example.com"])
            .status()
            .map(|st| st.success())
            .unwrap_or(false);
        if !online {
            eprintln!("skipping TLS check: offline");
            return;
        }
        let out = run(&s, "/usr/bin/curl", &["-sSI", "--max-time", "15", "https://example.com"]);
        assert!(out.status.success(), "curl https inside sandbox failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn sandbox_exec_enforces_profile() {
        if !sandbox_works() {
            eprintln!("skipping: sandbox-exec unavailable or nested sandbox not permitted");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let s = spec(&dir);
        let inside = s.worktree_root.join("inside.txt");
        std::fs::write(&inside, "hello-inside").unwrap();
        let outside_dir = dir.join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let outside = outside_dir.join("secret.txt");
        std::fs::write(&outside, "hello-outside").unwrap();
        let pem = s.worktree_root.join("server.pem");
        std::fs::write(&pem, "KEY").unwrap();
        let env = s.worktree_root.join(".env");
        std::fs::write(&env, "TOKEN=1").unwrap();

        let out = run(&s, "/bin/cat", &[inside.to_str().unwrap()]);
        assert!(out.status.success(), "cat inside failed: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello-inside");

        let out = run(&s, "/bin/cat", &[outside.to_str().unwrap()]);
        assert!(!out.status.success(), "cat outside must fail");
        assert!(out.stdout.is_empty());

        let ca = s.worktree_root.join("cacert.pem");
        std::fs::write(&ca, "PUBLIC").unwrap();
        let out = run(&s, "/bin/cat", &[ca.to_str().unwrap()]);
        assert!(out.status.success(), "public CA bundle must stay readable");
        let out = run(&s, "/bin/cat", &["/etc/ssl/cert.pem"]);
        assert!(out.status.success(), "system CA bundle must stay readable");
        let out = run(&s, "/bin/cat", &["/System/Library/Keychains/SystemRootCertificates.keychain"]);
        assert!(out.status.success(), "system root keychain must stay readable");

        for f in [&pem, &env] {
            let out = run(&s, "/bin/cat", &[f.to_str().unwrap()]);
            assert!(!out.status.success(), "protected {} must be unreadable", f.display());
        }

        let target = outside_dir.join("touched");
        let out = run(&s, "/usr/bin/touch", &[target.to_str().unwrap()]);
        assert!(!out.status.success(), "touch outside must fail");
        assert!(!target.exists());

        let ok = s.worktree_root.join("touched");
        let out = run(&s, "/usr/bin/touch", &[ok.to_str().unwrap()]);
        assert!(out.status.success(), "touch inside failed: {}", String::from_utf8_lossy(&out.stderr));
        assert!(ok.exists());

        // Tandem's policy file is never writable.
        let policy = s.project_root.join(".tandem/policy.json");
        let out = run(&s, "/usr/bin/touch", &[policy.to_str().unwrap()]);
        assert!(!out.status.success(), "policy.json must not be writable");

        // Shell pipelines work inside the sandbox.
        let script = format!("cd {} && echo hi > a.txt && cat a.txt | wc -c", s.worktree_root.display());
        let out = run(&s, "/bin/sh", &["-c", &script]);
        assert!(out.status.success(), "sh failed: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3");
    }

    #[test]
    fn git_common_dir_writes_are_narrow() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("p");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let s = SandboxSpec::new(&project, project.join(".tandem/worktrees/t1"), "/Users/u");
        assert_eq!(s.task_worktree.as_deref(), Some("t1"));
        let g = project.join(".git");
        assert_eq!(s.git_write_paths(), [g.join("objects"), g.join("refs/heads/tandem"), g.join("logs/refs/heads/tandem"), g.join("worktrees/t1")]);
        let writes = profile(&s);
        let writes = writes.split("filesystem: writes").nth(1).unwrap().split("tandem state").next().unwrap();
        assert!(!writes.contains(&format!("(subpath {})", q(&g))), "whole git dir must not be writable");
        assert!(writes.contains("packed-refs.lock"));
        // Worktree == project: no per-worktree admin dir.
        assert_eq!(SandboxSpec::new(&project, &project, "/Users/u").task_worktree, None);
    }

    /// Inside the profile `git commit` in the task worktree works, but hooks and config
    /// in the shared git dir stay read-only.
    #[test]
    fn sandboxed_git_commit_but_no_hooks() {
        if !sandbox_works() || !Path::new("/usr/bin/git").exists() {
            eprintln!("skipping: sandbox-exec or git unavailable");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let git = |cwd: &Path, args: &[&str]| {
            Command::new("/usr/bin/git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
                .args(args)
                .current_dir(cwd)
                .env("HOME", &dir)
                .output()
                .unwrap()
        };
        let probe = git(&project, &["init", "-q"]);
        if !probe.status.success() {
            eprintln!("skipping: git unusable: {}", String::from_utf8_lossy(&probe.stderr));
            return;
        }
        std::fs::write(project.join("a.txt"), "a\n").unwrap();
        assert!(git(&project, &["add", "a.txt"]).status.success());
        assert!(git(&project, &["commit", "-qm", "init"]).status.success());
        let wt = project.join(".tandem/worktrees/t1");
        let out = git(&project, &["worktree", "add", "-q", "-b", "tandem/t1", wt.to_str().unwrap()]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        let mut s = SandboxSpec::new(&project, &wt, &dir);
        // The test repo lives under $TMPDIR; keep only /tmp writable so the checks below
        // exercise the git rules, not the temp-dir allowance.
        s.temp_dirs.retain(|t| t.starts_with("/tmp") || t.starts_with("/private/tmp"));
        let common = project.join(".git");
        let script = format!(
            "cd {} && echo b > b.txt && /usr/bin/git add b.txt && /usr/bin/git -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -qm agent",
            q(&wt)
        );
        let args: Vec<String> = vec!["-c".into(), script];
        let (prog, a) = wrap_command(&s, "/bin/sh", &args);
        let out = Command::new(prog).args(a).env_clear().env("PATH", "/usr/bin:/bin").env("HOME", &dir).env("TMPDIR", "/tmp").output().unwrap();
        assert!(out.status.success(), "sandboxed git commit failed: {}", String::from_utf8_lossy(&out.stderr));
        let log = git(&project, &["log", "--format=%s", "-1", "tandem/t1"]);
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "agent");

        for target in [common.join("hooks/pre-commit"), common.join("config"), common.join("refs/heads/main"), common.join("index")] {
            let out = run(&s, "/bin/sh", &["-c", &format!("echo x >> {}", q(&target))]);
            assert!(!out.status.success(), "{} must not be writable", target.display());
        }
    }
}
