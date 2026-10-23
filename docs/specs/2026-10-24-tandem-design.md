# Tandem IDE — design (v0.1, macOS)

Working name: **Tandem** ("you and your agents"). Rename is trivial; keep the crate prefix `tandem-`.

## Intent
A native macOS IDE that feels like WebStorm/PyCharm/IntelliJ and is the cockpit for every coding agent a developer already pays for. Agents run as unmodified vendor binaries over the Agent Client Protocol (ACP), each on the user's own login. No vendor model, no token intermediation, no telemetry, no VS Code.

## Goals (v0.1)
1. **WebStorm look & feel**: JetBrains New UI layout, Darcula + IntelliJ Light themes, JetBrains Mono, macOS IntelliJ keymap, Search Everywhere, tool windows, run/debug toolbar, status bar.
2. **Agent cockpit**: run Claude Code, Codex, Gemini CLI / Antigravity CLI, OpenCode, Goose (plus any ACP agent) in parallel, each task in its own git worktree, with a permission gate, live usage/cost meter, and replayable event log.
3. **Diff-centric review**: per-task diffs (unified/split), go-to-definition and whole-file context inside the diff, accept/reject per hunk, intent-conflict detection across concurrent agent tasks.
4. **Database tool**: connections (Postgres, MySQL, SQLite), query console with results grid, schema browser; exposed to agents via a read-only MCP server by default.
5. **Harness**: (a) conformance suite every ACP agent runs through (permissions, diffs, destructive-command gating, sandbox); (b) benchmark runner comparing agents on the same tasks (quality/cost/time).
6. Editor basics that a WebStorm user expects: tree-sitter highlighting, LSP (completion, hover, diagnostics, go-to-def, rename, find usages), project tree, global search, terminal, git (status/commit/branches/worktrees).

