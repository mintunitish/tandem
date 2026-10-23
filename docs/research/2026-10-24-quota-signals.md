# Quota / usage signals per agent (2026-10-24)

Rule: never read credentials, never call vendor OAuth usage endpoints with the user's token.

## Claude Code 2.1.309
- Per-message usage: `claude -p --output-format stream-json --verbose` assistant events `message.usage` (input/output/cache_creation/cache_read tokens). Same in `~/.claude/projects/<slug>/*.jsonl` (undocumented format).
- Final cost: `result` event `total_cost_usd` (list-price estimate), `usage`, `permission_denials`.
- `rate_limit_event` in stream-json: `rate_limit_info{status: allowed|allowed_warning|rejected, resetsAt, rateLimitType: five_hour|seven_day, overageStatus, isUsingOverage}`; `utilization` only on warning/rejected.
- **Percentages: status-line JSON on stdin** (docs: code.claude.com/docs/en/statusline): `rate_limits.five_hour.{used_percentage, resets_at}`, `rate_limits.seven_day.{...}`, `context_window.{used_percentage, remaining_percentage}`, `cost`. Inject a statusline command via `--settings` that writes JSON to a file/socket. UNVERIFIED whether statusline fires under `-p`/ACP; test.
- `/usage` TUI only (calls `/api/oauth/usage` with token; do not replicate).
- Fallback: ccusage-style 5-hour block aggregation of JSONL (heuristic).

## Codex 0.147.0 (verified via `codex app-server generate-json-schema`)
- `account/rateLimits/read` → `rateLimits{limitId, limitName, planType, primary{usedPercent, windowDurationMins, resetsAt}, secondary{...}, credits{balance, hasCredits, unlimited}, spendControlReached}`; push `account/rateLimits/updated`.
- `thread/tokenUsage/updated` → total/last {inputTokens, cachedInputTokens, cacheWriteInputTokens, outputTokens, reasoningOutputTokens, totalTokens, modelContextWindow}.
- `account/usage/read` → lifetime/daily buckets.
- `codex exec --json` `turn.completed.usage`. Session logs `~/.codex/sessions/YYYY/MM/DD/*.jsonl` rows `event_msg.payload.type=token_count` with `rate_limits{primary{used_percent, window_minutes, resets_at}, secondary, plan_type}` (offline fallback).

## Gemini CLI 0.46 / Antigravity CLI `agy`
- `gemini -p -o stream-json` → `result` event stats (token counts per model; sub-fields undocumented).
- `/stats` quota is TUI-only via internal `retrieveUserQuota` (`remainingFraction`, `resetTime` per model) on cloudcode-pa.googleapis.com, needs OAuth token → opt-in only, fragile.
- `agy -p --output-format stream-json` supported; `/usage` `/credits` interactive only.

## Others
- Goose 1.32: `goose acp`; `~/.local/share/goose/sessions/sessions.db` has per-session token columns. Quota = upstream provider.
- OpenCode: ACP `usage_update` reported (unverified). Copilot: monthly premium requests via GitHub (gh). Kiro: `/usage` credits, ACP reporting unverified. Cline: unknown.

## ACP
- `session/update` `sessionUpdate:"usage_update"` stable (2026-06-05): `size`, `used` (context tokens), optional `cost{amount,currency}`. Context, not quota.
- `PromptResponse.usage` behind `unstable_end_turn_token_usage` (draft): totalTokens/input/output/thought/cachedRead/cachedWrite.
- No rate-limit windows in ACP; quota comes from agent-specific channels.

## Design → tandem-quota
Providers: ClaudeProvider (stream tap + statusline sidecar + JSONL fallback), CodexProvider (app-server rateLimits read/updated + session JSONL fallback), AcpGenericProvider (usage_update), GeminiProvider (result.stats only), GooseProvider (sessions.db). Event-driven first, reconcile after each turn, file watch fallbacks at 5 s debounce, expire windows past reset_at. Degradation order: AgentRpc > AgentEvent > LocalLog > Heuristic. Router: pick agent with max headroom on its tightest window, subject to user pins and phase preferences.
