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

## Corrections to frozen files

A merged migration's comment cannot be edited, so where one has turned out to
be wrong it is corrected here instead. Trust this section over the SQL comment.

### `0026_skill_executions.sql` and `0027_hooks.sql` — no writer exists

`0026`'s header says the table "is written by the skill runner itself
(`crates/knowledge/src/skill_exec.rs`), once per invocation". **It is not, and
as of 2026-08-14 neither table has any writer at all:**

```
$ grep -rn "skill_executions" --include=*.rs crates/   # nothing
$ grep -rn "hook_dispatches"  --include=*.rs crates/   # nothing
```

`SkillRunner` has no production caller (`crates/daemon/src/policy_gate.rs:22-29`
says so itself), and nothing discovers, registers, approves, dispatches or
executes a hook. So a skill execution and a hook dispatch each leave **no audit
record**. The schemas are the intended shape for whoever wires these up; do not
read either file as evidence that an audit trail is being kept.

A future writer for `hooks.scope_kind` should use
`codypendent_sandbox::HookScope::as_str()` (`user`/`repository`/`organization`/
`system`) rather than a free string — the Rust side is a closed enum as of
round 4, and the column is not.
