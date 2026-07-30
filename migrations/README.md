# Migrations are IMMUTABLE once merged

`sqlx::migrate` records a SHA-384 checksum of each applied migration in the
database's `_sqlx_migrations` table and **refuses to boot** if a file changes
after it was applied anywhere (`migration N was previously applied but has
been modified`). This includes comment-only and whitespace edits — the
checksum covers every byte.

This is not hypothetical: a comment clarification to `0003_phase2.sql`
(2026-07-30) hard-failed daemon startup on every pre-existing database.

Therefore, once a migration has merged to `main`:

- **Never edit it.** Not comments, not formatting, nothing.
- Fixes and clarifications go in a NEW migration (or in docs — e.g. the
  `codypendent index --help` text) — never in the applied file.
- Schema changes are always a new, next-numbered file.

When authoring a new migration, get the comment right the first time; after
merge, the file is frozen.
