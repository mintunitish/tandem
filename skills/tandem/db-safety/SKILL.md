---
name: tandem-db-safety
description: "Rules for touching databases from a Tandem task: never run dangerous SQL without the red card, prefer the read-only connection, explain before you mutate. Use whenever a task involves SQL, a database MCP tool, migrations, or a connection string."
license: Apache-2.0
---

# Tandem database safety

Tandem exposes databases to agents through a guarded connection. The guard classifies every statement. You must cooperate with it, not route around it.

## Rules

1. **Read-only first.** Use the read-only connection or tool for exploration: `SELECT`, `EXPLAIN`, `SHOW`, `DESCRIBE`, schema introspection. Do not ask for a read-write connection until you have a concrete change to make.
2. **Explain before you mutate.** Before any `INSERT`, `UPDATE`, `DELETE`, DDL, or migration, tell the user: the exact statement, the target database and table, the expected number of affected rows (from a `SELECT COUNT(*)` with the same `WHERE`), and how to undo it. Then wait for approval when the permission card appears.
3. **Dangerous SQL needs the red card.** `DROP`, `TRUNCATE`, `DELETE` or `UPDATE` without a `WHERE`, `ALTER ... DROP`, privilege changes, and anything run against a connection marked production are Dangerous. Never run them silently, never split them into smaller statements to look safe, never wrap them in a script or `psql -c` / `mysql -e` / `sqlite3` shell-out to avoid the guard. Request them through the normal tool call so the red card is shown, and accept a refusal as final.
4. **Prefer migrations to ad-hoc DDL.** Schema changes go through the project's migration tool as files in the worktree, reviewable in the diff. Make them reversible when the tool supports it.
5. **Test on a copy.** Run risky changes against a local or scratch database (for example SQLite or a throwaway container), not a shared one.
6. **Never print secrets.** Do not echo connection strings or passwords into the chat, commits, or files. Refer to connections by their configured name.
7. **Transactions.** Wrap multi-statement changes in a transaction and show the result before committing when the tool allows it.

## If the guard refuses

A refusal is a decision, not an error to work around. Say what was refused and why, and propose a safer alternative (narrower `WHERE`, soft delete, a migration file, or asking the user to run it).
