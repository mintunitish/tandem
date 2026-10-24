//! Enforced agent launch: scrubbed environment, optional Seatbelt sandbox, Kill Switch
//! registration and the real risk classifier.
//!
//! Agents cannot be trusted to ask before acting (they auto-approve their own shell
//! commands), so enforcement sits *under* the agent process: what it can read and
//! write is decided by the Seatbelt profile, what it inherits by [`child_env`].
//! See docs/specs/2026-10-24-tandem-design.md, "Security model" / "Kill Switch".

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tandem_core::{AgentDescriptor, McpServerSpec, PermissionClassifier, Policy};
use tandem_sandbox::env::hard_denied;
use tandem_sandbox::kill::{KillSwitch, StopReport};
use tandem_sandbox::seatbelt::{self, SandboxSpec};

/// How to launch one agent process.
#[derive(Clone)]
pub struct SpawnOptions {
    /// Task directory (worktree); the agent's cwd and its only writable project tree.
    pub cwd: PathBuf,
    /// Repository root (readable; `.tandem/` stays read-only).
    pub project_root: PathBuf,
    pub policy: Policy,
    /// Wrap the agent in `sandbox-exec` (macOS Seatbelt).
    pub sandbox: bool,
    /// Launch with the descriptor's `strict_args` / `strict_env` so the agent asks before
    /// running shell commands (see [`AgentDescriptor::strict_args`]).
    pub strict_permissions: bool,
    /// Explicit variables added after scrubbing (not filtered, except code-injection vars).
    pub env_overrides: BTreeMap<String, String>,
    pub mcp_servers: Vec<McpServerSpec>,
    /// Risk classifier for permission requests. [`SpawnOptions::new`] uses tandem-sandbox's.
    pub classifier: Arc<dyn PermissionClassifier>,
}

impl SpawnOptions {
    /// `policy.sandbox` / `policy.strict_permissions` decide `sandbox` / `strict_permissions`;
    /// the classifier is [`tandem_sandbox::Classifier`].
    pub fn new(cwd: impl Into<PathBuf>, project_root: impl Into<PathBuf>, policy: Policy) -> Self {
        let (cwd, project_root) = (canonical(cwd.into()), canonical(project_root.into()));
        let classifier = Arc::new(tandem_sandbox::Classifier::new(policy.clone(), &project_root, &cwd));
        SpawnOptions {
            sandbox: policy.sandbox,
            strict_permissions: policy.strict_permissions,
            cwd,
            project_root,
            policy,
            env_overrides: BTreeMap::new(),
            mcp_servers: vec![],
            classifier,
        }
    }
}

impl SpawnOptions {
    /// No sandbox, default policy, project root = `cwd`, every permission request
    /// classified `Review` (tests, demos, harness fakes). `strict_permissions` follows the
    /// policy.
    pub fn unsandboxed(cwd: impl Into<PathBuf>) -> Self {
        let cwd = canonical(cwd.into());
        let policy = Policy::default();
        SpawnOptions {
            project_root: cwd.clone(),
            cwd,
            sandbox: false,
            strict_permissions: policy.strict_permissions,
            policy,
            env_overrides: BTreeMap::new(),
            mcp_servers: vec![],
            classifier: Arc::new(crate::DefaultClassifier),
        }
    }
}

fn canonical(p: PathBuf) -> PathBuf {
    p.canonicalize().unwrap_or(p)
}

/// Child environment: [`tandem_sandbox::env::scrub`], then the descriptor's `strict_env`
/// (when `strict`), then `overrides` (code-injection names refused in both).
pub fn child_env(
    parent: &HashMap<String, String>,
    policy: &Policy,
    desc: &AgentDescriptor,
    strict: bool,
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = tandem_sandbox::env::scrub(parent, policy, desc);
    let strict_env = desc.strict_env.iter().filter(|_| strict);
    for (k, v) in strict_env.chain(overrides) {
        if !hard_denied(k) && !k.is_empty() && !k.contains('=') && !k.contains('\0') {
            env.insert(k.clone(), v.clone());
        }
    }
    env
}

