//! Validation of agent descriptors that did not come from the built-in registry.
//!
//! A descriptor's capability fields widen what the sandbox lets an agent do: `home_dirs`
//! become writable, `env_prefixes` / `env_exact` pass variables through the scrubber,
//! `strict_*` change how it is launched. Only the built-in registry is trusted with them.
//! A user's `~/.tandem/agents.json` may set them within the limits below; a project's
//! `.tandem/agents.json` (anyone who controls the repo) may not set them at all.

use crate::env::{hard_denied, session_or_secret};
use crate::paths::HOME_PROTECTED;
use std::path::{Component, Path};
use tandem_core::{AgentDescriptor, QuotaKind};

/// Where a descriptor came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// `tandem_acp::registry`: used as is.
    Builtin,
    /// `~/.tandem/agents.json`: capability fields validated.
    User,
    /// `<project>/.tandem/agents.json`: capability fields reset to safe defaults.
    Project,
}

/// Home-relative files that run code when a shell or git starts; never writable.
const STARTUP_FILES: &[&str] = &[
    ".profile", ".bashrc", ".bash_profile", ".bash_login", ".bash_logout", ".zshrc", ".zshenv", ".zprofile",
    ".zlogin", ".zlogout", ".config/fish", ".gitconfig", ".config/git", ".inputrc", ".tandem",
];

/// Drop `desc`'s capabilities its source may not grant. Invalid entries are logged and
/// removed; the rest of the descriptor is kept. The result is never `trusted` unless
/// `trust` is [`Trust::Builtin`].
pub fn sanitize(desc: AgentDescriptor, trust: Trust) -> AgentDescriptor {
    match trust {
        Trust::Builtin => desc,
        Trust::Project => AgentDescriptor {
            id: desc.id,
            display_name: desc.display_name,
            vendor: desc.vendor,
            command: desc.command,
            args: desc.args,
            install_hint: desc.install_hint,
            auth_note: desc.auth_note,
            acp: desc.acp,
            // Everything else (env, home dirs, env allow-lists, strict launch, login
            // command, quota provider, badge) back to defaults; the UI derives the badge.
            quota_kind: QuotaKind::AcpGeneric,
            ..Default::default()
        },
        Trust::User => {
            let id = desc.id.to_string();
            let keep = |what: &str, ok: bool, v: &str| {
                if !ok {
                    tracing::warn!(agent = %id, "ignoring {what} entry {v:?} in ~/.tandem/agents.json");
                }
                ok
            };
            let mut d = desc;
            d.trusted = false;
            d.home_dirs.retain(|h| keep("home_dirs", valid_home_dir(h), h));
            d.env_prefixes.retain(|p| keep("env_prefixes", valid_env_prefix(p), p));
            d.env_exact.retain(|k| keep("env_exact", valid_env_name(k), k));
            d.strict_env.retain(|k, _| keep("strict_env", !k.is_empty() && !k.contains('=') && !hard_denied(k), k));
            d.env.retain(|k, _| keep("env", !k.is_empty() && !k.contains('=') && !hard_denied(k), k));
            d
        }
    }
}

/// A home-relative dir an agent may write: relative, plain components only, and neither
/// inside nor above a protected path, `Library`, or a shell / git startup file.
pub fn valid_home_dir(h: &str) -> bool {
    let p = Path::new(h);
    if h.trim().is_empty() || p.is_absolute() || h.contains('\0') {
        return false;
    }
    if !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return false;
    }
    let off_limits = HOME_PROTECTED.iter().chain(STARTUP_FILES).chain(&["Library"]);
    !off_limits.into_iter().any(|x| {
        let x = Path::new(x);
        p.starts_with(x) || x.starts_with(p)
    })
}

/// `FOO_`: at least 3 chars, `[A-Z0-9_]`, ending in `_`, and not a code-injection prefix.
pub fn valid_env_prefix(p: &str) -> bool {
    p.len() >= 3
        && p.ends_with('_')
        && p.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !hard_denied(p)
        && !["DYLD_", "LD_"].iter().any(|d| p.starts_with(d) || d.starts_with(p))
}

