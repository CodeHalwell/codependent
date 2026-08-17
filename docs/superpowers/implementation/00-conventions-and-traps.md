# Implementation Conventions and Traps

**Audience:** anyone implementing a milestone of
[`plans/2026-08-16-hybrid-platform-program.md`](../plans/2026-08-16-hybrid-platform-program.md).
**Status:** living document · last verified against the tree on 2026-08-17.

The plan tells you *what* to build and the design spec tells you *why*. This file tells you the
things that are true about **this repository** and that will otherwise cost you a red CI run, a
silently dead feature, or a security review finding. Every item here has actually bitten someone.

Read this once before your first milestone. Skim it again before each PR.

---

## 1. The single most expensive trap: a shipped command nobody can call

`role_permits` in `crates/daemon/src/commands.rs` ends in `_ => false`. A new `CommandBody`
variant that nobody adds an arm for is **role-denied for every client**, and the failure is
invisible: the type exists, the handler exists, the handler's unit tests pass (they call the
handler directly and bypass the gate), and the feature is simply unreachable over the wire.

This has shipped **three times**: `RunUserShell`, `RememberMemory`, and `ReadArtifact`.

There is now a guard test — `every_client_issued_command_has_a_decided_role_floor` in
`crates/daemon/src/commands.rs` — which asserts every client-issued body is permitted by at least
one role. **Add your new command to that test's list.** It only protects what it enumerates.

**Checklist for every new client-issued command:**

1. Variant added to `CommandBody` (`crates/protocol/src/command.rs`).
2. Arm added to `role_permits` with a *decided* role floor (not copy-pasted).
3. Entry added to the guard test above.
4. `named_resources()` returns the real resources it names — **not an empty `Vec`**. An empty vec
   makes the ownership gate pass vacuously, which is how `ListSessions` and
   `SearchWorkspaceFiles` once leaked across users.
5. Ownership resolved in the gate (see §2).
6. Removed from `is_reserved_unsupported_command` if it was reserved there (see §3).
7. A golden vector added under `protocol-vectors/` — and then see §5, because that has its own
   trap.

## 2. Unauthorized and absent must be indistinguishable

A program rule, and it is enforced in shipped code you should copy rather than reinvent:

- `principal_owns_repository` (`crates/daemon/src/server.rs`) — component-wise comparison over
  canonicalized paths, so neither `..` nor a symlinked prefix escapes the owned root.
- The `NamedResource::Artifact` arm in the same file — `owner.unwrap_or(daemon_uid) ==
  principal.uid()`, with a **generic not-found** refusal so the error cannot be used as an
  existence oracle. Note the `unwrap_or(daemon_uid)`: rows predating an ownership migration are
  adopted by the daemon uid rather than being treated as unowned.

The rule extends to **counts, traversal, pagination, and error bodies** — a filtered count that
differs from an unfiltered one leaks existence just as effectively as a 403.

## 3. `is_reserved_unsupported_command` is your to-do list *and* your done signal

`crates/daemon/src/commands.rs` contains a function listing every payload M1.3 reserved but has
not implemented — inbox, analytics, automation bindings, bundles, editor actions. They all return
`protocol.unsupported-payload` today. This is **deliberate**, per the plan: *"Reserve payloads and
capability bits even when implementations land in later milestones; return explicit unsupported
errors until then."*

Two consequences:

- It is the cleanest inventory of remaining work in the repo. Read it first.
- **Implementing a milestone means deleting entries from it.** If your milestone is "done" but its
  variants are still listed there, the feature is still returning `unsupported` to every client.

## 4. Migrations: two sequences, different rules

| | Root `migrations/` | `crates/control-plane/migrations/` (M7+) |
|---|---|---|
| Engine | SQLite | PostgreSQL |
| Numbering | continues the existing sequence (0040 is the highest today) | starts independently at `0001` |
| Rule | **append-only and checksum-gated** | forward-only |

Root migrations are pinned by `migrations/checksums.json` and enforced by
`.github/scripts/check_migration_immutability.py`. **Never edit, delete, or renumber a committed
migration** — regenerate checksums only for genuinely appended files. There is a known historical
violation (a changed checksum on `0003_phase2.sql`) that makes databases from
`v0.1.0-build.43/.44/.45` unopenable; do not add a second one.

Assign migration numbers **centrally per release** when several people are working in parallel —
collisions have happened (three specs once all claimed `0034`).

## 5. The two CI gates that `cargo test` does not run

These are invisible locally and will turn your PR red after you think you are done.

**`doc-counts` job** runs two scripts:

```bash
python3 .github/scripts/check_doc_test_counts.py --skip-vitest   # test-count markers
python3 .github/scripts/check_docs_manifest.py                   # docs/MANIFEST.json
```

- Adding **any** Rust test drifts `<!-- doc-count:test … expect=N -->` markers in `ROADMAP.md`.
  Fix both the `expect=N` **and** the prose number next to it.
- Adding or removing **any** file under `docs/` fails the manifest check. Fix with
  `python3 .github/scripts/check_docs_manifest.py --fix` and commit the regenerated manifest.
