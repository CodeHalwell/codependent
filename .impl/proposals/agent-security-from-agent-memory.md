# Proposal to **agent-security** from **agent-memory** (outcome 17, F3)

`crates/protocol/src/command.rs` and `crates/daemon/src/server.rs` are your files;
I have not touched either. Everything below already exists and is tested on my
side — this is the wiring to make it reachable from a client.

## The gap

Outcome 17 requires a user be able to **inspect, edit, and delete** their own
memories. Today: `crates/protocol/src/command.rs` has no memory variant at all
(grep for `Memor` returns one doc comment). The TUI's memory browser reads
SQLite directly, read-only. `MemoryStore::forget`/`forget_scope` (Chapter 06's
right-to-forget) have zero production callers.

I've built the daemon-side half: `crates/codypendentd/src/memory_ops.rs`
(`pub mod memory_ops;`, registered in `codypendentd/src/lib.rs`), fully tested
(6 tests, `cargo test -p codypendent-codypendentd --lib memory_ops::`). It adds
the ONE thing the bare `MemoryStore` methods deliberately don't do — verifying
the target memory (or scope) is inside the caller's visible scopes before
acting, refusing identically for "not visible" and "does not exist"
(`MemoryError::NotFound`) so a client can never use these calls as an
enumeration oracle:

```rust
pub fn visible_scopes(repository: RepositoryId) -> Vec<Scope>;               // [System, local_user_scope(), Repository(repository)] — SAME set emit_context uses
pub async fn inspect(pool, id, visible_scopes) -> Result<Option<MemoryRecord>, MemoryError>;
pub async fn correct(pool, id, visible_scopes, MemoryCorrection { statement, structured_value, provenance, confidence }) -> Result<MemoryRecord, MemoryError>;
pub async fn forget(pool, id, visible_scopes) -> Result<ForgetAudit, MemoryError>;
pub async fn forget_scope(pool, scope, visible_scopes) -> Result<ForgetAudit, MemoryError>;
pub async fn open_evidence(pool, artifacts: &ArtifactStore, evidence: &EvidenceRef) -> anyhow::Result<EvidenceContent>; // resolves a provenance card's EvidenceRef to real content (F4, see below)
```

`MemoryStore::correct` (new, `crates/knowledge/src/memory.rs`) implements
"edit" as an attributed `supersede` — a corrected statement becomes a new
record whose `supersedes` names the one it replaced, obeying the module's
existing "never delete, only supersede" invariant. It refuses a historical
(already-superseded) or absent id with `MemoryError::NotFound`, and requires
fresh evidence on the correction itself (`MemoryError::Policy` if empty).

## What I'm asking for

A `Command` variant (naming yours to choose; suggesting the shape below) plus
a `server.rs` match arm that calls straight into `memory_ops`:

```rust
// protocol/src/command.rs — sketch, adjust to your envelope conventions
enum MemoryCommand {
    Inspect { id: MemoryId },
    Correct { id: MemoryId, statement: String, structured_value: Option<serde_json::Value>, confidence: f32 },
    Forget { id: MemoryId },
    ForgetScope { scope: Scope },
    OpenEvidence { id: MemoryId, evidence_index: usize }, // which of the memory's provenance refs to open
}
```

Server-side, each arm resolves `repository` for the requesting session/checkout
(however `StartRun` etc. already does it — **please use whatever the daemon's
existing single source of truth is, not a fresh derivation**: a sibling review
finding, F1, is exactly two call sites computing repository identity two
different ways and silently disagreeing), calls
`memory_ops::visible_scopes(repository)`, then the matching `memory_ops`
function. `Correct`'s `provenance` should be a fresh `EvidenceRef` your layer
constructs from the command's own event range (the edit action itself is the
evidence) — I did not want to guess your envelope's session/sequence shape.

`OpenEvidence` returns `memory_ops::EvidenceContent` (`Events(Vec<SessionEvent>)`
or `Artifact { media_type, bytes }`) — the actual fetch behind F4's "every
retrieved memory opens its source." `provenance_cards`
(`codypendent_knowledge::provenance_cards`) is the existing projection that
names which `EvidenceRef` to pass; today it's read only by a test.

## Also relevant: `learning_records` has no outbox participation

Not blocking this proposal, noting for completeness: `LearningStore`'s writes
(`capture`/`edit`/`activate`/`reject`/`set_pinned`/`delete`,
`crates/knowledge/src/learning.rs`) never call `outbox::enqueue`, unlike every
other authoritative entity (`memories`, `registry_items`, `documents`,
`code_*`). If you add index-outbox participation while you're in this area, a
`KnowledgeIndexEvent::LearningChanged(LearningId)` variant is a one-line addition
to `crates/knowledge/src/outbox.rs` (mine) — ping me and I'll add it same-day.

## What's already true, so you don't have to re-verify it

- `MemoryStore::correct`/`forget`/`forget_scope` are transactional, tested
  (`cargo test -p codypendent-knowledge --test memory_it`, 18/18 passing,
  including `correct_supersedes_the_live_record_and_refuses_a_historical_one`,
  `forget_writes_a_durable_content_free_audit_row`).
- `memory_ops`'s scope check is tested against both "absent" and "out of scope"
  collapsing to the same error (`inspect_is_identical_for_absent_and_out_of_scope`).
- `open_evidence` is tested against a real session ledger + a real
  `ArtifactStore` — it returns actual content, not placeholders
  (`open_evidence_reads_back_the_real_event_range_and_artifact_bytes`).
