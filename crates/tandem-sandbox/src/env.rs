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

/// Vendor variable allow-list for an agent: (prefixes, exact names), from its descriptor.
pub fn vendor_vars(agent: &AgentDescriptor) -> (&[String], &[String]) {
    (&agent.env_prefixes, &agent.env_exact)
}

/// Code-injection variables: never passed, whatever the policy says.
pub fn hard_denied(k: &str) -> bool {
    HARD_DENY.contains(&k) || HARD_DENY_PREFIXES.iter().any(|p| k.starts_with(p))
}

/// `env_passthrough` entries are exact names, or `PREFIX_*`.
pub fn passthrough(policy: &Policy, k: &str) -> bool {
    policy.env_passthrough.iter().any(|p| match p.strip_suffix('*') {
        Some(prefix) => !prefix.is_empty() && k.starts_with(prefix),
        None => p == k,
    })
}

/// Enclosing-agent session markers and credential-looking names.
pub fn session_or_secret(k: &str) -> bool {
    let u = k.to_uppercase();
    u == "CLAUDECODE"
        || u.starts_with("CLAUDE_CODE_")
        || u == "CLAUDE_PID"
        || ["_SECRET", "_TOKEN", "_KEY", "_PASSWORD"].iter().any(|s| u.ends_with(s))
        || ["SECRET", "TOKEN", "PASSWORD", "PRIVATE_KEY"].iter().any(|s| u.contains(s))
}

/// The agent's own API key (`ANTHROPIC_API_KEY` for claude...), which it may inherit so
/// API-key users can authenticate. Built-in (trusted) descriptors only: a custom agent
/// declaring `env_prefixes: ["OPENAI_"]` must not receive the OpenAI key.
fn own_api_key(k: &str, agent: &AgentDescriptor) -> bool {
    agent.trusted && k.ends_with("_API_KEY") && agent.env_prefixes.iter().any(|p| k.starts_with(p.as_str()))
}

/// Drop empty and relative PATH entries (`.`, `bin`): a planted `./ls` must not win.
fn clean_path(v: &str) -> String {
    v.split(':').filter(|e| e.starts_with('/')).collect::<Vec<_>>().join(":")
}

/// The environment an agent inherits from `parent`: base vars, its own vendor vars and
/// `policy.env_passthrough`; then session markers (`CLAUDECODE`, `CLAUDE_CODE_*`) and
/// `*_SECRET` / `*_TOKEN` / `*_KEY` names are removed unless passed through explicitly (the
/// agent's own `<VENDOR>_API_KEY` survives); finally the descriptor's `env` is applied.
pub fn scrub(parent: &HashMap<String, String>, policy: &Policy, agent: &AgentDescriptor) -> BTreeMap<String, String> {
    let (prefixes, exact) = vendor_vars(agent);
    let mut out = BTreeMap::new();
    for (k, v) in parent {
        if hard_denied(k) || k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0') {
            continue;
        }
        let allowed = BASE_VARS.contains(&k.as_str())
            || BASE_PREFIXES.iter().any(|p| k.starts_with(p))
            || prefixes.iter().any(|p| k.starts_with(p.as_str()))
            || exact.iter().any(|e| e == k);
        let keep = passthrough(policy, k) || (allowed && (own_api_key(k, agent) || !session_or_secret(k)));
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

    /// Mirrors the vendor allow-lists `tandem_acp::registry` fills in.
    fn agent(id: &str) -> AgentDescriptor {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (env_prefixes, env_exact) = match id {
            "claude" => (v(&["ANTHROPIC_", "CLAUDE_"]), vec![]),
            "codex" => (v(&["OPENAI_", "CODEX_"]), vec![]),
            "gemini" => (v(&["GOOGLE_", "GEMINI_"]), vec![]),
            "goose" => (v(&["GOOSE_"]), vec![]),
            "opencode" => (v(&["OPENCODE_"]), vec![]),
            "copilot" => (v(&["COPILOT_"]), v(&["GITHUB_TOKEN"])),
            _ => (vec![], vec![]),
        };
        let trusted = id != "mystery";
        AgentDescriptor { id: id.into(), display_name: id.into(), command: id.into(), acp: true, env_prefixes, env_exact, trusted, ..Default::default() }
    }

    #[test]
    fn custom_prefixes_and_trust() {
        let mut p = parent();
        p.insert("MINE_HOST".into(), "h".into());
        p.insert("MINE_TOKEN".into(), "t".into());
        p.insert("MINE_API_KEY".into(), "k".into());
        p.insert("XDG_RUNTIME_TOKEN".into(), "x".into());
        p.insert("XDG_SESSION_TOKEN".into(), "x".into());
        let custom = AgentDescriptor { id: "mine".into(), env_prefixes: vec!["MINE_".into(), "OPENAI_".into()], ..Default::default() };
        let e = scrub(&p, &Policy::default(), &custom);
        assert!(e.contains_key("MINE_HOST") && e.contains_key("XDG_CONFIG_HOME"));
        for k in ["MINE_TOKEN", "MINE_API_KEY", "OPENAI_API_KEY", "XDG_RUNTIME_TOKEN", "XDG_SESSION_TOKEN"] {
            assert!(!e.contains_key(k), "leaked {k}");
        }
        // OPENAI_API_KEY reaches the built-in codex only.
        for id in ["claude", "gemini", "goose", "opencode", "copilot", "mystery"] {
            assert!(!scrub(&p, &Policy::default(), &agent(id)).contains_key("OPENAI_API_KEY"), "{id}");
        }
        let codex = scrub(&p, &Policy::default(), &agent("codex"));
        assert!(codex.contains_key("OPENAI_API_KEY") && !codex.contains_key("XDG_RUNTIME_TOKEN"));
        // The same descriptor loaded from JSON is untrusted.
        let json = serde_json::to_string(&agent("codex")).unwrap();
        let loaded: AgentDescriptor = serde_json::from_str(&json).unwrap();
        assert!(!loaded.trusted);
        assert!(!scrub(&p, &Policy::default(), &loaded).contains_key("OPENAI_API_KEY"));
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
        for k in ["PATH", "HOME", "USER", "SHELL", "LANG", "LC_ALL", "TERM", "TMPDIR", "XDG_CONFIG_HOME", "ANTHROPIC_API_KEY"] {
            assert!(e.contains_key(k), "missing {k}");
        }
        // Session markers of an enclosing Claude Code are dropped even for claude.
        for k in ["CLAUDE_CODE_USE_BEDROCK", "OPENAI_API_KEY", "GOOGLE_API_KEY", "GITHUB_TOKEN", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "DATABASE_URL", "STRIPE_SECRET", "MY_API_KEY", "NPM_TOKEN", "SSH_AUTH_SOCK", "DYLD_INSERT_LIBRARIES", "LD_PRELOAD", "BASH_ENV", "CUSTOM_ALLOWED"] {
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
        // `GITHUB_TOKEN` is on copilot's allow-list but credential-looking names still need
        // an explicit `env_passthrough`.
        let cp = scrub(&p, &Policy::default(), &agent("copilot"));
        assert!(!cp.contains_key("GITHUB_TOKEN"));
        let pol = Policy { env_passthrough: vec!["GITHUB_TOKEN".into()], ..Policy::default() };
        assert!(scrub(&p, &pol, &agent("copilot")).contains_key("GITHUB_TOKEN"));
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
