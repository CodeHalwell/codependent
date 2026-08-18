# Protocol golden vectors

This directory is the **single source of truth** for the Rust <-> TypeScript
wire-codec drift guard (T16). The VS Code extension hand-duplicates the Rust
wire codec in `extensions/vscode/src/protocol/` (there is no generated SDK —
see `ROADMAP.md`'s cross-cutting "Generate the protocol SDK" item). That
duplication drifted once for real: the S1 bug, where the extension's approval
card omitted the `environment`/`cwd` fields the Rust
`ProposedAction::ExecuteCommand` type carries. These vectors exist so that
never happens silently again.

## What is here

One JSON file per source module in `crates/protocol/src/`
(`command.rs` -> `command.json`, `envelope.rs` -> `envelope.json`, ...). Each
file is a JSON object mapping a descriptive vector name (e.g.
`CommandBody_StartRun`, `ProposedAction_ExecuteCommand`) to one deterministic,
pretty-printed instance of that Rust type's serialized JSON. Every id is a
fixed sentinel (never `Uuid::now_v7()`) and every timestamp is fixed (never
`Utc::now()`), so the files are byte-for-byte stable across regenerations
until a wire type actually changes.

## The four wires

The top level of this directory is the **daemon** protocol
(`crates/protocol/src/`). Three subdirectories hold the wires that had a stated
compatibility guarantee and nothing behind it until v0.11:

| Directory | Wire | Rust types | Generator / guard |
| --- | --- | --- | --- |
| *(top level)* | Local daemon | `crates/protocol/src/` | `crates/protocol/tests/golden_vectors.rs` |
| `federation/` | Federation commands, payloads and the federated code graph | `crates/protocol/src/federated_graph.rs`, plus the federation arms of `command.rs`/`envelope.rs` | `crates/protocol/tests/federation_vectors.rs` |
| `control-plane/` | Hosted control plane | `crates/control-plane-protocol/src/` | `crates/control-plane-protocol/tests/golden_vectors.rs` |
| `runner/` | Remote runner | `crates/control-plane-protocol/src/runner.rs` | same file as `control-plane/` |

Each generator follows the identical pattern — fixed sentinels, one committed
JSON file per family, keys sorted, pretty-printed — and each ships the same
three guards: a regeneration-equality check, a round-trip-through-Rust check,
and a **partition guard** that reads the Rust source at test time so a newly
added type or variant fails instead of quietly widening the uncovered surface.

Regenerate them the same way, one command per generator:

```sh
cargo test -p codypendent-protocol --test golden_vectors regenerate_vectors -- --ignored
cargo test -p codypendent-protocol --test federation_vectors regenerate_vectors -- --ignored
cargo test -p codypendent-control-plane-protocol --test golden_vectors regenerate_vectors -- --ignored
```

> **Subdirectories must be walked recursively.** Both TypeScript inventory
> suites list this directory to decide what is covered. `readdirSync` without
> recursion only ever sees the top level, so vectors under `federation/`,
> `control-plane/` and `runner/` were invisible to it: the guard reported
> success while checking nothing about them. `extensions/vscode` was fixed in
> v0.10 and `sdk/protocol` in v0.11, and each suite now carries a meta-test
> that drops a probe file two levels down and requires the walk to find it —
> asserting the walk *descends*, rather than asserting it merely looks like it
> does.

## Who reads these files

- **Rust**: `crates/protocol/tests/golden_vectors.rs` is both the generator
  and the two CI checks:
  - `committed_vectors_match_current_protocol_types` — a fresh regeneration
    must equal the committed bytes exactly (catches "changed a type but forgot
    to regenerate").
  - `committed_vectors_round_trip_through_their_rust_types` — every committed
    entry, read off disk, deserialized through its own concrete Rust type, and
    re-serialized, must reproduce itself exactly (catches a hand-edited or
    otherwise-stale file even if the check above were bypassed).
  Both run in the ordinary `cargo test --workspace --all-features` CI job —
  no separate CI wiring needed.
- **TypeScript**: `extensions/vscode/test/protocol-vectors.test.ts` reads
  these SAME files directly via a relative path
  (`extensions/vscode/test/` -> `../../../protocol-vectors/`) — no copy, no
  second source of truth. It asserts the extension's hand-written
  `CommandBody`/`Payload`/`EventBody`/`ProposedAction`/... types in
  `src/protocol/types.ts` can represent every field of the vectors the
  extension actually sends/consumes, and that command vectors re-encode to
  identical JSON. This runs in the existing `extension` CI job via `npm test`
  — no separate CI wiring needed there either.

- **TypeScript (SDK)**: `sdk/protocol/test/protocol-vectors.test.ts` reads the
  same files for `@codypendent/protocol`. It covers the daemon wire and the
  `federation/` directory — the federation view types are not exported under
  their own names, so that suite derives each one from the exported `Payload`
  and `CommandBody` unions by indexed access, which pins the assertions to the
  types a consumer actually receives. `control-plane/` and `runner/` are listed
  as excluded there: they are separate wires served by `sdk/control-plane`, so
  `@codypendent/protocol` has no type for them to drift against.

Both sides read the identical files; neither copies or re-derives the other's
data. A Rust field the TypeScript type lacks makes the corresponding vector
fail on the TypeScript side — that is the drift catch.

Note the division of labour between the two languages, because neither guard
alone is sufficient: the **Rust** checks pin the vectors' exact *bytes* against
the emitter (a changed sentinel, a redaction that became `""`, an absent
measurement coerced to `0` all fail there), while the **TypeScript** checks pin
the *type shapes* (a field on the wire the TS type cannot hold fails there).

## Regenerating

Whenever a wire type in `crates/protocol/src/` changes shape (new field, new
variant, a changed field type):

> **New _variant_ (not just a new field): add its vector first.** A new field on
> an already-vectored type is self-enforcing — the Rust struct literal in
> `golden_vectors.rs` won't compile until you supply the new field, so it flows
> into the vectors automatically. A brand-new **variant** has no such forcing
> function: `regenerate_vectors` only re-serializes the instances already listed,
> so you must first add a `vec_of("TypeName_NewVariant", …)` call in the matching
> `*_vectors()` function (and, if the extension uses that type, its
> `reconstruct*`/partition entry) — *then* regenerate. Otherwise the guard stays
> silently blind to the new variant on both sides.

```sh
cargo test -p codypendent-protocol --test golden_vectors regenerate_vectors -- --ignored
```

Then:

1. Review the diff under `protocol-vectors/` — it should show exactly the
   change you made (a new key, a new field on an existing entry, ...).
2. If the change is one the VS Code extension needs to know about (it sends or
   reads the affected type), update `extensions/vscode/src/protocol/types.ts`
   and the corresponding case in
   `extensions/vscode/test/protocol-vectors.test.ts` in the same commit.
3. Run `cargo test -p codypendent-protocol --test golden_vectors` (the two
   non-ignored checks) and, from `extensions/vscode/`, `npm test` — both must
   be green.
4. Commit the regenerated `protocol-vectors/*.json` files alongside the code
   change.

## Scope

The Rust generator enumerates comprehensively: every `CommandBody` variant,
every `Payload` variant, the nested `PromotionAction` enum, and the newer
`blackboard.rs`/`workflow.rs`/`capabilities.rs`/`input.rs` modules — this
protects the Rust wire format on its own merits, independent of what the
extension uses.

The TypeScript test only checks the subset the extension actually types.
Known, intentional gaps (not drift — the extension simply does not model these
yet):

- `document.json`, `blackboard.json`, `board.json`, `workflow.json`,
  `workflow_graph.json`, `input.json` — the extension does not subscribe to
  `Document`/`Blackboard`/`Workflow` streams and has no `InputEnvelope` capture
  path, so it has no TypeScript type for these at all.
- `history.json`, `memory.json`, `promotion_evidence.json`, `voice.json` —
  daemon- or CLI-side domains the extension never issues and never receives.

  This list is no longer maintained by hand alone: the TypeScript suite's
  `protocol-vectors/ file inventory` test partitions the real directory
  listing into covered-vs-excluded and fails on any file in neither. It exists
  because the three files added for outcome 20 (`usage.json`, `memory.json`,
  `promotion_evidence.json`) landed while every assertion in that suite still
  passed — a whole new wire event went unmodeled with nothing to notice it.
  Adding a vector file now forces a decision here.
- `CommandBody`: only the 9 variants the extension actually sends
  (`AttachSession`, `SubmitUserInput`, `StartRun`, `ResolveApproval`,
  `CancelRun`, `PauseRun`, `ResumeRun`, `QueueSteering`, `UpdateIdeContext`) are
  checked. The other ~16 (workflow lifecycle, promotion, document, blackboard
  read commands) are Rust-only client-to-daemon commands the extension never
  issues.
- `Payload`: the 12 variants the extension's `Payload` union names explicitly
  are checked field-by-field; the rest fall through the union's permissive
  `{ type: string; [key: string]: unknown }` catch-all member (proving they at
  least parse and carry a `type` tag, matching the extension's actual
  forward-compatible handling — it ignores payload types it does not
  recognize).
- `ProposedAction`: `PublishDocument`, `BlackboardPost`, and `BlackboardQuery`
  are not modeled — they only ever appear on a workflow run's tool activity,
  which the extension does not subscribe to.
- `Subscription`: `Document`, `Blackboard`, and `Workflow` are not modeled, for
  the same reason.

The TypeScript test enforces this partition explicitly (a completeness check
per family) so a future Rust vector that is not accounted for on either side —
covered, or in one of the documented "not modeled" lists above — fails loudly
instead of silently falling through a gap.

A full generated TypeScript SDK / JSON-Schema pipeline remains the more
complete future direction (named in the 2026-07-21 project review and
ROADMAP.md); these vectors are the pragmatic guard in the meantime.
