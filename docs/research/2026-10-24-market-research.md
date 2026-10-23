# Market research — AI IDEs & coding agents (2026-10-24)

## Landscape
- VS Code forks: Cursor (SpaceX, $60B), Windsurf→Devin Desktop, Kiro (AWS), Trae (ByteDance), Antigravity 1.0 (Google). Antigravity 2.0 is a standalone agent platform + Go CLI (`agy`).
- Native: Zed 1.0 (Rust/gpui, ACP author), JetBrains (ACP in 2026.1, Air multi-agent in 2026.3 EAP), Xcode 26.3 (Claude Agent + Codex built in), Warp.
- Open-source VS Code agents collapsing: Roo Code archived (May 2026), Continue read-only, Void paused, Aider maintenance. Survivors: Cline, Kilo (Anaconda), OpenCode, Goose.
- Parallel agents in git worktrees = table stakes: Cursor 3 Agents Window, Codex app, Claude Code desktop, Zed, JetBrains Air, Conductor (Tauri, mac).
- Pricing ladder $20 / $60-100 / $200 everywhere. Usage-limit rage is the #1 complaint for every vendor.

## Protocols & auth
- **ACP** (Agent Client Protocol, agentclientprotocol.com): JSON-RPC over stdio, v1 stable, v2 draft (structured diffs, richer permissions). Rust/TS/Python/Kotlin SDKs. ~46 agents: Claude Agent (`claude-agent-acp`), Codex (`codex-acp`), Gemini CLI (`--experimental-acp`), Copilot, Goose, OpenCode, Cline (`--acp`), Kiro (`kiro-cli acp`), Junie, Qwen Code, Cursor, Droid, OpenHands, Mistral Vibe. Client spawns agent subprocess; agent owns login (`auth.terminal`). IDE never touches tokens.
- **Codex**: Apache-2 Rust CLI; `codex app-server` JSON-RPC (Thread/Turn/Item, approvals, steer/interrupt, typed schema via `codex app-server generate-ts`). ChatGPT-subscription login in third-party clients explicitly allowed.
- **Claude Code**: Pro/Max subscription usable ONLY via the unmodified `claude` binary with the user's own `/login`. Third-party apps may not wrap SDK login, intermediate tokens, or route via subscription credentials (docs: code.claude.com/docs/en/legal-and-compliance). Enforced Jan–Apr 2026 (OpenCode/Cline cut off). Hosting the stock binary via ACP adapter = allowed. Confirm in writing before launch.
- **Gemini CLI** free tier ended 2026-06-18 → Antigravity CLI `agy` (closed source). Gemini CLI still works with API key / Code Assist.
- Sandboxing: Anthropic `@anthropic-ai/sandbox-runtime` (Seatbelt + egress proxy + secret masking, open source); Codex uses Seatbelt profiles; Apple `container` CLI for VM isolation.
- MCP risks: tool poisoning, rug pulls (pin tool-definition hashes), plaintext creds.

## Top complaints (ranked)
1. Pricing/limits rage; opaque metering, no cost preview before a prompt.
2. "Not another VS Code fork" (Antigravity HN: 1,085 comments, 100+ fork mentions).
3. Can't use my Claude Max / ChatGPT sub inside my IDE; lock-in; Windsurf lost Claude overnight.
4. Agents wiping drives / prod DBs (Antigravity drive wipe, Cursor PocketOS DB, Replit).
5. Prompt injection via rules files, MCP, recommended extensions (CVE-2025-54135, CVE-2025-53773).
6. Telemetry (Trae 500 calls / 7 min after opt-out), forced logins (Warp).
7. Review bottleneck: transcript-centric, "5x longer to review, don't understand what I approve".
8. Multi-agent chaos: merge/intent conflicts (27.7% conflict rate across 142k agent PRs), babysitting.
9. Context loss, agents breaking working code.
10. Missing real IDE: JetBrains debugger/refactoring/DB/indexing unmatched.
11. Memory bloat (Cursor 50 GB), Zed slowdown with ACP.
12. Open VSX gaps (no ms-python). 13. CLI-vs-IDE friction. 14. Spec overhead (Kiro). 15. Vendor churn. 16. Forced AI, no real off switch.

## What people wish existed
- Own subscriptions everywhere, local LLMs too.
- Per-prompt cost preview and caps they control.
- "Power of Cursor inside WebStorm": JetBrains-grade debugger/refactor/DB + multi-file agent edits.
- Agent cockpit: monitor agents, worktrees built in, review per task, diff-centric with go-to-def and whole-file context.
- Intent-conflict tooling for parallel agents, not just worktrees.
- Safety runtime: execution boundaries, traces, replay, hard "no" on destructive commands, sandboxed MCP, egress control.
- Real AI-off switch that stops network calls.

Sources: see session research; key URLs: agentclientprotocol.com, zed.dev/blog/acp-progress-report, jetbrains.com/acp, code.claude.com/docs/en/legal-and-compliance, learn.chatgpt.com/docs/app-server, news.ycombinator.com/item?id=45967814, forum.cursor.com/t/allow-claude-max-claude-code-subscription-access-in-cursor/163716, news.ycombinator.com/item?id=46103532, news.ycombinator.com/item?id=49934037.
