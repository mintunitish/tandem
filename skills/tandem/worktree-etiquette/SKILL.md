---
name: tandem-worktree-etiquette
description: "How to behave inside a Tandem task worktree: stay in your worktree, never touch .tandem, commit small, and write a one-line intent first. Use at the start of every Tandem task."
license: Apache-2.0
---

# Tandem worktree etiquette

Your task runs in its own git worktree, usually `.tandem/worktrees/<task-id>`, on its own branch. Other agents run in sibling worktrees at the same time. These rules keep everyone's work reviewable and recoverable.

1. **Write your intent first.** Your first message or first commit-less step is a single line: `Intent: <what you will change and why>`. Update it if the plan changes.
2. **Stay in your worktree.** Read and write only inside the current working directory. Do not `cd ..`, do not edit the main checkout or sibling worktrees, do not use absolute paths that point outside the worktree. Reading shared documentation is fine; writing is not.
3. **Never touch `.tandem/`.** That directory holds Tandem's state: task logs, replay data, worktrees, config. Do not read secrets from it, edit it, delete it, or add it to commits. It is gitignored; keep it that way.
4. **Do not touch your own git plumbing.** No `git worktree add/remove`, no switching branches, no `git reset --hard`, no force-push, no rebasing onto other tasks' branches. Ask the user instead.
5. **Commit small.** One logical change per commit, imperative subject of 72 characters or fewer, body explains why. Commit when a step works, not at the end. Never commit build output, scratch files, or secrets.
6. **Do not push** unless the user asked. The review step happens in Tandem first.
7. **Keep the tree clean.** Remove temp files you created. Run the tests that cover your change before reporting done.
8. **Respect the permission cards.** If an action needs approval, wait. A denied action stays denied; do not retry it in another form.
9. **Report when finished.** End with the intent, what changed, how you verified it, and open questions.
