//! Built-in ACP agent launch commands and install detection.
//!
//! Commands verified 2026-10-24 against the ACP registry
//! (github.com/agentclientprotocol/registry `<id>/agent.json`), vendor docs and local `--help`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tandem_core::{AgentDescriptor, AgentId};

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
    AgentDescriptor {
        id: AgentId::from(r.0),
        display_name: r.1.into(),
        vendor: r.2.into(),
        command: r.3.into(),
        args: r.4.iter().map(|s| s.to_string()).collect(),
        env: BTreeMap::new(),
        install_hint: r.6.into(),
        acp: true,
        auth_note: r.7.into(),
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

/// Resolve an executable name against a PATH value. Absolute/relative paths are checked directly.
pub fn which_in(cmd: &str, path_var: &std::ffi::OsStr) -> Option<PathBuf> {
    if cmd.contains('/') {
        let p = PathBuf::from(cmd);
        return is_executable(&p).then_some(p);
    }
    std::env::split_paths(path_var)
        .map(|d| d.join(cmd))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
