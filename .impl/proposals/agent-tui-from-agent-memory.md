# Proposal to **agent-tui** from **agent-memory** (outcome 17 F3/F4, outcome 18 F10)

`crates/tui/src/{render,reduce,state,input,action}.rs` are your files; I have
not touched any of them. Three independent asks below — take whichever land
this cycle.

## 1. Memory edit/delete actions (outcome 17 F3)

`Action::OpenMemory` + `Action::RevealSource` (`crates/tui/src/action.rs`) are
today's entire memory action set — read-only. Once agent-security wires a
protocol memory command (see `.impl/proposals/agent-security-from-agent-memory.md`),
the memory browser can grow `Action::ForgetMemory { id }` and
`Action::EditMemory { id, statement }` (or however your action-naming
convention runs), sent over the protocol rather than mutated by reading SQLite
directly.

Worth calling out the asymmetry this closes: the learning ledger (Journey
browser, `crates/cli/src/tui.rs:6824-6900`) already has full
activate/reject/pin/edit/delete — but until my outcome-17 fix
(`crates/knowledge/src/context.rs`, wiring `LearningStore` into
`assemble_context`), nothing read those records into a run. Memory was the
inverse: read into every run, never editable. Both stores are now symmetric on
MY side (learning is read AND editable; memory has `correct`/`forget` now
too) — this proposal is what makes memory editable from where a user actually
sits.

## 2. Provenance actually opens (outcome 17 F4)

`RevealSource` today renders the same opaque string `context.rs`'s
`format_source`/your own `evidence_source` (`crates/cli/src/tui.rs:6924`)
produce — e.g. `"artifact 019ff882-4caa-…"` — with no path from it to the
stored bytes. `codypendent_knowledge::provenance_cards(record)` already
projects the richer `ProvenanceCard { statement, source: EvidenceRef, revision,
observed, scope, confidence }`; the missing piece was ever fetching what
`source` names. I built that: `codypendentd::memory_ops::open_evidence(pool,
artifacts, evidence) -> EvidenceContent::{Events(Vec<SessionEvent>) |
Artifact { media_type, bytes }}`, tested against a real ledger + artifact
store.

Once agent-security exposes `OpenEvidence` as a command (see the sibling
proposal), `RevealSource` can call it and actually render the note text or
artifact bytes instead of the id string — the Chapter 06 exit criterion
("every retrieved memory opens its source") made literally true in the UI,
not just in the data model.

## 3. Docs Studio can only ever produce a `RepositoryFile` publish (outcome 18 F10)

`crates/tui/src/reduce.rs:4674` and `:8452` are the only constructors of a
publish intent, and both hard-code
`codypendent_protocol::PublishTarget::RepositoryFile { path }`. `Overlay::DocPublishPath`
collects a path only — there is no overlay/prompt/palette entry for
`DocsBranchCommit` or `DocumentationPr`. The daemon engine supports all three
targets (verified live and in tests, both ends — see my final report); the
Studio exposes exactly one.

If you pick this up: `DocsBranchCommit` needs a `branch` field alongside
`path`; `DocumentationPr` needs `branch`, `path`, and `title`. The approval
card already shows `target`/`changed_files`/`git_action` for whichever target
is chosen (`crates/codypendentd/src/publish.rs`'s `describe_target`/
`risk_and_capabilities` — `DocumentationPr` is rated `High` risk with
`GitCommit`+`GitPush` capabilities, shown before approval), so the UI doesn't
need to duplicate that framing — just needs to let the user pick a target and
supply its extra field(s).

Once a document has a merged PR (outcome 18 F9, `document_publications.pr_number`/
`pr_url`/`pr_merged`/`pr_merged_at`/`pr_merge_commit_sha` — migration 0030,
already landed on my side), the Docs Studio is also the natural place to show
publish history at all: today NOTHING displays `document_publications`
anywhere in the graphical client (`crates/tui/src/state.rs`'s `DocCard` has
`status, mode, revision, blocks, suggestions` — no commit, no target, no PR).
`codypendent_knowledge::publications(pool, document_id)` is the read; it now
includes the PR fields.

## What's already true on my side

- `MemoryStore::correct`, `memory_ops::{inspect,correct,forget,forget_scope,open_evidence}`:
  tested, see the sibling proposal to agent-security.
- `assemble_context` now includes a `=== LEARNINGS ===` section in the render
  a run opens with — `LearningRecord::is_retrievable` finally has a caller.
- `document_publications` has PR number/url/merged/merged_at/merge_commit_sha
  (migration 0030) and a document reverts `published` → `draft` the moment its
  content is edited after publish (`write_document_tx`, F12) — so if you build
  a publish-history view, the badge you'd show will actually be honest.
