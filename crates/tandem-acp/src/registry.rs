//! Built-in ACP agent launch commands and install detection.
//!
//! Commands verified 2026-10-24 against the ACP registry
//! (github.com/agentclientprotocol/registry `<id>/agent.json`), vendor docs and local `--help`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use tandem_core::{AgentDescriptor, AgentId, QuotaKind};

/// (id, display name, vendor, command, args, npm package providing `command`, install hint, auth note)
type Row = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    Option<&'static str>,
    &'static str,
    &'static str,
);

const AGENTS: &[Row] = &[
    (
        "claude",
        "Claude Code",
        "Anthropic",
        "claude-agent-acp",
        &[],
        Some("@agentclientprotocol/claude-agent-acp"),
        "npm i -g @agentclientprotocol/claude-agent-acp",
        "Claude Code login (`claude /login`)",
    ),
    (
        "codex",
        "Codex",
        "OpenAI",
        "codex-acp",
        &[],
        Some("@agentclientprotocol/codex-acp"),
        "npm i -g @agentclientprotocol/codex-acp",
        "Codex login (`codex login`)",
    ),
    (
        "gemini",
        "Gemini CLI",
        "Google",
        "gemini",
        &["--acp"],
        None,
        "npm i -g @google/gemini-cli",
        "Gemini CLI login (`gemini`)",
    ),
    (
        "opencode",
        "OpenCode",
        "OpenCode",
        "opencode",
        &["acp"],
        None,
        "curl -fsSL https://opencode.ai/install | bash",
        "OpenCode auth (`opencode auth login`)",
    ),
    (
        "goose",
        "Goose",
        "Block",
        "goose",
        &["acp"],
        None,
        "https://block.github.io/goose/docs/getting-started/installation",
        "Goose config (`goose configure`)",
    ),
    (
        "kiro",
        "Kiro CLI",
        "AWS",
        "kiro-cli",
        &["acp"],
        None,
        "https://kiro.dev/docs/cli/installation",
        "Kiro login (`kiro-cli login`)",
    ),
    (
        "cline",
        "Cline",
        "Cline",
        "cline",
        &["--acp"],
        None,
        "npm i -g cline",
        "Cline auth (`cline auth`)",
    ),
    (
        "copilot",
        "GitHub Copilot CLI",
        "GitHub",
        "copilot",
        &["--acp"],
        None,
        "npm i -g @github/copilot",
        "GitHub login (`copilot` then /login)",
    ),
    (
        "qwen",
        "Qwen Code",
        "Alibaba",
        "qwen",
        &["--acp"],
        None,
        "npm i -g @qwen-code/qwen-code",
        "Qwen login (`qwen`)",
    ),
];

fn descriptor(r: &Row) -> AgentDescriptor {
    let mut d = AgentDescriptor {
        id: AgentId::from(r.0),
        display_name: r.1.into(),
        vendor: r.2.into(),
        command: r.3.into(),
        args: r.4.iter().map(|s| s.to_string()).collect(),
        env: BTreeMap::new(),
        install_hint: r.6.into(),
        acp: true,
        auth_note: r.7.into(),
        trusted: true,
        ..Default::default()
    };
    capabilities(&mut d);
    d
}

/// OpenCode inline config (`OPENCODE_CONFIG_CONTENT`). It sits above project
/// `opencode.json` in OpenCode's precedence, so a file the agent writes into the
/// worktree cannot loosen it, and it needs no file under `.tandem/`.
pub const OPENCODE_STRICT_CONFIG: &str = r#"{"permission":{"bash":"ask","edit":"allow","webfetch":"ask","external_directory":"ask"}}"#;