/// An exact variable name that is not code-injecting, a session marker or secret-looking.
pub fn valid_env_name(k: &str) -> bool {
    !k.is_empty() && !k.contains('=') && !k.contains('\0') && !hard_denied(k) && !session_or_secret(k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn custom() -> AgentDescriptor {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        AgentDescriptor {
            id: "mine".into(),
            display_name: "Mine".into(),
            vendor: "Me".into(),
            command: "/opt/mine/bin/mine".into(),
            args: v(&["--acp"]),
            install_hint: "brew install mine".into(),
            auth_note: "mine login".into(),
            acp: true,
            env: BTreeMap::from([("NODE_OPTIONS".into(), "--require /evil.js".into())]),
            home_dirs: v(&[".mine", ".config/mine"]),
            env_prefixes: v(&["MINE_", "OPENAI_"]),
            env_exact: v(&["MINE_HOST"]),
            strict_args: v(&["--ask"]),
            strict_env: BTreeMap::from([("MINE_MODE".into(), "ask".into())]),
            login_command: Some(v(&["mine", "login"])),
            badge: ("M".into(), "123456".into()),
            quota_kind: QuotaKind::Claude,
            trusted: true,
        }
    }

    #[test]
    fn project_keeps_only_launch_fields() {
        let d = sanitize(custom(), Trust::Project);
        assert_eq!((d.id.as_str(), d.display_name.as_str(), d.vendor.as_str()), ("mine", "Mine", "Me"));
        assert_eq!((d.command.as_str(), d.args.as_slice()), ("/opt/mine/bin/mine", ["--acp".to_string()].as_slice()));
        assert!(d.acp && d.install_hint == "brew install mine" && d.auth_note == "mine login");
        assert!(d.env.is_empty() && d.home_dirs.is_empty() && d.env_prefixes.is_empty() && d.env_exact.is_empty());
        assert!(d.strict_args.is_empty() && d.strict_env.is_empty() && d.login_command.is_none());
        assert_eq!(d.badge, (String::new(), String::new()));
        assert_eq!(d.quota_kind, QuotaKind::AcpGeneric);
        assert!(!d.trusted);
    }

    #[test]
    fn user_keeps_valid_capabilities() {
        let d = sanitize(custom(), Trust::User);
        assert_eq!(d.home_dirs, [".mine", ".config/mine"]);
        assert_eq!(d.env_prefixes, ["MINE_", "OPENAI_"]);
        assert_eq!(d.strict_args, ["--ask"]);
        assert_eq!(d.quota_kind, QuotaKind::Claude);
        assert!(!d.trusted, "user agents never get the built-in trust");
        assert!(sanitize(custom(), Trust::Builtin).trusted);
    }

    #[test]
    fn home_dirs_table() {
        for (h, ok) in [
            (".mine", true),
            (".config/mine", true),
            (".local/share/mine", true),
            ("", false),
            ("   ", false),
            ("/etc", false),
            ("/Users/u/.mine", false),
            ("..", false),
            ("../x", false),
            (".mine/../.ssh", false),
            ("./.mine", false),
            (".ssh", false),
            (".ssh/keys", false),
            (".aws", false),
            (".config", false), // ancestor of .config/gcloud
            (".config/gcloud/x", false),
            (".local", false),  // ancestor of .local/share/keyrings
            ("Library", false),
            ("Library/LaunchAgents", false),
            ("Library/Application Support/mine", false),
            (".zshrc", false),
            (".gitconfig", false),
            (".tandem", false),
            (".claude", false),
        ] {
            assert_eq!(valid_home_dir(h), ok, "{h:?}");
        }
    }

    #[test]
    fn env_tables() {
        for (p, ok) in [("MINE_", true), ("AB_", true), ("A_", false), ("", false), ("_", false), ("MINE", false), ("mine_", false), ("MI-NE_", false), ("DYLD_", false), ("LD_", false)] {
            assert_eq!(valid_env_prefix(p), ok, "{p:?}");
        }
        for (k, ok) in [("MINE_HOST", true), ("LD_PRELOAD", false), ("DYLD_INSERT_LIBRARIES", false), ("BASH_ENV", false), ("GITHUB_TOKEN", false), ("MINE_API_KEY", false), ("CLAUDECODE", false), ("", false), ("A=B", false)] {
            assert_eq!(valid_env_name(k), ok, "{k:?}");
        }
        let mut d = custom();
        d.env_prefixes = vec!["".into(), "X".into(), "MINE_".into()];
        d.env_exact = vec!["SSH_AUTH_TOKEN".into(), "MINE_HOST".into()];
        d.strict_env.insert("DYLD_INSERT_LIBRARIES".into(), "/evil".into());
        d.env.insert("LD_PRELOAD".into(), "/evil".into());
        let d = sanitize(d, Trust::User);
        assert_eq!(d.env_prefixes, ["MINE_"]);
        assert_eq!(d.env_exact, ["MINE_HOST"]);
        assert!(!d.strict_env.contains_key("DYLD_INSERT_LIBRARIES") && d.strict_env.contains_key("MINE_MODE"));
        assert!(!d.env.contains_key("LD_PRELOAD"));
        // A project agent asking for another vendor's prefix gets nothing.
        let mut p = custom();
        p.env_prefixes = vec!["OPENAI_".into()];
        assert!(sanitize(p, Trust::Project).env_prefixes.is_empty());
    }
}
