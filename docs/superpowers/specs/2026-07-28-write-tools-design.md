# File-editing tools — `workspace.write_file` & `workspace.edit_file` — design

**Date:** 2026-07-28 · **Status:** draft (pre-implementation) · **Branch:** `claude/write-tools`

## Problem

Today the **only** way an agent can change a file is `git.apply_patch`
(`crates/runtime/src/tools/git.rs:200-253`). It runs `git apply --check` first and
**refuses on any context-line mismatch**, returning `ToolError::PatchDoesNotApply`
(`git.rs:238-249`; the error is defined at `crates/runtime/src/tools/mod.rs:122-124`).
A unified diff must reproduce exact surrounding context lines and exact `@@` hunk
offsets. Small/local models — the target persona runs `glm-5.2:cloud` — are poor at
producing exact-context diffs, so a single off-by-one context line kills the whole
edit with no partial progress and no actionable hint.

The user chose to add **two** structured-argument tools that sidestep the diff format
entirely:

1. **`workspace.write_file`** — whole-file write (create a new file, or overwrite an
   existing one with full new contents). Ideal for new files and small rewrites.
2. **`workspace.edit_file`** — targeted **search/replace** edits. The model supplies
   one or more `{search, replace}` pairs as **structured JSON tool args** (not
   free-form Aider markers parsed from prose). For each pair, the exact `search` text
   is located in the file and replaced. Robust for editing large files without
   reproducing them whole.

Both must slot into the existing tool machinery with the **same worktree confinement,
the same policy/capability treatment, and the same mode gating** as `git.apply_patch`,
which this spec treats as the reference template.

## Goals

1. Add `workspace.write_file` and `workspace.edit_file` as **CORE tools in the offered
   baseline** (alongside `git.apply_patch`), dispatchable in every run kind.
2. **Worktree-confined** — a write can never land outside the run's disposable
   worktree (the security boundary). Reuse the existing root-containment mechanism.