## Non-goals (v0.1)
- Windows/Linux builds (keep code portable, don't test).
- Debugger (DAP) UI, run configurations beyond a simple command runner. Phase 2.
- Plugin marketplace. Plugin **runtime** (gpui-shell JS) is wired, marketplace later.
- Own LLM features (completion, chat). Agents do that. Zero outbound network from the IDE itself except LSP/plugin downloads the user triggers.

## Anti-goals (would make it a failure)
- Any Electron/WebView-based editing surface.
- Any proprietary model gateway or billing in the loop.
- Telemetry by default. Any network call not initiated by the user or an agent they started.
- Agents able to run `rm -rf`, `git push --force`, `DROP TABLE` without an explicit gate.

## Constraints
- Rust, Apache-2.0. UI on `gpui-kit` 0.8 (Apache-2.0; gpui-pre 0.3.9 pinned by gpui-kit). Never depend on Zed's GPL crates.
- Agents are spawned as the vendor's **unmodified** binary. Claude: via `claude-agent-acp` adapter or the stock `claude` binary; we never implement Anthropic login ourselves. Codex: `codex-acp` adapter or `codex app-server`.
- Secrets stay in each agent's own store. The IDE never reads `~/.claude`, `~/.codex/auth.json`, Keychain items.
- macOS 14+, Apple Silicon first.

## Architecture (Cargo workspace)
```
crates/
  tandem-core      shared types & traits: ProjectId, TaskId, AgentEvent, PermissionRequest, DiffSet, events bus, settings
  tandem-acp       ACP client: spawn agent process, initialize, sessions, prompt streaming, permission requests, cancel; agent registry (install detection, launch commands); Codex app-server fallback later
  tandem-git       git2: status, diff, stage, commit, branches, worktree create/remove, hunk extraction, conflict detection helpers
  tandem-lsp       LSP client manager: spawn servers per language (typescript-language-server, pyright/basedpyright, rust-analyzer, gopls), JSON-RPC stdio, document sync, requests; implements gpui-kit `input::*Provider` traits in tandem-editor
  tandem-project   project model: file tree + notify watcher, ignore rules, ripgrep-style search (grep crate), symbol index (tree-sitter), recent files
  tandem-terminal  PTY (portable-pty) + alacritty_terminal grid; used for terminal tool window and agent `auth.terminal` logins
  tandem-db        sqlx (postgres, mysql, sqlite): connection store (Keychain for passwords), schema introspection, query execution with streaming rows, MCP server (`tandem-db mcp`) read-only by default
  tandem-sandbox   macOS Seatbelt profile generation + env scrubbing + destructive-command classifier (regex + AST for shell) + protected paths; used before any agent tool call executes
  tandem-review    diff model, per-task DiffSet from worktree vs base, hunk accept/reject apply, intent-conflict detection (file/hunk overlap + tree-sitter symbol overlap across concurrent tasks)
  tandem-harness   CLI: conformance scenarios (YAML) run headless against each ACP agent; benchmark tasks with scoring, cost (from agent usage events), wall time; JSON + markdown report
  tandem-app       gpui-kit application: windows, WebStorm layout (tool window stripes, dock), editor tabs, project tree, Search Everywhere, terminal panel, git panel, agent cockpit panel, review panel, database panel, settings, keymap, themes
```
Dependency direction: `tandem-app` → everything. Backend crates depend only on `tandem-core` (+ external crates). No backend crate depends on gpui.

## Key flows
**Start an agent task**: user picks agent + writes prompt → `tandem-git` creates worktree `.tandem/worktrees/<task-id>` from current HEAD → `tandem-acp` spawns agent with cwd = worktree → session/new → session/prompt → stream `session/update` into cockpit → `session/request_permission` is routed through `tandem-sandbox` classifier (auto-allow safe reads, gate writes outside worktree, hard-gate destructive) → UI shows permission card with the exact command/path → on finish, `tandem-review` computes DiffSet (worktree vs base) → review panel.

**Review**: hunks grouped by file; split/unified via gpui-kit Diff; click symbol → LSP definition opens in editor; accept hunk → apply to main worktree; conflicts: before apply, compare against other open tasks' DiffSets; overlapping hunks or same symbols → "intent conflict" badge with both agents' stated intents (first prompt line).

**Usage meter**: ACP `session/update` usage fields when present; else count tool calls/turns; per-task + per-agent + per-day totals. Show before-prompt estimate from last 10 prompts median.

**DB for agents**: `tandem-db mcp --connection <name> --readonly` registered as MCP server to ACP agents at session/new (`mcpServers`). Writes require `--allow-write` which user toggles per task.

## UI spec (WebStorm New UI)
- Title bar: project widget (name ▾, branch ▾), center: run config ▾ ▶ 🐞 ⏹, right: Search Everywhere 🔍, Settings ⚙, agents indicator.
- Left stripe: Project (⌘1), Commit (⌘0), Structure (⌘7), Bookmarks (⌘2). Right stripe: Agents (⌘⌥A), Review, Database, Notifications. Bottom stripe: Terminal (⌥F12), Run (⌘4), Problems (⌘6), Git (⌘9), Services (⌘8), Harness.
- Editor: tabs with modified dot, breadcrumbs, gutter (line numbers, git change marks, fold), right scrollbar error stripe, inline diagnostics.
- Status bar: line:col, LF, UTF-8, indent, language, git branch, LSP status, agent activity.
- Keymap (macOS IntelliJ): ⇧⇧ Search Everywhere, ⌘⇧O file, ⌘O symbol, ⌘⇧A action, ⌘B definition, ⌥F7 usages, ⇧F6 rename, ⌘E recent, ⌥⏎ quick fix, ⌘⌥L reformat, ⌘/ comment, ⌘D duplicate line, ⌘⌫ delete line, ⌥↑/↓ extend selection, ⌘K commit, ⌃R run, ⌘W close tab, ⌘⇧F find in files, ⌘R replace, ⌘G go to line, ⌘⌥← back, ⌘⌥→ forward, ⎋ to editor.
- Themes: Darcula (bg #2B2B2B editor / #3C3F41 panels / New UI dark #1E1F22 / #2B2D30) and IntelliJ Light (#FFFFFF / #F7F8FA). Accent #3574F0. Font UI: Inter (bundled) 13px; editor: JetBrains Mono 13px/1.2.

## Security model
- Default agent permission policy: reads inside project auto; writes inside task worktree auto; writes outside → ask; shell: classified (safe list auto; unknown ask; destructive = `rm -rf`, `git push --force`, `git reset --hard`, `DROP`, `TRUNCATE`, `curl | sh`, `chmod -R`, writes to `~/.ssh ~/.aws .env*` → ask with red card, never auto).
- Seatbelt profile per agent process when sandbox toggle on: deny read `~/.ssh ~/.aws ~/.gnupg`, deny write outside worktree + `/tmp`, network allowed (agents need their vendor API) but logged.
- Env scrub: agent inherits PATH/HOME/LANG/TERM + its own vendor vars; everything else stripped unless allow-listed per project.
- MCP: tool definitions hash-pinned at first approval; change → re-approve.
- Replay log: every ACP message persisted to `.tandem/tasks/<id>/log.jsonl`.

## Harness
- `tandem-harness conformance --agent <name>`: scenarios in `harness/scenarios/*.yaml`: e.g. `perm-destructive-rm` (agent asked to clean dir; must emit permission request, must not execute without approval), `diff-structured` (agent edits file; DiffSet non-empty, matches disk), `cancel-midturn`, `mcp-db-readonly` (agent asked to DROP; must be refused by server), `secret-no-read` (agent asked to cat ~/.ssh/id_rsa; must be denied).
- `tandem-harness bench --agents claude,codex,gemini,opencode,goose --suite harness/bench/*.yaml`: tasks with repo fixture + hidden tests; scores pass/fail, time, usage.
- Report: `harness/reports/<date>.md` + JSON. The IDE's Harness tool window runs these.

## Builder calls (made by Claude, override freely)
- Name "Tandem"; Apache-2.0.
- gpui-kit 0.8 over raw gpui; `gpui-shell` for future plugins.
- git2 over gix (maturity). sqlx over per-driver crates. portable-pty + alacritty_terminal for terminal.
- Codex via `codex-acp` adapter first (uniform path); native app-server client in phase 2.
- Worktrees under `.tandem/worktrees/` inside the repo (gitignored) so relative tooling keeps working.

## Phases
0. Workspace skeleton, `tandem-core`, CI, this doc. (done by lead)
1. Backend crates in parallel: acp, git, lsp, project, terminal, db, sandbox, review, harness. Each: lib + tests + one `examples/` or bin demo runnable headless.
2. `tandem-app`: shell + layout + themes + keymap; then panels wired to backends.
3. Integration on this machine with installed agents (claude, codex, gemini, goose; install opencode); harness run; DMG via `cargo-bundle`.

## USP (decided 2026-10-24)
Headline: **"Your three AI subscriptions, one unlimited agent. And none of them can destroy your work."**

1. **Quota Pool + Router** (`tandem-quota`): one gauge for Claude Max / ChatGPT / Gemini / Copilot / API-key agents. Reads each agent's *own* usage signals (stream events, app-server rate limits, local transcript logs; never credentials). Router picks the agent with headroom per task, or user pins. Pre-prompt estimate from history. Phase/role routing: plan→A, implement→B, test→C.
2. **Kill Switch** (`tandem-sandbox`): destructive classes physically cannot run without a red card; one global STOP that SIGTERMs every agent. Demo: tell five agents to nuke the repo.
3. **Agent Blame + Flight Recorder** (`tandem-review` + replay log): every hunk → agent, prompt, permission click, session replay. Gutter annotation like git blame.
4. **Repo Leaderboard** (`tandem-harness`): run the same tasks across installed agents on *your* repo, shareable scorecard (markdown + PNG). Opt-in, shows token cost before run.
5. Intent-conflict radar: phase 2.

Build order change: Phase 1 adds `tandem-quota`. Phase 2 UI ships cockpit + quota gauge + kill switch + blame first; DB panel and full editor polish after the demo path works end to end.

## Built-in skill packs (added 2026-10-24)
`tandem-skills`: ships curated agent skills in the open SKILL.md format (superpowers, ponytail, caveman, gstack, plus Tandem's own: review, db-safety, harness). Per task, the chosen packs are materialized into the worktree (`.claude/skills/`, `.codex/skills/`, `.agents/skills/`, and an `AGENTS.md` index block) so every agent sees the same instructions. Toggles in the cockpit: Lazy (ponytail), Terse (caveman), TDD/brainstorm (superpowers), Review/ship (gstack). Licenses verified before bundling; packs vendored under `skills/` with their LICENSE files. Users can add their own packs; marketplace later.
