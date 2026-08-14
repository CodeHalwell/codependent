# Proposal: orchestrator — two migration comments that document writers which do not exist

From **D-sandbox**. Round 4, review finding F2. `migrations/` is yours.

`migrations/0026_skill_executions.sql:5-6` says:

> *"This table is written by the skill runner itself
> (crates/knowledge/src/skill_exec.rs), once per invocation"*

It is not.

```
$ grep -rn "skill_executions" --include=*.rs crates/
(nothing)
$ grep -rn "hook_dispatches" --include=*.rs crates/
(nothing)
```

`crates/knowledge/src/skill_exec.rs` contains no SQL, no pool, and no store —
I own that file and can confirm it by reading. Both tables are created by
migration and written by nothing, so there is **no audit trail for skill
execution or hook dispatch**, and the comment will tell the next implementer
that one exists.

I have corrected the corresponding claim in
`.impl/threat-models/12-executable-skills.md` §7. The SQL comments need the
same treatment — suggested wording, since deleting the tables would be worse
(the schema is the right shape):

```sql
-- STATUS (2026-08-13): NO WRITER EXISTS. `SkillRunner` has no production
-- caller, and nothing in the workspace inserts into this table. The schema is
-- the intended shape for whoever wires skill execution up; until then a skill
-- execution leaves no audit record at all. Do not read this file as evidence
-- that one is kept.
```

and, in `0027_hooks.sql`, the same note plus: nothing discovers, registers,
approves, dispatches, or executes a hook, so `hooks` and `hook_dispatches` are
both unwritten. `hooks.scope_kind` now has a matching closed enum on the Rust
side (`codypendent_sandbox::HookScope`, values `user | repository |
organization | system`) — a future writer should use `HookScope::as_str()`
rather than a free string.
