# `@codypendent/protocol`

TypeScript types for the Codypendent daemon wire protocol.

## These types are hand-maintained, not generated

There is no code generator behind this package. Every type here was written by
hand against `crates/protocol/src/*.rs`, one TypeScript module per Rust module,
same type names, same field names.

What makes them trustworthy is not their provenance — it is
`test/protocol-vectors.test.ts`, which pins every declared type to the
**committed golden vectors** in `<repo-root>/protocol-vectors/`. Those vectors
are the authoritative serialized shapes: `crates/protocol/tests/golden_vectors.rs`
both generates them and asserts in CI that a fresh regeneration matches the
committed bytes exactly, so a Rust type that changes shape without regenerating
fails on the Rust side, and a regenerated vector that this package cannot
represent fails here.

This package reads those same files directly by relative path. No copy, no
second source of truth. (The VS Code extension does the same thing with its own
hand-written types in `extensions/vscode/src/protocol/types.ts` — this package
is a second, independent consumer of the same contract, not a fork of it.)

### What the conformance suite actually checks

Each vector is `unknown` JSON at runtime, so the suite runs it through a
`reconstructX` function that copies named fields one by one into an object
literal **annotated with the exact exported type**. Two independent failures
follow from drift:

1. **`npm run typecheck` fails** when the literal names a field the type does
   not declare, or omits one it requires. (Verified: adding a stray property to
   any reconstructed literal is a `TS2353`.)
2. **The round-trip deep-equal fails** when a field present in the vector never
   made it across, because the type had nowhere to put it. (Verified: deleting
   `label` from the `ToolStarted` reconstruction fails
   `decodes and re-encodes EventBody_ToolStarted identically`.)

Coverage is **total**: every committed vector file, and every vector in it, is
exercised. The suite walks the directory listing at run time rather than naming
files, so a newly added vector file is picked up automatically instead of being
silently ignored — the failure mode that let three new vector files land in the
extension's suite with every assertion still green. The machinery to declare
something out of scope is still there (`EXCLUDED_VECTORS`, `EXCLUDED_FILES`) and
both lists are empty today; a vector with no reconstructor and no exclusion entry
fails the per-file partition check.

### The one gap, stated plainly

The vectors are the contract, and a brand-new Rust **variant** does not
automatically acquire a vector (see `protocol-vectors/README.md` — adding one is
a documented manual step). So "every vector is modeled" is not the same claim as
"every Rust variant is modeled". Where a variant exists in Rust with no vector,
it is still declared here from the Rust source, and marked as vector-less:

- `CommandBody`: the UI-plugin lifecycle family, `ResolveQuestion`,
  `RestoreCheckpoint`, `ForkSession`, the queued-prompt family, `RunUserShell`,
  `RememberMemory`.
- `Payload`: `SessionForked`, `UiPluginLifecycle`, and `RemoteUi` — the last of
  which carries `remote_ui.rs`'s `UiWireMessage`, left as `unknown` rather than
  guessed at.
- `EventBody`: `ModelRetrying`, `ContextUsage`, `QuestionAsked`,
  `QuestionResolved`, `CheckpointRecorded`, `CheckpointRestored`,
  `SessionForked`, `PendingPromptsChanged`.

## How to keep it in sync

When a wire type in `crates/protocol/src/` changes:

1. Regenerate the vectors as `protocol-vectors/README.md` describes
   (`cargo test -p codypendent-protocol --test golden_vectors regenerate_vectors -- --ignored`).
   For a brand-new **variant**, add its `vec_of(...)` entry first — otherwise
   nothing on either side will notice it.
2. Run `npm --prefix sdk/protocol run check`. A changed field makes it fail.
3. Update the affected module in `src/` and its reconstructor in
   `test/protocol-vectors.test.ts` in the same commit.

## Conventions used when mapping Rust to TypeScript

| Rust | TypeScript |
| --- | --- |
| `#[serde(tag = "type")]` enum | discriminated union on `type` |
| `#[serde(tag = "op")]` / `"kind"` / `"block"` | union on that field instead — `DocumentMutation`, `PublishTarget`, `InputBlock` |
| `#[serde(other)] Unknown` | an explicit `{ type: "Unknown" }` member, plus the folds in `src/tags.ts` |
| newtype variant, e.g. `Payload::Event(SessionEvent)` | intersection — the inner fields sit alongside the tag |
| `skip_serializing_if` (`Option::is_none`, `Vec::is_empty`, zero, false, empty string) | optional `field?:` — absent on the wire, never a placeholder value |
| `#[serde(default)]` with **no** skip | required — the daemon always serializes it (e.g. `ExecuteCommand.environment`, `ExecuteCommand.cwd`, `IdeContextUpdate.diagnostics_revision`) |
| `Option<T>` with no skip | `T \| null` — serialized as an explicit `null` |
| `#[serde(transparent)]` id newtypes | plain `string` aliases (`SessionId`, `RunId`, …), so `JSON.parse` output is directly assignable |
| `chrono::DateTime<Utc>` | `Timestamp` (an RFC 3339 string) |
| `serde_json::Value` | `JsonValue` |
| `u64` / `i64` | `number` — values above `Number.MAX_SAFE_INTEGER` are not representable, which matches what `JSON.parse` would give you anyway |

## Forward compatibility

Rust's `#[serde(other)] Unknown` degrades an unrecognized variant instead of
failing the frame. TypeScript has no deserializer to hang that off, so
`src/tags.ts` provides it explicitly:

```ts
import { foldUnknownPayload } from "@codypendent/protocol";

const payload = foldUnknownPayload(JSON.parse(frame) as { type: string });
// a tag from a newer daemon becomes { type: "Unknown" } rather than a lie
```

`PAYLOAD_TAGS`, `EVENT_BODY_TAGS` and `COMMAND_BODY_TAGS` are checked against
their unions at compile time in both directions, so a variant added to a union
without a tag entry (or vice versa) is a type error.

## Scripts

```sh
npm --prefix sdk/protocol run check   # typecheck + conformance tests + build
```