3. **Same policy treatment as `apply_patch`** — the `FileWrite` capability via the
   existing write-evaluation path, gated by the mode overlay (Explore/Ask deny writes)
   and confined to the write scope. See [§ Verified: how `apply_patch` is
   gated](#verified-how-apply_patch-is-actually-gated) — this is auto-`Allow` **within
   the worktree**, reviewed as an end-of-run change-set, *not* a per-call human prompt.
4. **Atomic `edit_file`** — all edits succeed or the file is left untouched (no partial
   write).
5. **Actionable errors for a weak model** — a failed match says exactly what to fix
   ("not found" / "ambiguous — add more context").
6. **Honesty** — the returned observation reports what actually happened
   (created / overwrote / N edits applied), never a fabricated success.
7. **Prefer no protocol/wire/golden change** — reuse the existing `FileWrite`
   (`ProposedAction::WritePatch`) path. See [§ Protocol
   decision](#protocol-decision-reuse-writepatch).

## Non-goals

- No new external dependency (all resolution uses the existing `std`/`tokio` seams).
- No change to `git.apply_patch` (it stays; these tools are additive alternatives).
- No fuzzy / whitespace-insensitive / regex matching in `edit_file` — matches are
  **exact byte substrings**. (Fuzziness is a possible future iteration; the strict
  unique-match contract below is the robust weak-model baseline.)
- No line-range or offset addressing — `edit_file` addresses by content, not position.

## Verified: how `apply_patch` is actually gated

The task brief says "approval-gated like `apply_patch`." The code shows a subtlety
that the spec must model faithfully:

- `git.apply_patch`'s **own** `proposed_action()` (`git.rs:215-226`) returns
  `ExecuteCommand { program: "git", … }`, but the agent loop's `prepare` arm for
  `ApplyPatch::NAME` **overrides it**: it spills the patch text to an artifact and
  emits `ProposedAction::WritePatch { patch: <artifact id> }`
  (`crates/runtime/src/agent.rs:1532-1546`). So on the wire, applying a patch is a
  `WritePatch`, i.e. a **file write**, not a command.
- The policy engine routes `WritePatch` to `eval_write`
  (`crates/daemon/src/policy/mod.rs:270`, `352-373`). `eval_write` returns
  **`Decision::Allow`** with a `FileWrite` capability whenever the mode permits writes
  and the write scope is non-empty — it is **not** `RequireApproval`. This is pinned by
  `build_mode_write_is_allowed_in_worktree` (`policy/mod.rs:867-883`) and
  `explore_mode_cannot_write` (`policy/mod.rs:852-865`).

**Therefore `apply_patch` writes are auto-allowed into the disposable worktree.** The
real safety envelope is three layers, none of which is a per-write approval card:

1. **Worktree confinement** — writes land only in the run's throwaway worktree
   (`RunContext::worktree`), never the user's real tree.
2. **Mode overlay** — `Explore`/`Ask` set `write_allowed = false`, so `eval_write`
   **denies** every write in those modes (`policy/mod.rs:352-358`).
3. **End-of-run change-set review** — the loop's review node inspects the accumulated
   diff before anything leaves the worktree (`agent.rs:1183` `review_changeset`).

`RequireApproval` in `run_tool` (`agent.rs:1376-1444`) *does* exist and *would* fire
if the decision demanded it (e.g. `shell.run`, git commit/push, GitHub writes) — it is
simply not what governs a worktree file write. **`write_file` and `edit_file` inherit
this exact model**: reuse `WritePatch` → `eval_write` → auto-`Allow` in the worktree,
denied in read-only modes, reviewed as a change-set. This is the honest reading of
"same treatment as `apply_patch`," and [Open decision 1](#open-decisions) asks the
team to confirm it is the intended envelope (it matches `apply_patch` precisely).

## Architecture

Both tools follow the established four-part tool contract (`tools/mod.rs:1-27`): a
`NAME` constant, a `required_capabilities()` advertisement, a `proposed_action()`
builder the middleware evaluates, and an async `execute()` that takes a typed input
plus exactly the context it needs (here: the granted write `PathScope`). They are pure
filesystem writers — **no subprocess** — so unlike the git tools they need only
`CapabilityKind::FileWrite` (not `CommandExecute`).

```
model tool call {path, content|edits}
        │
        ▼
run_tool (agent.rs:1304)
  ├─ prepare(tool,args,run)                         ── agent.rs:1491
  │    parse_write_file / parse_edit_file           (root raw path at run.worktree)
  │    spill content/edits → artifact (sink.store)  (mirrors apply_patch prepare)
  │    action = ProposedAction::WritePatch{patch:id}
  ├─ policy.evaluate(action, eval_ctx)              ── eval_write → FileWrite, Allow-in-worktree
  │    (Deny in Explore/Ask via mode overlay)
  ├─ (Allow) emit ToolStarted{label}                ── tool_label(tool,args) → path
  └─ execute_prepared(prepared,run)                 ── agent.rs:1667
       write_scope = self.write_scope(run)          (file_write_scope; PathScope)
       WriteFile::execute(&input, &write_scope)  ──┐
       EditFile::execute(&input, &write_scope)   ──┤ resolve→classify→act, worktree-confined
                                                    ▼
                                        observation string + ToolOutcome
```

### Worktree-containment mechanism (reused)

Containment is the security boundary and is reused wholesale from the existing scope
machinery — **no new logic invented**:

- The parse helpers root a relative `path` at `run.worktree` exactly as
  `parse_read_file` does (`agent.rs:2255-2271`): `path.is_absolute() ? path :
  worktree.join(path)`. An absolute path is taken as given and still scope-checked.
- The granted write scope is `PolicyEngine::file_write_scope` (`policy/mod.rs:245-247`),
  surfaced by the loop as `self.write_scope(run)` (`agent.rs:2049-2051`) — the same
  `PathScope` `apply_patch` runs under (`execute_prepared`'s `write_scope`,
  `agent.rs:1782`).
- `PathScope::classify` (`crates/daemon/src/policy/scope.rs:81-91`) canonicalizes the
  path (deny-list wins, then root-containment via component-wise `is_within`) and
  returns `Allowed` / `Denied` / `OutsideRoots`. The tool maps `Denied →
  ToolError::PathDenied`, `OutsideRoots → ToolError::PathOutOfScope`, mirroring
  `ReadFile::guard_scope` (`read_file.rs:167-173`).

**Non-existent leaf (new file) resolution.** `read_file` calls the *strict*
`tokio::fs::canonicalize` first (`read_file.rs:80`), which requires the path to exist —
unusable for `write_file` creating a new file. The scope layer already has the right
primitive: `canonicalize_lenient` (`scope.rs:150-179`) canonicalizes the nearest
**existing** ancestor (resolving `..` and symlinks in that prefix) and re-appends the
not-yet-created remainder. `PathScope::classify` already uses it internally, so a
`classify(new_path)` call resolves the existing prefix's symlinks/`..` and then applies
root-containment — a traversal (`../../etc/passwd`) or a symlinked parent cannot escape.

**Resolve-once, act-on-the-same-path (TOCTOU).** To keep `read_file`'s no-TOCTOU
discipline (`read_file.rs:64-72`: check and act on the *same* resolved path), the write
tools must **classify and then write the identical resolved path**, not re-derive it.
`canonicalize_lenient` is `pub(crate)` to the daemon crate; the runtime tool cannot
name it. **Add a small additive public helper**
`PathScope::resolve(&self, &Path) -> (PathBuf, ScopeVerdict)` that returns the
lenient-canonical path *and* its verdict together (a two-line wrapper over the existing
`canonicalize_lenient` + `classify`). The tool then writes to the returned `PathBuf`.
This closes the check/act gap for the resolved *prefix*.

**Leaf-symlink guard.** A symlink planted *at the leaf* after the check could still
redirect a follow-through write outside the worktree. Close it directly: before
writing, `tokio::fs::symlink_metadata(target)`; if the target exists and
`file_type().is_symlink()`, refuse (`ToolError::NotRegularFile`). Write tools operate
on regular files, never through a symlink — this is both a security guard and good
hygiene, and it is unit-testable.

## Component decomposition (plan-task seeds)

Each item is an independently implementable, unit-testable task.

1. **`crates/daemon/src/policy/scope.rs` — `PathScope::resolve`.** Add
   `pub fn resolve(&self, path: &Path) -> (PathBuf, ScopeVerdict)` = `let c =
   canonicalize_lenient(path); (c.clone(), self.classify_canonical(c))` (refactor
   `classify` to share a `classify_canonical(&Path)` core so no double-canonicalize).
   Test: a new (non-existent) leaf under the root resolves+`Allowed`; a `..` escape
   resolves+`OutsideRoots`; a denied subpath resolves+`Denied`.

2. **`crates/runtime/src/tools/write_file.rs` — `WriteFile` tool (new module).**
   `NAME = "workspace.write_file"`; `required_capabilities() = &[FileWrite]`;
   `proposed_action(&WriteFileInput) ->` (see [§ Protocol
   decision](#protocol-decision-reuse-writepatch) — the artifact-spill happens in the
   loop's `prepare`, so this may return a placeholder that the loop overrides, exactly
   as `ApplyPatch::proposed_action` is overridden today); `execute(&WriteFileInput,
   &PathScope) -> Result<WriteFileOutcome, ToolError>`. Body: `resolve`+classify →
   leaf-symlink/existing-type guard → detect create-vs-overwrite via
   `symlink_metadata`/`metadata` → `create_dir_all(parent)` → `tokio::fs::write(target,
   content)`. Outcome carries `{ path, bytes_written, created: bool }`. Tests:
   [§ Testing](#testing).

3. **`crates/runtime/src/tools/edit_file.rs` — `EditFile` tool (new module).**
   `NAME = "workspace.edit_file"`; `required_capabilities() = &[FileWrite]`;
   `execute(&EditFileInput, &PathScope) -> Result<EditFileOutcome, ToolError>`. Body:
   `resolve`+classify → leaf-symlink/`is_file` guard → read whole file (byte-bounded,
   see below) → apply the [unique-match sequential](#edit_file-match-semantics)
   algorithm in memory → on total success, one `tokio::fs::write`. Outcome carries
   `{ path, edits_applied }`.

4. **`crates/runtime/src/tools/mod.rs` — error variants + re-exports.** Add
   `ToolError` variants and stable `code()`s (see [§ Error handling](#error-handling)).
   Re-export `WriteFile`, `WriteFileInput`, `WriteFileOutcome`, `EditFile`,
   `EditFileInput`, `EditFileOutcome`, `FileEdit`. Add the two modules to the `mod`
   list. Update the module doc header ("Four tools…" → the new count).

5. **`crates/runtime/src/agent.rs` — loop wiring.**
   - `offered_tool_names` baseline (`agent.rs:859-868`): add `WriteFile::NAME` and
     `EditFile::NAME` to the unconditional CORE list (next to `ApplyPatch::NAME`).
   - `tool_definitions` catalog (`agent.rs:2478-`): add two `decl(...)` entries with the
     JSON schemas in [§ Tool contracts](#tool-contracts).
   - `prepare` (`agent.rs:1491-`): two arms mirroring the `ApplyPatch::NAME` arm
     (`agent.rs:1532-1546`) — parse, spill the payload to an artifact via `self.sink`,
     emit `ProposedAction::WritePatch { patch: stored.id }`.
   - `execute_prepared` (`agent.rs:1667-`): two arms mirroring the `ApplyPatch`
     arm (`agent.rs:1781-1792`), calling `WriteFile::execute` / `EditFile::execute`
     under `write_scope` and formatting the honest observation string.
   - Parse helpers `parse_write_file` / `parse_edit_file` next to `parse_read_file`
     (`agent.rs:2255-`), rooting the raw `path` at `run.worktree`.
   - `PreparedTool` enum (`agent.rs:2070`): add `WriteFile(WriteFileInput)` and
     `EditFile(EditFileInput)` variants.
   - Test: extend the offered-set tests (`agent.rs:3215-`) to assert both names are
     offered in a solo run; add a `prepare`→`execute_prepared` round-trip like the
     `memory.remember` test (`agent.rs:3314-`).

6. **`crates/runtime/src/tools/label.rs` — tool-card label.** `write_file` is already
   recognized by the existing `"workspace.write_file" | "write_file" | …` arm
   (`label.rs:60-62`) → its `path`. Add `EditFile::NAME` (`"workspace.edit_file"` /
   `"edit_file"`) to that same path-yielding arm so the card shows
   `workspace.edit_file · src/lib.rs`. Tests: both names yield the `path`; the bulky
   `content`/`edits` args never surface (they are not among the recognized keys).

7. **(Only if [Open decision 1](#open-decisions) picks the honest-label alternative)**
   protocol `ProposedAction::WriteFile { path }` variant + golden vector + policy arm +
   TUI `render.rs`/`reduce.rs` arms. **Not required** under the recommended reuse of
   `WritePatch`.

## Tool contracts

### `workspace.write_file`

```jsonc
// args
{ "path": "string (required)", "content": "string (required)" }
```

- **Schema** (`tool_definitions`): `{"type":"object","properties":{"path":{"type":
  "string"},"content":{"type":"string"}},"required":["path","content"]}`.
- **Description** (model-facing): "Create a new file or overwrite an existing file with
  the full new contents. Use for new files or small full rewrites; for a targeted
  change to a large file use `workspace.edit_file`."
- **Semantics**: resolve `path` in the worktree; refuse if it escapes scope; refuse if
  the existing target is a symlink, directory, or other non-regular file; create parent
  directories as needed; write `content` (truncating overwrite). Empty `content` is
  valid (writes an empty file).
- **Observation** (honest): `created <path> (<n> bytes)` or
  `overwrote <path> (<n> bytes)`.

### `workspace.edit_file`

```jsonc
// args
{
  "path": "string (required)",
  "edits": [                         // required, length >= 1
    { "search": "string (required, non-empty)", "replace": "string (required)" }
  ]
}
```

- **Schema**: `{"type":"object","properties":{"path":{"type":"string"},"edits":
  {"type":"array","minItems":1,"items":{"type":"object","properties":{"search":
  {"type":"string"},"replace":{"type":"string"}},"required":["search","replace"]}}},
  "required":["path","edits"]}`.
- **Description** (model-facing): "Edit an existing file with one or more exact
  search/replace pairs. Each `search` must appear **exactly once** in the file — if a
  match is not unique the edit is rejected and you should include more surrounding
  context. All edits apply together or not at all."
- **Semantics**: see [§ `edit_file` match semantics](#edit_file-match-semantics).
- **Observation** (honest): `applied <k> edit(s) to <path>`.

## `edit_file` match semantics

**Exact, unique, sequential, atomic.**

1. **Exact substring match.** `search` is matched as a literal byte substring — no
   trimming, no whitespace normalization, no regex.
2. **Unique match required.** For each edit, count **non-overlapping** occurrences of
   `search` in the *current* buffer:
   - `0` → fail with `ToolError::SearchNotFound` →
     `edit <i>: search text not found in <path>`.
   - `>1` → fail with `ToolError::SearchAmbiguous` →
     `edit <i>: search text is ambiguous (<n> matches) — include more surrounding
     context so it is unique`.
   - exactly `1` → replace that single occurrence with `replace`.
   `<i>` is the 1-based edit index, so the model knows *which* pair failed. This strict
   contract is the deliberate, robust weak-model design: it forces the model to widen
   its snippet rather than silently edit the wrong location.
3. **Empty `search` rejected.** An empty `search` is a malformed edit
   (`ToolError::EmptySearch` → `edit <i>: search text must not be empty`), rejected
   before any matching — an empty string is trivially ambiguous.
4. **Sequential against the evolving buffer** (chosen over all-against-original).
   Edits apply in array order; edit `i+1` matches against the buffer *after* edit `i`
   was applied. **Justification**: (a) this is the standard Aider/CLI-edit contract
   models are trained against; (b) it lets a later edit target text a prior edit just
   produced; (c) uniqueness is well-defined against one concrete buffer state at each
   step, whereas all-against-original makes overlapping/adjacent edits ill-defined.
5. **Atomic — all-or-nothing.** The entire sequence is computed **in memory**; the
   filesystem is touched **only once**, after every edit has matched uniquely. If any
   edit fails, `execute` returns that edit's error and **writes nothing** — no partial
   state is possible because no write occurs before full success.
6. **Whole-file read is byte-bounded.** `edit_file` must hold the file in memory to
   search it; bound the read at `MAX_EDIT_BYTES` (reuse the read tool's ceiling,
   `read_file.rs:16` `64 * 1024 * 1024`). A file exceeding the cap is refused with a
   clear error (`ToolError::FileTooLarge` → `file exceeds the <cap>-byte edit limit;
   use git.apply_patch for very large files`) rather than being silently truncated.
7. **Missing target.** `edit_file` edits an existing file; a missing path fails via the
   underlying read (`ToolError::Io`, "No such file or directory"). New files use
   `write_file`.

## Data flow

1. Model emits a tool call. `run_tool` (`agent.rs:1304`) → `prepare`.
2. `prepare` parses args (worktree-rooted path), **spills the payload to an artifact**
   (`write_file`: the new `content`; `edit_file`: the serialized `edits`) via
   `self.sink.store(...)` exactly as the `apply_patch` arm does (`agent.rs:1534-1542`),
   and returns `Prepared { action: WritePatch { patch: id }, tool: PreparedTool::… }`.
   The spilled artifact **is the audit record** of what was written.
3. `policy.evaluate(action, eval_ctx)` → `eval_write` → `FileWrite` capability,
   `Decision::Allow` in the worktree (or `Deny` in Explore/Ask). No approval card for
   the write itself (see [§ Verified](#verified-how-apply_patch-is-actually-gated)).
4. `run_tool` emits `ToolStarted { label: tool_label(tool,args) }` — the `path`
   (`label.rs`), never `content`/`edits`.
5. `execute_prepared` runs `WriteFile::execute` / `EditFile::execute` under
   `write_scope`, producing the honest observation + `ToolOutcome`.
6. `ToolCompleted` carries the outcome; the observation is fed back to the model.
7. At run end, `review_changeset` (`agent.rs:1183`) reviews the accumulated worktree
   diff before anything is published.

## Protocol decision: reuse `WritePatch`

**Recommended: reuse `ProposedAction::WritePatch { patch: ArtifactId }` — zero
protocol/wire/golden change.** Verified feasible:

- **Policy**: `WritePatch → eval_write → FileWrite`, `Allow`-in-worktree
  (`policy/mod.rs:270`). Identical to `apply_patch`. **No policy-engine change** — the
  existing arm handles it; it does **not** hit the `_ => deny` catch-all
  (`policy/mod.rs:280-284`).
- **Protocol**: no new enum variant → **no `crates/protocol` change and no new golden
  vector** (`crates/protocol/tests/golden_vectors.rs:927` already covers `WritePatch`).
- **TUI**: `render.rs:3452/3501` and `reduce.rs:1646` already handle `WritePatch`.
- **Mechanism**: `prepare` spills the payload to an artifact and references it by id —
  the exact pattern `apply_patch` already uses (`agent.rs:1534-1546`). For `write_file`
  the artifact is the new file content; for `edit_file` it is the serialized `edits`.

**Known imperfection (the one wrinkle):** the `WritePatch` variant's human-facing
labels read "apply patch" (`render.rs`) / "FileWrite (apply patch)" (`reduce.rs`). For
a whole-file write this label is slightly inaccurate. In practice it is rarely
surfaced, because the decision is `Allow` — **no `ToolProposed`/approval card is emitted
at all** for a worktree write (`run_tool` only emits `ToolProposed` on
`RequireApproval`, `agent.rs:1401-1410`). The tool's own **observation** (the
model-facing and user-facing honest text — "created/overwrote/applied N edits") is
accurate regardless, satisfying the honesty constraint. The imperfect string is
internal audit metadata only.

**Flagged alternative (honest labeling, additive churn):** add
`ProposedAction::WriteFile { path: String }` (the `ProposedAction` enum is already
`#[non_exhaustive]`, so this is additive exactly like the recent `RecordMemory`
variant, `run.rs:168-173`). Cost: a one-line policy arm (`WriteFile { .. } =>
self.eval_write(ctx)`), **one new golden vector**, and TUI `render.rs`/`reduce.rs`
arms. Benefit: audit/label reads "write file: `<path>`" and the spilled-artifact media
type is honestly a file, not `text/x-diff`. This is the more truthful modeling but
violates the "prefer no golden change" constraint. See [Open decision 1](#open-decisions).

## Error handling

New `ToolError` variants in `crates/runtime/src/tools/mod.rs` (each with a stable dotted
`code()` mirroring the existing convention, `mod.rs:145-162`). All messages are
**actionable** — they tell a weak model precisely what to change:

| Variant | `code()` | Message (model-facing) |
|---|---|---|
| `SearchNotFound { path, index }` | `tool.search-not-found` | `edit <i>: search text not found in <path>` |
| `SearchAmbiguous { path, index, count }` | `tool.search-ambiguous` | `edit <i>: search text is ambiguous (<n> matches) — include more surrounding context so it is unique` |
| `EmptySearch { index }` | `tool.empty-search` | `edit <i>: search text must not be empty` |
| `NotRegularFile(PathBuf)` | `tool.not-regular-file` | `not a regular file (symlink or directory): <path>` |
| `FileTooLarge { path, cap }` | `tool.file-too-large` | `file exceeds the <cap>-byte edit limit; use git.apply_patch for very large files` |

Reused existing variants: `PathOutOfScope` / `PathDenied` (containment, `mod.rs:93-98`)
and `Io` (missing file, permission, etc., `mod.rs:137-139`). A failing `edit_file`
returns on the **first** failing edit (deterministic, lowest-index-first) and writes
nothing. As with `apply_patch` (`agent.rs:1784-1790`), the `ToolCompleted` failure
message carries the stable `code()`, while the model-facing observation carries the
full human text.

## Constraints

- **Worktree-confined.** Every write path is resolved and classified against the
  granted write `PathScope` before any filesystem mutation; `OutsideRoots`/`Denied`
  refuse. A new file's non-existent leaf is handled by `canonicalize_lenient` (resolves
  the existing prefix's `..`/symlinks, then root-checks). A leaf symlink is refused.
  **A write can never land outside the disposable worktree** — this is the security
  boundary and it reuses the exact scope machinery `apply_patch`/`read_file` use.
- **Approval/mode-gated as `apply_patch`.** `FileWrite` via `eval_write`: auto-`Allow`
  in the worktree, **`Deny` in read-only modes** (Explore/Ask), reviewed as an
  end-of-run change-set. No auto-write escapes the worktree; nothing is published
  without change-set review.
- **Atomic `edit_file`.** All edits computed in memory; a single filesystem write only
  after full success; any failure leaves the file byte-for-byte unchanged.
- **Actionable errors.** Not-found / ambiguous / empty-search / too-large / not-regular
  each tell the model exactly what to fix.
- **Honesty.** The observation reports the real outcome (created vs overwrote, byte
  count, edits applied); never a fabricated success.
- **Prefer no protocol/golden change.** Reuse `WritePatch` (recommended); the honest-
  label alternative is flagged as additive if chosen.
- **No new external dependency.** Only `std`/`tokio::fs` and the existing scope/policy
  seams.
- **Testable.** Path containment, unique-match, ambiguity, atomicity, and
  create-vs-overwrite are all unit-testable without a live model or daemon.

## Testing

**`PathScope::resolve` (`scope.rs`)**
- New (non-existent) leaf under a root → resolved path + `Allowed`.
- `root/../escape` → `OutsideRoots`. Denied subpath → `Denied`.
- Symlinked parent resolves before the root check (no escape).

**`WriteFile::execute` (`write_file.rs`)**
- Create a new file (parent dirs auto-created) → file exists with exact bytes; outcome
  `created`, correct `bytes_written`.
- Overwrite an existing file → truncating overwrite; outcome `overwrote`.
- Empty `content` → empty file created.
- Path escaping the scope (`../../etc/x`, absolute outside root) → `PathOutOfScope`,
  **no file written**.
- Target is an existing directory → `NotRegularFile`, no write.
- Target is a symlink → `NotRegularFile`, no write (leaf-symlink guard).

**`EditFile::execute` (`edit_file.rs`)**
- Single unique match → replaced; file content exact; `edits_applied == 1`.
- `search` absent → `SearchNotFound { index: 1 }`, **file unchanged on disk**.
- `search` appears twice → `SearchAmbiguous { index, count: 2 }`, file unchanged.
- Empty `search` → `EmptySearch`, file unchanged.
- Multiple edits, all unique → all applied; **sequential** proof: edit 2's `search`
  targets text produced by edit 1 → succeeds.
- **Atomicity**: 3 edits where edit 2 is ambiguous → call fails with edit 2's error and
  the file is byte-for-byte unchanged (edit 1 not persisted).
- Path escaping the scope → `PathOutOfScope`, no write.
- Missing file → `Io` (not-found).
- File over `MAX_EDIT_BYTES` → `FileTooLarge`, no write.

**Loop wiring (`agent.rs` tests)**
- `offered_tool_names` for a solo run includes `workspace.write_file` and
  `workspace.edit_file` (extends `agent.rs:3215-`).
- `prepare` → `execute_prepared` round-trip for each tool writes into a temp worktree
  and returns a `Succeeded` outcome with the honest observation (mirrors the
  `memory.remember` round-trip test, `agent.rs:3314-`).

**Label (`label.rs` tests)**
- `workspace.write_file` / `workspace.edit_file` (and bare `write_file`/`edit_file`)
  → the `path`.
- Bulky `content` / `edits` args never surface in the label.

## Self-review

- **Placeholders**: none — every path, line reference, arg shape, error code, and
  observation string is concrete.
- **Consistency**: tool names `workspace.write_file` / `workspace.edit_file` match the
  `workspace.*` family and `label.rs`'s existing recognized set; both use only
  `FileWrite` (no `CommandExecute`, since no subprocess); both reuse `WritePatch` and
  `eval_write` — consistent with the verified `apply_patch` behavior.
- **Scope**: additive only; `apply_patch` untouched; no new dependency; the single new
  cross-crate API is the small additive `PathScope::resolve`.
- **Ambiguity resolved**: sequential-vs-original (→ sequential, justified);
  unique-match contract (→ exactly-one, with exact error text and indices); atomicity
  (→ in-memory then one write); new-file leaf resolution (→ `canonicalize_lenient` via
  `resolve`); leaf-symlink escape (→ refuse); protocol modeling (→ reuse `WritePatch`,
  honest-label alternative flagged).

## Open decisions

1. **`ProposedAction` modeling (confirm before planning).** Recommended: **reuse
   `WritePatch`** — zero protocol/wire/golden/TUI change; the only cost is that internal
   audit labels read "apply patch" for a whole-file write (rarely surfaced, since a
   worktree write is `Allow` and emits no approval card; the model-facing observation is
   honest regardless). Alternative: add an additive `ProposedAction::WriteFile { path }`
   for truthful labels at the cost of **one new golden vector** + a policy arm + two TUI
   arms. Bundled into this decision: confirm the safety envelope is **the same as
   `apply_patch`** — writes auto-`Allow` into the *disposable worktree*, denied in
   read-only modes, reviewed as an end-of-run change-set — **not** a per-call human
   approval prompt (which `apply_patch` also does not have).
2. **`edit_file` uniqueness contract (confirm).** Recommended: **require exactly one
   match** per `search` (0 → not-found, >1 → ambiguous with the count), applied
   **sequentially** against the evolving buffer, **fully atomic**. Confirm this strict
   Aider-style contract is preferred over replace-first-occurrence or replace-all (both
   of which risk silently editing the wrong location for a weak model).
