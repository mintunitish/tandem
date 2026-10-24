//! Enforced agent launch: scrubbed environment, optional Seatbelt sandbox, Kill Switch
//! registration and the real risk classifier.
//!
//! Agents cannot be trusted to ask before acting (they auto-approve their own shell
//! commands), so enforcement sits *under* the agent process: what it can read and
//! write is decided by the Seatbelt profile, what it inherits by [`child_env`].
//! See docs/specs/2026-10-24-tandem-design.md, "Security model" / "Kill Switch".

use crate::client::{McpServerSpec, PermissionClassifier};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tandem_core::{AgentDescriptor, Policy, RiskClass, ToolCall};
use tandem_sandbox::env::{HARD_DENY, HARD_DENY_PREFIXES};
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
    /// Explicit variables added after scrubbing (not filtered, except code-injection vars).
    pub env_overrides: BTreeMap<String, String>,
    pub mcp_servers: Vec<McpServerSpec>,
    /// Risk classifier for permission requests. [`SpawnOptions::new`] uses tandem-sandbox's.
    pub classifier: Arc<dyn PermissionClassifier>,
}

impl SpawnOptions {
    /// `policy.sandbox` decides `sandbox`; the classifier is [`tandem_sandbox::Classifier`].
    pub fn new(cwd: impl Into<PathBuf>, project_root: impl Into<PathBuf>, policy: Policy) -> Self {
        let (cwd, project_root) = (canonical(cwd.into()), canonical(project_root.into()));
        let classifier = Arc::new(tandem_sandbox::Classifier::new(
            policy.clone(),
            &project_root,
            &cwd,
        ));
        SpawnOptions {
            sandbox: policy.sandbox,
            cwd,
            project_root,
            policy,
            env_overrides: BTreeMap::new(),
            mcp_servers: vec![],
            classifier,
        }
    }
}

fn canonical(p: PathBuf) -> PathBuf {
    p.canonicalize().unwrap_or(p)
}

impl PermissionClassifier for tandem_sandbox::Classifier {
    fn classify(&self, call: &ToolCall) -> (RiskClass, String) {
        tandem_sandbox::Classifier::classify(self, call)
    }
}

fn hard_denied(k: &str) -> bool {
    HARD_DENY.contains(&k) || HARD_DENY_PREFIXES.iter().any(|p| k.starts_with(p))
}

fn passthrough(policy: &Policy, k: &str) -> bool {
    policy
        .env_passthrough
        .iter()
        .any(|p| match p.strip_suffix('*') {
            Some(prefix) => !prefix.is_empty() && k.starts_with(prefix),
            None => p == k,
        })
}

/// Enclosing-agent session markers and credential-looking names.
fn session_or_secret(k: &str) -> bool {
    let u = k.to_uppercase();
    u == "CLAUDECODE"
        || u.starts_with("CLAUDE_CODE_")
        || u == "CLAUDE_PID"
        || ["_SECRET", "_TOKEN", "_KEY", "_PASSWORD"]
            .iter()
            .any(|s| u.ends_with(s))
        || ["SECRET", "TOKEN", "PASSWORD", "PRIVATE_KEY"]
            .iter()
            .any(|s| u.contains(s))
}

/// The agent's own API key (`ANTHROPIC_API_KEY` for claude...), which the spec lets it
/// inherit so API-key users can authenticate.
fn own_api_key(k: &str, desc: &AgentDescriptor) -> bool {
    let (prefixes, _) = tandem_sandbox::env::vendor_vars(desc);
    k.ends_with("_API_KEY") && prefixes.iter().any(|p| k.starts_with(p))
}

/// Child environment: [`tandem_sandbox::env::scrub`], then session markers
/// (`CLAUDECODE`, `CLAUDE_CODE_*`) and `*_SECRET` / `*_TOKEN` / `*_KEY` names removed
/// unless listed in `policy.env_passthrough` (the agent's own `<VENDOR>_API_KEY`
/// survives), then `overrides`.
pub fn child_env(
    parent: &HashMap<String, String>,
    policy: &Policy,
    desc: &AgentDescriptor,
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = tandem_sandbox::env::scrub(parent, policy, desc);
    env.retain(|k, _| {
        desc.env.contains_key(k)
            || passthrough(policy, k)
            || own_api_key(k, desc)
            || !session_or_secret(k)
    });
    if strict_enabled(overrides) {
        env.extend(strict_launch(desc).env);
    }
    for (k, v) in overrides {
        if !hard_denied(k) && !k.is_empty() && !k.contains('=') && !k.contains('\0') {
            env.insert(k.clone(), v.clone());
        }
    }
    env
}

/// `env_overrides` key that turns strict permission launch off when set to `0`, `false`
/// or `off`. Strict launch is on by default.
pub const STRICT_PERMISSIONS_VAR: &str = "TANDEM_STRICT_PERMISSIONS";

/// Extra launch arguments and environment that put one agent into a mode where shell
/// commands and out-of-worktree writes are sent as ACP `session/request_permission`
/// instead of being auto-approved inside the agent (so Tandem's classifier and UI see them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StrictLaunch {
    /// Appended after the descriptor's own args.
    pub args: Vec<String>,
    /// Set after env scrubbing, before `SpawnOptions::env_overrides`.
    pub env: BTreeMap<String, String>,
}

