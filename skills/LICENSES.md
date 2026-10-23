# Bundled skill pack licenses

Verified 2026-10-24 against the upstream repositories' LICENSE files. All four third-party packs are MIT and permit redistribution with the copyright notice; each pack directory carries its upstream `LICENSE` and an `UPSTREAM.md` describing the exact commit and every change Tandem made. No pack was withheld, so no "install from upstream" fetcher is needed today.

| Pack | Upstream | License | Copyright | Vendored snapshot | Notes |
|---|---|---|---|---|---|
| `superpowers` | https://github.com/obra/superpowers | MIT | (c) 2025 Jesse Vincent | v7.0.0, bb92a77 | 14 of 15 skills; omitted `diagnosing-superpowers` and `writing-skills/anthropic-best-practices.md` |
| `ponytail` | https://github.com/DietrichGebert/ponytail | MIT | (c) 2026 DietrichGebert | v4.7.0, 45f7d2f | 4 of 5 skills (no `ponytail-help`) |
| `caveman` | https://github.com/JuliusBrussee/caveman | MIT (snapshot) | (c) 2026 Julius Brussee | 84cc3c1 | 3 skills; upstream `main` now publishes Apache-2.0, also redistributable; re-check on upgrade |
| `gstack` | https://github.com/garrytan/gstack | MIT | (c) 2026 Garry Tan | a3259400 | 16 skills, ids prefixed `gstack-`, Claude Code preamble stripped; helper binaries not bundled |
| `tandem` | this repository | Apache-2.0 | Tandem contributors | n/a | review, db-safety, worktree-etiquette, harness |

MIT requires the copyright notice and permission text to travel with copies; `materialize` copies whole skill directories, and the pack-level `LICENSE` files live in `skills/<pack>/LICENSE`. Each vendored SKILL.md also carries `license:` frontmatter.