/// Seatbelt spec for an agent: project + worktree, the agent's own config dirs,
/// package caches (npm/npx, cargo, ...), the agent executable itself; network on.
pub fn sandbox_spec(
    desc: &AgentDescriptor,
    opts: &SpawnOptions,
    home: &Path,
    path_var: Option<&str>,
) -> SandboxSpec {
    let mut spec = SandboxSpec::new(&opts.project_root, &opts.cwd, home);
    spec.agent_home_dirs = SandboxSpec::default_agent_home_dirs(desc, home);
    spec.protected_extra = opts.policy.protected_paths.clone();
    spec.network = true;
    // npm/npx: `~/.npm` (cache, `_npx` installs, logs) and `~/.cache` are already in the
    // default write roots; npm also keeps per-user state under `~/.config` for some
    // packages, but that tree is not opened up (it holds other tools' credentials).
    // The launcher binary may live outside the read roots (e.g. a dev build).
    if let Some(exe) = crate::registry::which_in(
        &desc.command,
        std::ffi::OsStr::new(path_var.unwrap_or_default()),
    ) && let Ok(real) = exe.canonicalize()
    {
        spec.read_roots.push(exe.clone());
        spec.read_roots.push(real);
    }
    spec
}

/// Program + args to execute: `desc` itself, or wrapped in `sandbox-exec`.
pub fn launch_command(
    desc: &AgentDescriptor,
    opts: &SpawnOptions,
    env: &BTreeMap<String, String>,
) -> (String, Vec<String>) {
    let mut desc = desc.clone();
    if opts.strict_permissions {
        desc.args.extend(desc.strict_args.clone());
    }
    let desc = &desc;
    if !opts.sandbox {
        return (desc.command.clone(), desc.args.clone());
    }
    let home = env
        .get("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(tandem_core::fs::home);
    let spec = sandbox_spec(desc, opts, &home, env.get("PATH").map(String::as_str));
    // Resolve against the scrubbed PATH: sandbox-exec runs `execvp` with its own env.
    let cmd = crate::registry::which_in(
        &desc.command,
        std::ffi::OsStr::new(env.get("PATH").map(String::as_str).unwrap_or_default()),
    )
    .map(|p| p.to_string_lossy().into_owned())
    .unwrap_or_else(|| desc.command.clone());
    seatbelt::wrap_command(&spec, &cmd, &desc.args)
}

/// Global STOP: SIGTERM every registered agent process group, SIGKILL survivors after 2 s.
/// New spawns are refused until [`reset_stop`].
pub fn stop_all() -> StopReport {
    KillSwitch::global().stop_all()
}

/// Re-arm spawning after [`stop_all`].
pub fn reset_stop() {
    KillSwitch::global().reset();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in descriptor (capabilities included) with `command` = `/bin/sh`.
    fn desc(id: &str) -> AgentDescriptor {
        let base = crate::registry::get(id).unwrap_or_else(|| AgentDescriptor { id: id.into(), display_name: id.into(), acp: true, ..Default::default() });
        AgentDescriptor { command: "/bin/sh".into(), args: vec![], ..base }
    }

    #[test]
    fn env_drops_sessions_and_secrets() {
        let parent: HashMap<String, String> = [
            ("PATH", "/usr/bin"),
            ("HOME", "/Users/u"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            ("CLAUDE_CODE_OAUTH_TOKEN", "t"),
            ("CLAUDE_CONFIG_DIR", "/c"),
            ("ANTHROPIC_API_KEY", "a"),
            ("ANTHROPIC_AUTH_TOKEN", "a"),
            ("OPENAI_API_KEY", "o"),
            ("TANDEM_CANARY_SECRET", "x"),
            ("XDG_SESSION_TOKEN", "x"),
            ("LC_SECRET", "x"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let mut policy = Policy::default();
        let e = child_env(&parent, &policy, &desc("claude"), true, &BTreeMap::new());
        for k in ["PATH", "HOME", "CLAUDE_CONFIG_DIR", "ANTHROPIC_API_KEY"] {
            assert!(e.contains_key(k), "missing {k}");
        }
        for k in [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_AUTH_TOKEN",
            "OPENAI_API_KEY",
            "TANDEM_CANARY_SECRET",
            "XDG_SESSION_TOKEN",
            "LC_SECRET",
        ] {
            assert!(!e.contains_key(k), "leaked {k}");
        }
        policy.env_passthrough = vec!["CLAUDE_CODE_OAUTH_TOKEN".into(), "TANDEM_*".into()];
        let overrides = [
            ("X".to_string(), "1".to_string()),
            ("DYLD_INSERT_LIBRARIES".to_string(), "/evil".to_string()),
        ]
        .into();
        let e = child_env(&parent, &policy, &desc("claude"), true, &overrides);
        assert!(e.contains_key("CLAUDE_CODE_OAUTH_TOKEN") && e.contains_key("TANDEM_CANARY_SECRET"));
        assert_eq!(e.get("X").map(String::as_str), Some("1"));
        assert!(!e.contains_key("DYLD_INSERT_LIBRARIES") && !e.contains_key("CLAUDECODE"));
    }

    #[test]
    fn strict_env_beats_inherited_and_opt_out_works() {
        let parent: HashMap<String, String> = [("PATH", "/usr/bin"), ("GOOSE_MODE", "auto")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let policy = Policy::default();
        let e = child_env(&parent, &policy, &desc("goose"), true, &BTreeMap::new());
        assert_eq!(e["GOOSE_MODE"], "approve");
        let e = child_env(&parent, &policy, &desc("goose"), false, &BTreeMap::new());
        assert_eq!(e["GOOSE_MODE"], "auto");
        assert_eq!(child_env(&parent, &policy, &desc("codex"), true, &BTreeMap::new())["INITIAL_AGENT_MODE"], "workspace-write");
        let mut evil = desc("goose");
        evil.strict_env.insert("DYLD_INSERT_LIBRARIES".into(), "/evil".into());
        evil.strict_env.insert("LD_PRELOAD".into(), "/evil".into());
        let e = child_env(&parent, &policy, &evil, true, &BTreeMap::new());
        assert!(!e.contains_key("DYLD_INSERT_LIBRARIES") && !e.contains_key("LD_PRELOAD"));
        for id in ["claude", "kiro"] {
            assert!(desc(id).strict_args.is_empty() && desc(id).strict_env.is_empty(), "{id}");
        }
    }

    #[test]
    fn strict_args_appended_unless_opted_out() {
        let mut opts = SpawnOptions::new("/tmp", "/tmp", Policy::default());
        opts.sandbox = false;
        let mut d = desc("gemini");
        d.command = "gemini".into();
        d.args = vec!["--acp".into()];
        let env = BTreeMap::new();
        let (prog, args) = launch_command(&d, &opts, &env);
        assert_eq!(prog, "gemini");
        assert_eq!(args, ["--acp", "--approval-mode", "default"]);
        opts.strict_permissions = false;
        assert_eq!(launch_command(&d, &opts, &env).1, ["--acp"]);
    }

    #[test]
    fn sandbox_wraps_and_allows_npm_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = SpawnOptions::new(dir.path(), dir.path(), Policy::default());
        opts.sandbox = true;
        let mut d = desc("codex");
        d.command = "npx".into();
        d.args = vec!["-y".into(), "@agentclientprotocol/codex-acp@latest".into()];
        let env: BTreeMap<String, String> = [
            ("HOME".to_string(), "/Users/u".to_string()),
            ("PATH".to_string(), "/nonexistent".to_string()),
        ]
        .into();
        let (prog, args) = launch_command(&d, &opts, &env);
        assert_eq!(prog, seatbelt::SANDBOX_EXEC);
        assert_eq!(&args[2..], ["npx", "-y", "@agentclientprotocol/codex-acp@latest"]);
        let profile = &args[1];
        assert!(profile.contains("(subpath \"/Users/u/.codex\")"));
        let writes = profile.split("filesystem: writes").nth(1).unwrap();
        assert!(writes.contains("(subpath \"/Users/u/.npm\")"));
        opts.sandbox = false;
        assert_eq!(launch_command(&d, &opts, &env).0, "npx");
    }
}
