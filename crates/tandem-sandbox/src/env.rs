//! Environment scrubbing for agent processes.
//!
//! Agents inherit only a minimal base (PATH, HOME, locale, terminal, temp, XDG), their
//! *own* vendor variables, `Policy.env_passthrough`, and the descriptor's explicit
//! `env`. Everything else (AWS_*, DATABASE_URL, *_TOKEN, SSH_AUTH_SOCK, other vendors'
//! keys...) is stripped.

use std::collections::{BTreeMap, HashMap};
use tandem_core::{AgentDescriptor, Policy};

/// Always passed (exact names).
pub const BASE_VARS: &[&str] = &["PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "TERM", "COLORTERM", "TMPDIR", "TZ"];
/// Always passed (prefixes).
pub const BASE_PREFIXES: &[&str] = &["LC_", "XDG_"];

/// Never passed, not even via `env_passthrough`: these inject code into the child.
pub const HARD_DENY: &[&str] = &["LD_PRELOAD", "LD_LIBRARY_PATH", "LD_AUDIT", "BASH_ENV", "ENV", "PROMPT_COMMAND", "SHELLOPTS", "BASHOPTS"];
pub const HARD_DENY_PREFIXES: &[&str] = &["DYLD_"];

/// Vendor variable allow-list for an agent: (prefixes, exact names).
pub fn vendor_vars(agent: &AgentDescriptor) -> (Vec<&'static str>, Vec<&'static str>) {
    let id = agent.id.as_str().to_lowercase();
    let vendor = agent.vendor.to_lowercase();
    let is = |k: &str, v: &str| id.contains(k) || vendor.contains(v);
    if is("claude", "anthropic") {
        (vec!["ANTHROPIC_", "CLAUDE_"], vec![])
    } else if is("codex", "openai") {
        (vec!["OPENAI_", "CODEX_"], vec![])
    } else if is("gemini", "google") {
        (vec!["GOOGLE_", "GEMINI_"], vec![])
    } else if is("goose", "block") {
        (vec!["GOOSE_"], vec![])
    } else if is("opencode", "sst") {
        (vec!["OPENCODE_"], vec![])
    } else if is("copilot", "github") {
        (vec!["COPILOT_"], vec!["GITHUB_TOKEN"])
    } else {
        (vec![], vec![])
    }
}

fn hard_denied(k: &str) -> bool {
    HARD_DENY.contains(&k) || HARD_DENY_PREFIXES.iter().any(|p| k.starts_with(p))
}

/// `env_passthrough` entries are exact names, or `PREFIX_*`.
fn passthrough(policy: &Policy, k: &str) -> bool {
    policy.env_passthrough.iter().any(|p| match p.strip_suffix('*') {
        Some(prefix) => !prefix.is_empty() && k.starts_with(prefix),
        None => p == k,
    })
}

/// Drop empty and relative PATH entries (`.`, `bin`): a planted `./ls` must not win.
fn clean_path(v: &str) -> String {
    v.split(':').filter(|e| e.starts_with('/')).collect::<Vec<_>>().join(":")
}

pub fn scrub(parent: &HashMap<String, String>, policy: &Policy, agent: &AgentDescriptor) -> BTreeMap<String, String> {
    let (prefixes, exact) = vendor_vars(agent);
    let mut out = BTreeMap::new();
    for (k, v) in parent {
        if hard_denied(k) || k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0') {
            continue;
        }
        let keep = BASE_VARS.contains(&k.as_str())
            || BASE_PREFIXES.iter().any(|p| k.starts_with(p))
            || prefixes.iter().any(|p| k.starts_with(p))
            || exact.contains(&k.as_str())
            || passthrough(policy, k);
        if keep {
            let v = if k == "PATH" { clean_path(v) } else { v.clone() };
            out.insert(k.clone(), v);
        }
    }
    for (k, v) in &agent.env {
        if !hard_denied(k) && !k.is_empty() && !k.contains('=') {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// Apply a scrubbed environment to a command (clears everything else).
pub fn apply(cmd: &mut std::process::Command, env: &BTreeMap<String, String>) {
    cmd.env_clear();
    cmd.envs(env);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> AgentDescriptor {
        AgentDescriptor {
            id: id.into(),
            display_name: id.into(),
            vendor: String::new(),
            command: id.into(),
            args: vec![],
            env: BTreeMap::new(),
            install_hint: String::new(),
            acp: true,
            auth_note: String::new(),
        }
    }

    fn parent() -> HashMap<String, String> {
        [
            ("PATH", "/usr/bin:.::bin:/opt/homebrew/bin"),
            ("HOME", "/Users/u"),
            ("USER", "u"),
            ("SHELL", "/bin/zsh"),
            ("LANG", "en_US.UTF-8"),
            ("LC_ALL", "C"),
            ("TERM", "xterm"),
            ("TMPDIR", "/tmp/"),
            ("XDG_CONFIG_HOME", "/Users/u/.config"),
            ("ANTHROPIC_API_KEY", "a"),
            ("CLAUDE_CODE_USE_BEDROCK", "0"),
            ("OPENAI_API_KEY", "o"),
            ("CODEX_HOME", "/c"),
            ("GOOGLE_API_KEY", "g"),
            ("GEMINI_API_KEY", "g2"),
            ("GOOSE_PROVIDER", "x"),
            ("OPENCODE_CONFIG", "y"),
            ("GITHUB_TOKEN", "gh"),
            ("AWS_ACCESS_KEY_ID", "k"),
            ("AWS_SECRET_ACCESS_KEY", "s"),
            ("DATABASE_URL", "postgres://"),
            ("STRIPE_SECRET", "x"),
            ("MY_API_KEY", "x"),
            ("NPM_TOKEN", "x"),
            ("SSH_AUTH_SOCK", "/tmp/agent"),
            ("DYLD_INSERT_LIBRARIES", "/evil.dylib"),
            ("LD_PRELOAD", "/evil.so"),
            ("BASH_ENV", "/evil.sh"),
            ("CUSTOM_ALLOWED", "1"),
            ("TEAM_FOO", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn claude_gets_only_its_vars() {
        let e = scrub(&parent(), &Policy::default(), &agent("claude"));
        for k in ["PATH", "HOME", "USER", "SHELL", "LANG", "LC_ALL", "TERM", "TMPDIR", "XDG_CONFIG_HOME", "ANTHROPIC_API_KEY", "CLAUDE_CODE_USE_BEDROCK"] {
            assert!(e.contains_key(k), "missing {k}");
        }
        for k in ["OPENAI_API_KEY", "GOOGLE_API_KEY", "GITHUB_TOKEN", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "DATABASE_URL", "STRIPE_SECRET", "MY_API_KEY", "NPM_TOKEN", "SSH_AUTH_SOCK", "DYLD_INSERT_LIBRARIES", "LD_PRELOAD", "BASH_ENV", "CUSTOM_ALLOWED"] {
            assert!(!e.contains_key(k), "leaked {k}");
        }
        assert_eq!(e["PATH"], "/usr/bin:/opt/homebrew/bin");
    }

    #[test]
    fn per_vendor() {
        let p = parent();
        let codex = scrub(&p, &Policy::default(), &agent("codex"));
        assert!(codex.contains_key("OPENAI_API_KEY") && codex.contains_key("CODEX_HOME") && !codex.contains_key("ANTHROPIC_API_KEY"));
        let gem = scrub(&p, &Policy::default(), &agent("gemini"));
        assert!(gem.contains_key("GOOGLE_API_KEY") && gem.contains_key("GEMINI_API_KEY") && !gem.contains_key("OPENAI_API_KEY"));
        let goose = scrub(&p, &Policy::default(), &agent("goose"));
        assert!(goose.contains_key("GOOSE_PROVIDER") && !goose.contains_key("OPENAI_API_KEY"));
        let oc = scrub(&p, &Policy::default(), &agent("opencode"));
        assert!(oc.contains_key("OPENCODE_CONFIG") && !oc.contains_key("GITHUB_TOKEN"));
        let cp = scrub(&p, &Policy::default(), &agent("copilot"));
        assert!(cp.contains_key("GITHUB_TOKEN"));
        let unknown = scrub(&p, &Policy::default(), &agent("mystery"));
        assert!(!unknown.contains_key("GITHUB_TOKEN") && !unknown.contains_key("OPENAI_API_KEY"));
        assert!(!scrub(&p, &Policy::default(), &agent("claude")).contains_key("GITHUB_TOKEN"));
    }

    #[test]
    fn passthrough_and_hard_deny() {
        let pol = Policy { env_passthrough: vec!["CUSTOM_ALLOWED".into(), "TEAM_*".into(), "LD_PRELOAD".into(), "DYLD_*".into()], ..Policy::default() };
        let e = scrub(&parent(), &pol, &agent("claude"));
        assert!(e.contains_key("CUSTOM_ALLOWED") && e.contains_key("TEAM_FOO"));
        assert!(!e.contains_key("LD_PRELOAD") && !e.contains_key("DYLD_INSERT_LIBRARIES"));
    }

    #[test]
    fn descriptor_env_applies() {
        let mut a = agent("claude");
        a.env.insert("CLAUDE_CONFIG_DIR".into(), "/x".into());
        a.env.insert("DYLD_INSERT_LIBRARIES".into(), "/evil".into());
        let e = scrub(&HashMap::new(), &Policy::default(), &a);
        assert_eq!(e.get("CLAUDE_CONFIG_DIR").map(String::as_str), Some("/x"));
        assert!(!e.contains_key("DYLD_INSERT_LIBRARIES"));
    }
}
