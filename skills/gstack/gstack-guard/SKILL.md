---
name: gstack-guard
description: "Full safety mode: destructive command warnings + directory-scoped edits. (gstack)"
license: MIT
metadata:
  upstream: https://github.com/garrytan/gstack
  upstream-name: guard
---

> **Tandem vendoring note.** Vendored from gstack (MIT, (c) Garry Tan, https://github.com/garrytan/gstack @ a3259400). The upstream Claude-Code-specific boilerplate (telemetry, session tracking, update checks, plan-mode hooks, learnings/brain sync) was removed; the workflow itself is unchanged. Paths below are relative to this skill's directory; sibling skills live at `../<id>/` (ids are prefixed `gstack-`). `$GSTACK_ROOT` is an optional upstream gstack install (default `~/.claude/skills/gstack`) needed only for optional helpers such as the `browse` binary; skip any step that needs it when it is absent. "Ask the user" means a plain chat question when no structured question tool exists.

## When to invoke this skill

Combines /careful (warns before rm -rf, DROP TABLE, force-push, etc.) with
/freeze (blocks edits outside a specified directory). Use for maximum safety
when touching prod or debugging live systems. Use when asked to "guard mode",
"full safety", "lock it down", or "maximum safety".

# /guard — Full Safety Mode

Activates both destructive command warnings and directory-scoped edit restrictions.
This is the combination of `/careful` + `/freeze` in a single command.

**Dependency note:** This skill references hook scripts from the sibling `/careful`
and `/freeze` skill directories. Both must be installed (they are installed together
by the gstack setup script).


## Setup

Ask the user which directory to restrict edits to. Use ask-the-user:

- Question: "Guard mode: which directory should edits be restricted to? Destructive command warnings are always on. Files outside the chosen path will be blocked from editing."
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

Tell the user:
- "**Guard mode active.** Two protections are now running:"
- "1. **Destructive command warnings** — rm -rf, DROP TABLE, force-push, etc. will warn before executing (you can override)"
- "2. **Edit boundary** — file edits restricted to `<path>/`. Edits outside this directory are blocked."
- "To remove the edit boundary, run `/unfreeze`. To deactivate everything, end the session."

## What's protected

See `/careful` for the full list of destructive command patterns and safe exceptions.
See `/freeze` for how edit boundary enforcement works.


## Enforcement outside Claude Code (Tandem note)

Upstream enforces this skill with a Claude Code `PreToolUse` hook. Other agents have no such hook, so you must self-enforce: before every shell command or file edit, run the matching check script from this skill's `bin/` directory (`bin/check-careful.sh` / `bin/check-freeze.sh`; they read the tool-input JSON on stdin) or apply the same rules by hand, and stop to ask the user whenever a rule trips. Tandem's own sandbox and permission cards remain the hard boundary.
