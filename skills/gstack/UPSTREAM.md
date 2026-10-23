# Upstream

Vendored from https://github.com/garrytan/gstack (commit a3259400, 2026-07-14), MIT License, Copyright (c) 2026 Garry Tan. See `LICENSE`.

Changes by Tandem: skill ids are prefixed `gstack-` (frontmatter `name` and sibling paths); the generated Claude-Code preamble (telemetry, session/update checks, plan-mode hooks, learnings and brain sync, skill routing, AskUserQuestion format) was stripped; `~/.claude/skills/gstack/<skill>/` paths became skill-relative paths; `*.tmpl` sources and hook frontmatter were dropped (careful, freeze, guard gain a self-enforcement note). Optional gstack helper binaries (`bin/`, `browse`) are NOT vendored; install upstream gstack and set `$GSTACK_ROOT` to use them.
