use std::path::PathBuf;

/// An agent family and where it discovers project-level skills (relative to the worktree root).
///
/// Verified against vendor docs on 2026-10-24:
/// - Claude Code: `.claude/skills/<name>/SKILL.md` (<https://code.claude.com/docs/en/skills>).
/// - Codex: `.agents/skills` from the cwd up to the repo root; `.codex/skills` is not documented
///   (<https://learn.chatgpt.com/docs/build-skills>, formerly developers.openai.com/codex/skills).
/// - Gemini CLI: workspace `.gemini/skills/` or the `.agents/skills/` alias, the alias taking
///   precedence (<https://geminicli.com/docs/cli/skills/>).
/// - OpenCode: `.opencode/skills/`, `.claude/skills/`, `.agents/skills/`
///   (<https://opencode.ai/docs/skills/>).
/// - Goose: `.agents/skills/` (recommended standard), also `.goose/skills/` and `.claude/skills/`
///   (<https://goose-docs.ai/docs/guides/context-engineering/using-skills>).
/// - Cline: `.cline/skills/` (recommended), `.clinerules/skills/`, `.claude/skills/`
///   (<https://docs.cline.bot/customization/skills>).
/// - Kiro: `.kiro/skills/` (<https://kiro.dev/docs/skills/>); steering lives in `.kiro/steering/`
///   and Kiro also loads a workspace-root `AGENTS.md` (<https://kiro.dev/docs/steering/>).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentTarget {
    /// `.claude/skills` (Claude Code; also read by OpenCode, Goose, Cline).
    ClaudeCode,
    /// `.agents/skills`, the cross-agent standard: Codex, Gemini CLI, OpenCode, Goose.
    /// `materialize` always writes this one because the index block points at it.
    Agents,
    /// Alias of [`AgentTarget::Agents`] kept for call-site clarity: Codex reads only `.agents/skills`.
    Codex,
    /// `.gemini/skills` (Gemini CLI's own dir; `.agents/skills` also works there).
    Gemini,
    /// `.opencode/skills`.
    OpenCode,
    /// `.goose/skills` (backward-compatible dir; `.agents/skills` is the recommended one).
    Goose,
    /// `.cline/skills`.
    Cline,
    /// `.kiro/skills`, plus a steering file `.kiro/steering/tandem-skills.md` carrying the index.
    Kiro,
}

impl AgentTarget {
    pub const ALL: [AgentTarget; 8] = [
        AgentTarget::ClaudeCode,
        AgentTarget::Agents,
        AgentTarget::Codex,
        AgentTarget::Gemini,
        AgentTarget::OpenCode,
        AgentTarget::Goose,
        AgentTarget::Cline,
        AgentTarget::Kiro,
    ];

    /// Skills directory relative to the worktree root.
    pub fn skills_dir(self) -> PathBuf {
        PathBuf::from(match self {
            AgentTarget::ClaudeCode => ".claude/skills",
            AgentTarget::Agents | AgentTarget::Codex => ".agents/skills",
            AgentTarget::Gemini => ".gemini/skills",
            AgentTarget::OpenCode => ".opencode/skills",
            AgentTarget::Goose => ".goose/skills",
            AgentTarget::Cline => ".cline/skills",
            AgentTarget::Kiro => ".kiro/skills",
        })
    }

    /// Extra index file this target reads in addition to `AGENTS.md` / `CLAUDE.md`.
    pub fn steering_file(self) -> Option<PathBuf> {
        match self {
            AgentTarget::Kiro => Some(PathBuf::from(".kiro/steering/tandem-skills.md")),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            AgentTarget::ClaudeCode => "claude",
            AgentTarget::Agents => "agents",
            AgentTarget::Codex => "codex",
            AgentTarget::Gemini => "gemini",
            AgentTarget::OpenCode => "opencode",
            AgentTarget::Goose => "goose",
            AgentTarget::Cline => "cline",
            AgentTarget::Kiro => "kiro",
        }
    }

    pub fn from_name(s: &str) -> Option<AgentTarget> {
        AgentTarget::ALL.into_iter().find(|t| t.name().eq_ignore_ascii_case(s))
    }
}