/// Per-agent capabilities: vendor env allow-list, own config dirs (writable in the
/// sandbox), strict-permission launch, login command, badge and quota provider.
///
/// Strict launch sources:
/// - codex: `codex-acp` modes (`INITIAL_AGENT_MODE`; its default `agent` mode lets a
///   reviewer model auto-approve, `workspace-write` is `on-request` + user reviewer);
///   github.com/agentclientprotocol/codex-acp README "Runtime options".
/// - opencode: <https://opencode.ai/docs/permissions/>, <https://opencode.ai/docs/config/>.
/// - goose: `GOOSE_MODE=approve` (default is `auto`).
/// - gemini: `--approval-mode default` (not `auto_edit` / `yolo`).
/// - claude (claude-agent-acp) and the rest: untouched; Claude's default mode asks.
fn capabilities(d: &mut AgentDescriptor) {
    let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let kv = |xs: &[(&str, &str)]| xs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>();
    let badge = |l: &str, c: &str| (l.to_string(), c.to_string());
    match d.id.as_str() {
        "claude" => {
            d.env_prefixes = v(&["ANTHROPIC_", "CLAUDE_"]);
            d.home_dirs = v(&[".claude", ".claude.json", ".config/claude"]);
            // Claude Code signs in with its `/login` slash command.
            d.login_command = Some(v(&["claude", "/login"]));
            d.badge = badge("C", "D97757");
            d.quota_kind = QuotaKind::Claude;
        }
        "codex" => {
            d.env_prefixes = v(&["OPENAI_", "CODEX_"]);
            d.home_dirs = v(&[".codex"]);
            d.strict_env = kv(&[("INITIAL_AGENT_MODE", "workspace-write")]);
            d.login_command = Some(v(&["codex", "login"]));
            d.badge = badge("X", "10A37F");
            d.quota_kind = QuotaKind::Codex;
        }
        "gemini" => {
            d.env_prefixes = v(&["GOOGLE_", "GEMINI_"]);
            d.home_dirs = v(&[".gemini"]);
            d.strict_args = v(&["--approval-mode", "default"]);
            d.login_command = Some(v(&["gemini"]));
            d.badge = badge("G", "4285F4");
            d.quota_kind = QuotaKind::Gemini;
        }
        "opencode" => {
            d.env_prefixes = v(&["OPENCODE_"]);
            d.home_dirs = v(&[".config/opencode", ".local/share/opencode", ".local/state/opencode"]);
            d.strict_env = kv(&[("OPENCODE_CONFIG_CONTENT", OPENCODE_STRICT_CONFIG)]);
            d.login_command = Some(v(&["opencode", "auth", "login"]));
            d.badge = badge("O", "C77DBB");
        }
        "goose" => {
            d.env_prefixes = v(&["GOOSE_"]);
            d.home_dirs = v(&[".config/goose", ".local/share/goose", ".local/state/goose"]);
            d.strict_env = kv(&[("GOOSE_MODE", "approve")]);
            d.login_command = Some(v(&["goose", "configure"]));
            d.badge = badge("Gs", "F2C55C");
            d.quota_kind = QuotaKind::Goose;
        }
        "kiro" => {
            d.login_command = Some(v(&["kiro-cli", "login"]));
            d.badge = badge("K", "9046FF");
        }
        "cline" => {
            d.login_command = Some(v(&["cline", "auth"]));
            d.badge = badge("Cl", "5B8DEF");
        }
        "copilot" => {
            d.env_prefixes = v(&["COPILOT_"]);
            d.env_exact = v(&["GITHUB_TOKEN"]);
            d.home_dirs = v(&[".copilot", ".config/github-copilot"]);
            d.login_command = Some(v(&["copilot"]));
            d.badge = badge("Co", "8957E5");
        }
        "qwen" => {
            d.login_command = Some(v(&["qwen"]));
            d.badge = badge("Q", "615CED");
        }
        _ => {}
    }
}

/// All built-in agent descriptors.
pub fn builtin() -> Vec<AgentDescriptor> {
    AGENTS.iter().map(descriptor).collect()
}

/// Built-in descriptor by id.
pub fn get(id: &str) -> Option<AgentDescriptor> {
    AGENTS.iter().find(|r| r.0 == id).map(descriptor)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Binary found on PATH.
    Installed(PathBuf),
    /// Binary missing, but runnable via `npx -y <pkg>@latest` (descriptor is rewritten to use npx).
    Installable,
}

#[derive(Clone, Debug)]
pub struct DetectedAgent {
    pub descriptor: AgentDescriptor,
    pub availability: Availability,
}

/// Agents runnable on this machine, searching `$PATH`.
pub fn detect_installed() -> Vec<DetectedAgent> {
    detect_in(std::env::var_os("PATH").as_deref().unwrap_or_default())
}

/// Same as [`detect_installed`] with an explicit PATH value.
pub fn detect_in(path_var: &std::ffi::OsStr) -> Vec<DetectedAgent> {
    let npx = which_in("npx", path_var);
    AGENTS
        .iter()
        .filter_map(|r| {
            let mut descriptor = descriptor(r);
            if let Some(p) = which_in(r.3, path_var) {
                return Some(DetectedAgent {
                    descriptor,
                    availability: Availability::Installed(p),
                });
            }
            let (pkg, npx) = (r.5?, npx.as_ref()?);
            descriptor.command = npx.to_string_lossy().into_owned();
            descriptor.args = ["-y".to_string(), format!("{pkg}@latest")]
                .into_iter()
                .chain(descriptor.args)
                .collect();
            Some(DetectedAgent {
                descriptor,
                availability: Availability::Installable,
            })
        })
        .collect()
}

pub use tandem_core::fs::which_in;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn touch_exe(dir: &Path, name: &str) {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn all_builtins_have_unique_ids() {
        let ids: std::collections::BTreeSet<_> = builtin().into_iter().map(|d| d.id).collect();
        assert_eq!(ids.len(), AGENTS.len());
        assert_eq!(get("gemini").unwrap().args, vec!["--acp"]);
        assert!(builtin().iter().all(|d| !d.badge.0.is_empty() && d.login_command.is_some() && d.trusted));
        assert_eq!(get("claude").unwrap().login_command.unwrap(), ["claude", "/login"]);
        let cfg: serde_json::Value = serde_json::from_str(&get("opencode").unwrap().strict_env["OPENCODE_CONFIG_CONTENT"]).unwrap();
        assert_eq!(cfg["permission"]["bash"], "ask");
        assert_eq!(cfg["permission"]["edit"], "allow");
        assert_eq!(cfg["permission"]["external_directory"], "ask");
    }

    #[test]
    fn detects_installed_and_npx_installable() {
        let dir = tempfile::tempdir().unwrap();
        touch_exe(dir.path(), "goose");
        touch_exe(dir.path(), "npx");
        let found = detect_in(dir.path().as_os_str());
        let ids: Vec<_> = found
            .iter()
            .map(|d| d.descriptor.id.as_str().to_string())
            .collect();
        assert_eq!(ids, vec!["claude", "codex", "goose"]);
        assert_eq!(found[0].availability, Availability::Installable);
        assert_eq!(
            found[0].descriptor.args,
            vec!["-y", "@agentclientprotocol/claude-agent-acp@latest"]
        );
        assert!(matches!(found[2].availability, Availability::Installed(_)));
    }
}