- Markers must describe the **committed** tree, not your working tree. If you have uncommitted
  work that adds tests, the number you commit must be the number CI will measure.

**`extension` job** runs the vitest markers *and* the protocol-vector partition test:

- The `doc-count:vitest` markers (`VS Code vitest suite` / `files`) are **deferred** by
  `--skip-vitest`, so a local run reports OK while CI fails. Adding a vitest `it(...)` — including
  one generated by adding a vector to a partition list — drifts them.
- Every new vector in `protocol-vectors/*.json` must be added to either the `modeled` or the
  `notModeled`/`passthrough` list in `extensions/vscode/test/protocol-vectors.test.ts`, or
  `assertPartitionIsComplete` fails. Commands only the TUI/CLI issue belong in the exclusion list.
  `sdk/protocol`'s own suite auto-discovers the vector directory, so it needs no such edit.

## 6. Generated contracts, and the mirrors that predate them

M1.1/M1.2 shipped real generation: `crates/protocol/src/bin/export_schema.rs` →
`sdk/protocol/schema/*.schema.json` → `sdk/protocol/scripts/generate.mjs` →
`sdk/protocol/src/generated/`, gated by `.github/scripts/check_generated_protocol.sh`.

The program rule is: **generate TypeScript contracts from Rust schemas; never add another
hand-maintained mirror.** Honour it for anything new.

Be aware of the debt behind it: ~14 pre-existing modules in `sdk/protocol/src/` still carry a
`/** Mirrors crates/protocol/src/X.rs. */` header and were never migrated to generation, and
`extensions/vscode/src/protocol/` remains a separate hand-written mirror entirely. If you touch
one of those types, prefer migrating it to generation over deepening the mirror.

## 7. Reference implementations — copy these shapes

Do not design from scratch what already exists:

| You are building | Copy |
|---|---|
| An owner-scoped store + query service | `crates/daemon/src/session_library.rs` (1189 lines), `migrations/0040_session_library.sql`, `crates/daemon/tests/session_library_it.rs` |
| A sandboxed subprocess with a typed verdict | `crates/daemon/src/hook_engine.rs` + `hook_exec.rs` |
| Signed-artifact verification + trust | `crates/sandbox/src/manifest.rs`, `verify.rs` |
| OS confinement that fails closed | `crates/sandbox/src/executor.rs` — `enforcing_executor()` returns a typed error rather than an unconfined runner; callers must refuse, never fall back |
| Crash-consistent external effects | the `received` → effect → `applied` pattern in `crates/daemon/src/commands.rs`, and `resume_received`'s re-drive rules |
| A durable, replayable record | `crates/daemon/src/ledger.rs` — append-only; `append_next_event` claims its sequence *inside* the INSERT (prefer it over `next_sequence` + `append_event`, which race) |

## 8. Measurement honesty

Two invariants the codebase enforces and reviewers check:

- **Absent measurements stay absent.** Never coerce a missing token count, cost, or quality score
  to zero. `RunUsage` omits fields rather than reporting `0`.
- **Never record an effect that did not happen.** A past defect wrote a model-switch transition
  into the run trace on every failure, describing a switch the runtime never performed. If you
  compute a candidate action you cannot execute, say so in those words.

Related: a derived `DataClassification` may only ever **raise** sensitivity above the operator
ceiling, never lower it (`crates/codypendentd/src/routing.rs`). Anything that sends data off-device
must honour that ceiling.

## 9. Working practice

- **Every feature task must name and exercise a production caller.** A domain object with unit
  tests is not feature-complete — this is a program rule, and the reason the three unreachable
  commands in §1 all had passing tests.
- **Verify claims against the code, not against a document.** Both directions have been wrong
  here: the superpowers plans have **zero ticked checkboxes** despite M0/M1/M2.1 being shipped, so
  a plan file will happily lead you to redo finished work. Equally, `ROADMAP.md` has historically
  overclaimed. Grep before you build.
- **Watch for renamed or reverted work.** `Overlay::AddModelName` never existed (it shipped as
  `AddModelId`); the shortcuts footer bar was built and then deliberately deleted; `daemon_build_id`
  in the TUI header was reversed on purpose. A plan naming a symbol is not evidence the symbol
  exists.
- **Preserve unrelated work.** Before committing, inspect `git status --short`, `git diff`, and
  `git diff --cached`, and stage by explicit path. This tree regularly carries other people's
  in-flight and untracked files; `git add -A` will sweep them in. Check for `node_modules/` and
  `dist/` in new package directories — three shipped without `.gitignore`.

## 10. The gate, in order

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # --all-targets is mandatory: it is the
                                                        # difference between green and red CI
cargo test --workspace
npm --prefix sdk/ui run check                           # and sdk/protocol, sdk/remote-ui, apps/desktop
python3 .github/scripts/check_doc_test_counts.py --skip-vitest
python3 .github/scripts/check_docs_manifest.py
```

Green tests are necessary, not sufficient. Every release in this project's recent history passed a
fully green gate and still had real defects found by adversarial review afterwards — including a
data-classification inversion, three SDKs that were unreachable or fictional, and telemetry that
was always `None`. Budget for a review pass; do not treat green as done.
