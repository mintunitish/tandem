---
name: tandem-review
description: "Diff-centric self-review checklist to run before you report a task done: intent conflicts with concurrent tasks, risky files, test coverage, and a reviewable diff. Use when you finish a change, before asking the user to accept hunks, or when asked to review a diff."
license: Apache-2.0
---

# Tandem review

Tandem shows your work to a human as a diff, hunk by hunk, next to diffs from other agents working in parallel. Make that review fast and safe. Before saying "done", run this checklist against `git diff <base>...HEAD` plus uncommitted changes.

## 1. State the intent
Write one sentence: what this change is supposed to do and why. If you cannot, the diff is not ready. Put it in your final message and in the commit body.

## 2. Check for intent conflicts
Other tasks run in sibling worktrees (`.tandem/worktrees/*`). The reviewer is warned when two tasks touch the same files, hunks, or symbols.
- Prefer small, local edits over reformatting or moving code you do not need to change.
- Do not rename, move, or reformat shared symbols (public functions, types, config keys, schema columns) unless that is the task. If you must, say so explicitly in the summary.
- Do not touch lockfiles, generated files, or vendored directories unless required. If a lockfile changed, say which dependency change caused it.

## 3. Flag risky files
Call out every hunk in these areas and say why it is needed:
- Secrets and environment: `.env*`, credentials, key material, CI secrets.
- Build and release: CI workflows, Dockerfiles, deploy scripts, package manifests, lockfiles.
- Data: migrations, schema files, SQL, seeds. Migrations must be reversible or explicitly marked one-way.
- Security boundaries: auth, permissions, sandbox profiles, input validation, crypto.
- Anything deleting files or data, or widening permissions.

## 4. Tests
- New behavior has a test that fails without your change.
- Run the project's tests that cover the touched code and report the exact command and result. Never claim "tests pass" without running them.
- Do not delete, skip, or weaken tests to get green. If a test is wrong, explain why in the summary.

## 5. Diff hygiene
- No debug prints, commented-out code, stray TODOs, or unrelated formatting churn.
- No files outside the task's scope. Remove scratch files.
- Each commit is one logical step with a message that says why.

## 6. Report
Finish with: intent (one sentence), files changed grouped by risk, test command and result, anything you were unsure about, anything you chose not to do.
