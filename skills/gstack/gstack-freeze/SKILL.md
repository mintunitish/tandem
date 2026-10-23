---
name: gstack-freeze
description: "Restrict file edits to a specific directory for the session. (gstack)"
license: MIT
metadata:
  upstream: https://github.com/garrytan/gstack
  upstream-name: freeze
---

> **Tandem vendoring note.** Vendored from gstack (MIT, (c) Garry Tan, https://github.com/garrytan/gstack @ a3259400). The upstream Claude-Code-specific boilerplate (telemetry, session tracking, update checks, plan-mode hooks, learnings/brain sync) was removed; the workflow itself is unchanged. Paths below are relative to this skill's directory; sibling skills live at `../<id>/` (ids are prefixed `gstack-`). `$GSTACK_ROOT` is an optional upstream gstack install (default `~/.claude/skills/gstack`) needed only for optional helpers such as the `browse` binary; skip any step that needs it when it is absent. "Ask the user" means a plain chat question when no structured question tool exists.

## When to invoke this skill

Blocks Edit and
Write outside the allowed path. Use when debugging to prevent accidentally
"fixing" unrelated code, or when you want to scope changes to one module.
Use when asked to "freeze", "restrict edits", "only edit this folder",
or "lock down edits".

# /freeze — Restrict Edits to a Directory

Lock file edits to a specific directory. Any Edit or Write operation targeting
a file outside the allowed path will be **blocked** (not just warned).


## Setup

Ask the user which directory to restrict edits to. Use ask-the-user:

- Question: "Which directory should I restrict edits to? Files outside this path will be blocked from editing."
- Text input (not multiple choice) — the user types a path.

Once the user provides a directory path:

1. Resolve it to an absolute path:
```bash
FREEZE_DIR=$(cd "<user-provided-path>" 2>/dev/null && pwd)
echo "$FREEZE_DIR"
```

2. Ensure trailing slash and save to the freeze state file:
```bash
FREEZE_DIR="${FREEZE_DIR%/}/"
eval "$($GSTACK_ROOT/bin/gstack-paths)"
STATE_DIR="$GSTACK_STATE_ROOT"
mkdir -p "$STATE_DIR"
echo "$FREEZE_DIR" > "$STATE_DIR/freeze-dir.txt"
echo "Freeze boundary set: $FREEZE_DIR"
```

Tell the user: "Edits are now restricted to `<path>/`. Any Edit or Write
outside this directory will be blocked. To change the boundary, run `/freeze`
again. To remove it, run `/unfreeze` or end the session."

## How it works

The hook reads `file_path` from the Edit/Write tool input JSON, then checks
whether the path starts with the freeze directory. If not, it returns
`permissionDecision: "deny"` to block the operation.

The freeze boundary persists for the session via the state file. The hook
script reads it on every Edit/Write invocation.

## Notes

- The trailing `/` on the freeze directory prevents `/src` from matching `/src-old`
- Freeze applies to Edit and Write tools only — Read, Bash, Glob, Grep are unaffected
- This prevents accidental edits, not a security boundary — Bash commands like `sed` can still modify files outside the boundary
- To deactivate, run `/unfreeze` or end the conversation


## Enforcement outside Claude Code (Tandem note)

Upstream enforces this skill with a Claude Code `PreToolUse` hook. Other agents have no such hook, so you must self-enforce: before every shell command or file edit, run the matching check script from this skill's `bin/` directory (`bin/check-careful.sh` / `bin/check-freeze.sh`; they read the tool-input JSON on stdin) or apply the same rules by hand, and stop to ask the user whenever a rule trips. Tandem's own sandbox and permission cards remain the hard boundary.
