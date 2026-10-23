---
name: gstack-careful
description: "Safety guardrails for destructive commands. (gstack)"
license: MIT
metadata:
  upstream: https://github.com/garrytan/gstack
  upstream-name: careful
---

> **Tandem vendoring note.** Vendored from gstack (MIT, (c) Garry Tan, https://github.com/garrytan/gstack @ a3259400). The upstream Claude-Code-specific boilerplate (telemetry, session tracking, update checks, plan-mode hooks, learnings/brain sync) was removed; the workflow itself is unchanged. Paths below are relative to this skill's directory; sibling skills live at `../<id>/` (ids are prefixed `gstack-`). `$GSTACK_ROOT` is an optional upstream gstack install (default `~/.claude/skills/gstack`) needed only for optional helpers such as the `browse` binary; skip any step that needs it when it is absent. "Ask the user" means a plain chat question when no structured question tool exists.

## When to invoke this skill

Warns before rm -rf, DROP TABLE,
force-push, git reset --hard, kubectl delete, and similar destructive operations.
User can override each warning. Use when touching prod, debugging live systems,
or working in a shared environment. Use when asked to "be careful", "safety mode",
"prod mode", or "careful mode".

# /careful — Destructive Command Guardrails

Safety mode is now **active**. Every bash command will be checked for destructive
patterns before running. If a destructive command is detected, you'll be warned
and can choose to proceed or cancel.


## What's protected

| Pattern | Example | Risk |
|---------|---------|------|
| `rm -rf` / `rm -r` / `rm --recursive` | `rm -rf /var/data` | Recursive delete |
| `DROP TABLE` / `DROP DATABASE` | `DROP TABLE users;` | Data loss |
| `TRUNCATE` | `TRUNCATE orders;` | Data loss |
| `git push --force` / `-f` | `git push -f origin main` | History rewrite |
| `git reset --hard` | `git reset --hard HEAD~3` | Uncommitted work loss |
| `git checkout .` / `git restore .` | `git checkout .` | Uncommitted work loss |
| `kubectl delete` | `kubectl delete pod` | Production impact |
| `docker rm -f` / `docker system prune` | `docker system prune -a` | Container/image loss |

## Safe exceptions

These patterns are allowed without warning:
- `rm -rf node_modules` / `.next` / `dist` / `__pycache__` / `.cache` / `build` / `.turbo` / `coverage`

## How it works

The hook reads the command from the tool input JSON, checks it against the
patterns above, and returns `permissionDecision: "ask"` with a warning message
if a match is found. You can always override the warning and proceed.

To deactivate, end the conversation or start a new one. Hooks are session-scoped.


## Enforcement outside Claude Code (Tandem note)

Upstream enforces this skill with a Claude Code `PreToolUse` hook. Other agents have no such hook, so you must self-enforce: before every shell command or file edit, run the matching check script from this skill's `bin/` directory (`bin/check-careful.sh` / `bin/check-freeze.sh`; they read the tool-input JSON on stdin) or apply the same rules by hand, and stop to ask the user whenever a rule trips. Tandem's own sandbox and permission cards remain the hard boundary.