/// OpenCode inline config (`OPENCODE_CONFIG_CONTENT`). It sits above project
/// `opencode.json` in OpenCode's precedence, so a file the agent writes into the
/// worktree cannot loosen it, and it needs no file under `.tandem/`.
pub const OPENCODE_STRICT_CONFIG: &str = r#"{"permission":{"bash":"ask","edit":"allow","webfetch":"ask","external_directory":"ask"}}"#;

/// Strict-permission launch settings for a built-in agent id. Sources:
/// - codex: `codex-acp` modes (`INITIAL_AGENT_MODE`; its default `agent` mode lets a
///   reviewer model auto-approve, `workspace-write` is `on-request` + user reviewer);
///   github.com/agentclientprotocol/codex-acp README "Runtime options".
/// - opencode: <https://opencode.ai/docs/permissions/>, <https://opencode.ai/docs/config/>.
/// - goose: `GOOSE_MODE=approve` (default is `auto`).
/// - gemini: `--approval-mode default` (not `auto_edit` / `yolo`).
/// - claude (claude-agent-acp) and the rest: untouched; Claude's default mode asks.
pub fn strict_launch(desc: &AgentDescriptor) -> StrictLaunch {
    let env = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    match desc.id.as_str() {
        "codex" => StrictLaunch {
            args: vec![],
            env: env(&[("INITIAL_AGENT_MODE", "workspace-write")]),
        },
        "opencode" => StrictLaunch {
            args: vec![],
            env: env(&[("OPENCODE_CONFIG_CONTENT", OPENCODE_STRICT_CONFIG)]),
        },
        "goose" => StrictLaunch {
            args: vec![],
            env: env(&[("GOOSE_MODE", "approve")]),
        },
        "gemini" => StrictLaunch {
            args: vec!["--approval-mode".into(), "default".into()],
            env: BTreeMap::new(),
        },
        _ => StrictLaunch::default(),
    }
}

fn strict_enabled(env: &BTreeMap<String, String>) -> bool {
    !matches!(
        env.get(STRICT_PERMISSIONS_VAR).map(String::as_str),
        Some("0" | "false" | "off")
    )
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
    spec.agent_home_dirs = SandboxSpec::default_agent_home_dirs(desc.id.as_str(), home);
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
    if strict_enabled(env) {
        desc.args.extend(strict_launch(&desc).args);
    }
    let desc = &desc;
    if !opts.sandbox {
        return (desc.command.clone(), desc.args.clone());
    }
    let home = env
        .get("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/var/empty"));
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

    fn desc(id: &str) -> AgentDescriptor {
        AgentDescriptor {
            id: id.into(),
            display_name: id.into(),
            vendor: String::new(),
            command: "/bin/sh".into(),
            args: vec![],
            env: BTreeMap::new(),
            install_hint: String::new(),
            acp: true,
            auth_note: String::new(),
        }
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
        let e = child_env(&parent, &policy, &desc("claude"), &BTreeMap::new());
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
        let e = child_env(&parent, &policy, &desc("claude"), &overrides);
        assert!(e.contains_key("CLAUDE_CODE_OAUTH_TOKEN") && e.contains_key("TANDEM_CANARY_SECRET"));
        assert_eq!(e.get("X").map(String::as_str), Some("1"));
        assert!(!e.contains_key("DYLD_INSERT_LIBRARIES") && !e.contains_key("CLAUDECODE"));
    }

    #[test]
    fn strict_launch_per_agent() {
        let c = strict_launch(&desc("codex"));
        assert!(c.args.is_empty());
        assert_eq!(c.env["INITIAL_AGENT_MODE"], "workspace-write");
        let o = strict_launch(&desc("opencode"));
        let cfg: serde_json::Value = serde_json::from_str(&o.env["OPENCODE_CONFIG_CONTENT"]).unwrap();
        assert_eq!(cfg["permission"]["bash"], "ask");
        assert_eq!(cfg["permission"]["edit"], "allow");
        assert_eq!(cfg["permission"]["webfetch"], "ask");
        assert_eq!(cfg["permission"]["external_directory"], "ask");
        assert_eq!(strict_launch(&desc("goose")).env["GOOSE_MODE"], "approve");
        let g = strict_launch(&desc("gemini"));
        assert_eq!(g.args, ["--approval-mode", "default"]);
        assert!(g.env.is_empty());
        for id in ["claude", "kiro", "fake"] {
            assert_eq!(strict_launch(&desc(id)), StrictLaunch::default(), "{id}");
        }
    }

    #[test]
    fn strict_env_beats_inherited_and_opt_out_works() {
        let parent: HashMap<String, String> = [("PATH", "/usr/bin"), ("GOOSE_MODE", "auto")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let policy = Policy::default();
        let e = child_env(&parent, &policy, &desc("goose"), &BTreeMap::new());
        assert_eq!(e["GOOSE_MODE"], "approve");
        let off: BTreeMap<String, String> =
            [(STRICT_PERMISSIONS_VAR.to_string(), "0".to_string())].into();
        let e = child_env(&parent, &policy, &desc("goose"), &off);
        assert_eq!(e["GOOSE_MODE"], "auto");
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
        let off = [(STRICT_PERMISSIONS_VAR.to_string(), "off".to_string())].into();
        assert_eq!(launch_command(&d, &opts, &off).1, ["--acp"]);
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
