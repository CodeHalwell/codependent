# Agent report: Docs Studio

## Docs studio reality check (what a user/agent can actually do today, traced file:line)

**Engine (real and good).** A document is typed blocks + links + citations + an authorship log (`crates/knowledge/src/docs/model.rs:299-312`), stored as an authoritative Loro CRDT snapshot BLOB with a per-mutation attribution log and outbox row per write (`store.rs:118-150`, `migrations/0008_phase4_documents.sql:10-40`). The CRDT maps each block to a Loro map `{id, type, text-container, meta}` (`crdt.rs:1-16`), so blocks are typed and round-trip losslessly (`docs_it.rs:107,118`) — "AST-backed" is honest at block granularity; text inside a block is a flat Loro text container (no inline rich-text spans).

**User path (traced).** TUI: `D` opens the Docs Studio overlay — tree / editor rail / review rail (`crates/tui/src/input.rs:341`, `render.rs:3731-3768`). `e` opens a block-edit prompt; submit acquires a block lease, then fires `MutateDocument::EditText{position:0, delete_len:0, insert}` — i.e. **prepend-only** (`reduce.rs:2573-2589`, `2048-2085`). `a`/`r` accept/reject the focused suggestion (`reduce.rs:2110-2133`). `P` prompts for a path and sends `PublishDocument` (`reduce.rs:2014-2041`); approval flows through the normal approvals rail (`reduce.rs:1469`). The server role-gates (Observer can't mutate; only Approver/Controller resolve suggestions, `crates/daemon/src/server.rs:1006-1045`), applies via the `KnowledgeDocumentMutator` seam (`crates/codypendentd/src/documents.rs:62-120`), and fans a full-snapshot `DocumentSync` to `Subscription::Document` subscribers (`server.rs:1069-1075`, `3374`), which the CLI harness merges into a `DocumentReplica` and re-projects (`crates/cli/src/tui.rs:2996-3033`). CLI: `codypendent docs publish <id> --target repo-file|docs-branch|doc-pr` previews the deterministic plan, parks the approval, self-resolves it, and polls for the commit (`crates/cli/src/main.rs:456-486`, `commands.rs:642-733`).

**Agent path (traced): does not exist.** The runtime tool registry has no document tool — `crates/runtime/src/tools/mod.rs:35-79` lists blackboard/edit_file/git/github/label/memory/read_file/repository/search/shell/web_search/write_file only; grep for "document" in `crates/runtime/src` hits nothing but an unrelated bench. The mutator comment says "an agent authors through the runtime, not this path" (`codypendentd/src/documents.rs:74-76`), but that runtime path was never built. Every socket mutation is attributed `DocumentAuthor::Human{client_id}` (`documents.rs:189-193`); `DocumentAuthor::Agent` is never constructed in production.

## Verified working

- **CRDT convergence & safety**: concurrent text and block edits converge (`docs_it.rs:137,194`); out-of-bounds text ops return `OutOfBounds` instead of panicking the daemon (`crdt.rs:169-217`, `docs_it.rs:399`); replica seed/merge is idempotent and shares one Loro history to avoid block duplication (`replica.rs:9-16`, tests `replica.rs:144-256`).
- **Optimistic concurrency**: saves and link writes are revision-guarded UPDATEs distinguishing stale vs missing (`store.rs:238-252`, `434-489`); `set_links` deliberately excluded from content saves to avoid clobbering (`store.rs:429-433`).
- **Suggestions**: propose/accept/reject with revision anchoring, original-text drift check, atomic pending-claim + document write in one transaction (`collab.rs:159-347`); double-accept refused (`docs_collab_it.rs:248`); drifted range/insertion refused (`383,447`).
- **Leases**: real single-writer enforcement — `BEGIN IMMEDIATE` serializes contenders (`leases.rs:91`), whole-doc vs block overlap rules (`leases.rs:219-253`), TTL lazy expiry (`docs_leases_it.rs:120`), renew-in-place, and accept-suggestion requires the target block's lease (`codypendentd/src/documents.rs:91-100`, test `439-497`). Enforced end-to-end over the socket (`codypendentd/tests/docs_sync_it.rs:313,451`).
- **Publication**: deterministic render (`render.rs:23-143`), plan → durable `document_publish_jobs` row → parked approval → background execution only on approve (`publish.rs:205-298`, `0016_document_publish_jobs.sql`); worktree-safe docs-branch commits, byte-identical republish is a no-op, unrelated staged changes never swept (tests `publish.rs:1044-1573`); path/branch injection validated fail-closed (`publish.rs:437-477`); PR target idempotent via hidden marker (`publish.rs:657-673`, test `1810`); restart recovery re-arms pending jobs and **is** called at startup (`publish.rs:125-201`, `codypendentd/src/lib.rs:210`).
- **Staleness library**: symbol extraction (blocks + inline `{{ symbol:… }}`), resolution against codegraph nodes recording `symbol_key/source_path/signature_hash/revision`, signature-change and disappearance findings keyed by (source_path, name), Maintain-mode suggestion drafting — all correct and well-tested (`staleness.rs:36-247`, `staleness_it.rs:57-527`). It is genuinely codegraph-wired, not heuristic — but only in tests (see below).
- **Bench**: `benches/crdt-bench/src/main.rs` really compares Loro vs Automerge vs Yrs per the intent doc's required matrix (`main.rs:53-96`).

## Bugs & broken wiring (severity)

1. **CRITICAL — no way to create a document.** `DocumentStore::create`/`NewDocument` have zero production callers (grep: only tests, `codypendentd/src/{publish,documents}.rs` test modules). No `CreateDocument` in `protocol/src/command.rs:199-247`, no `docs new` CLI (`main.rs:456-486` has only `Publish`), no TUI action. The Docs Studio ships as a three-rail browser over a permanently empty set; every end-to-end test must seed documents by writing to the store directly.
2. **CRITICAL — agent authoring is vapor.** No runtime tool (`runtime/src/tools/mod.rs`), no `DocumentAuthor::Agent` construction anywhere in production. Rubric #4's "doc writer" agent cannot draft, suggest, or maintain a document.
3. **HIGH — staleness engine unwired.** `resolve_links`/`detect_staleness`/`as_suggestion`/`set_links` have no production callers (only `lib.rs:96` re-export). `/update-docs` is only a registry card for retrieval (`knowledge/src/builtin.rs:229-246`); the only embedded workflow is `repair-github-check` (`workflow/src/source.rs:47`). Since `set_links` never runs, `links_json` is always `[]`, so even a wired detector would find nothing. `staleness.rs:12`'s claim that the flow "is registered as the `/update-docs` command" is aspirational.
4. **HIGH — suggestion review rail self-defeats at N>1.** Propose refuses unless the proposer saw the current revision (`collab.rs:184-189`); accept refuses unless `source_revision == doc.revision` (`collab.rs:284-287`). Accepting one suggestion bumps the revision, so **every other pending suggestion becomes permanently un-acceptable** (`SuggestionRangeDrifted`), and nothing re-proposes. Safe, but a 3-suggestion review session can accept exactly one.
5. **MEDIUM — `index_outbox` is write-only.** Every doc write enqueues `DocumentChanged` (`store.rs:150,300,482`), but `outbox::unprocessed`/`mark_processed` have no production callers — rows accumulate forever and documents never reach retrieval/embedding indexes (also hurts rubric #9's top-k selection).
6. **MEDIUM — TUI editing is prepend-only.** `EditText{position:0, delete_len:0}` (`reduce.rs:2582-2587`): no replace/delete, no block insert/delete/reorder surface (protocol supports them, `document.rs:24-31`), no title/metadata editing, no authorship/history view (`DocCard` omits authorship, `cli/src/tui.rs:4022-4033`).
7. **LOW — status lifecycle is decorative.** Nothing ever transitions Draft→InReview/Published/Archived (grep: `InReview` appears only in model + a TUI doc comment `tui/src/state.rs:616`); publishing records a publication row but the document stays `draft`.
8. **LOW — Markdown renderer edge cases.** Multi-line callout text only quotes the first line (`render.rs:73-80`); table cells don't escape `|` (`render.rs:120-143`); a heading containing `\n` breaks the heading line. The staleness warning inserted at block start (`staleness.rs:236-240`) inherits the callout issue.
9. **LOW — lease TTL uncapped.** `ttl_seconds` passes through unclamped (`server.rs:1118`, `documents.rs:157`); a client can lease a block for years (crashed holder blocks it until expiry). Overflowing TTLs silently become zero-length leases (`leases.rs:112-113`).
10. **LOW — `mode` on the wire is trusted-derived only server-side but displayed client-side by re-deriving** (`cli/src/tui.rs:4028`); fine today, will skew the moment per-document modes become settable.

## Gaps vs rubric #4 (doc writer)

Today an agent **cannot** draft a doc; a user **can** see, minimally edit, accept/reject suggestions on, and publish docs that cannot be created. To call this a "doc writer":

1. **Creation** — protocol `CreateDocument` + CLI `docs new` + TUI action (engine ready: `store.rs:101`).
2. **Agent tools** — runtime tools `docs.create` / `docs.edit` / `docs.suggest` / `docs.read` calling `apply_mutation` with `DocumentAuthor::Agent{run_id, model, policy_version}` (`apply.rs:100` accepts any author; the attribution schema is already built for this, `model.rs:58-71`).
3. **Mode selection** — Ask/CoAuthor/Review/Maintain are unreachable; only `default_for_scope` runs (`codypendentd/src/documents.rs:86`). Needs a per-document (or per-run) mode column + command.
4. **Maintain loop** — a background/`/update-docs` workflow: on codegraph revision change, `resolve_links` → `set_links` → `detect_staleness` → `as_suggestion` → propose. All pieces exist; zero glue.
5. **Review UX** — re-anchoring or auto-re-propose of surviving suggestions after an accept (finding 4); comments (intent doc `08-docs-studio.md:17` promises them; only suggestion `rationale` exists); authorship/history rail.
6. **Real text editing** — cursor-level block editing in the TUI, block insert/delete, document titles.

## Prioritized opportunities (S/M/L, impact)

1. **S, very high**: `CreateDocument` command + `docs new`/TUI create + seed-from-file import (`render` already gives you export; import = markdown→blocks parser). Unblocks everything else.
2. **M, very high**: agent `docs.*` runtime tools (mirror `memory.rs`'s pattern, `runtime/src/tools/memory.rs`) with Agent attribution; org-scope Suggest default already makes this safe-by-default (`collab.rs:54-59`).
3. **S, high**: wire `/update-docs`: a daemon job that runs `resolve_links`+`set_links` after each codegraph index, `detect_staleness` on demand, and proposes Maintain suggestions. ~1 file of glue over tested code.
4. **S, high**: fix suggestion revision-pinning — on accept, re-anchor other pending suggestions whose block text still matches `original` (bump their `source_revision` in the same transaction) instead of stranding them (`collab.rs:284`).
5. **M, medium**: outbox consumer indexing documents into BM25/vector retrieval so docs are findable and feed rubric #9.
6. **M, medium**: real block editor in TUI (edit buffer pre-filled with block text, replace via `delete_len=len`), block insert/delete bound to keys; status transitions on publish (`Draft→Published`).
7. **L, medium**: comments + presence (the lease indicator `tui/src/render.rs:5387` is a good seed), authorship timeline rail from `document_authorship`.

## Extra ideas

- **Doc-from-run**: a "write this up" action on a finished run that creates a doc from the run's evidence with citations pre-filled (`Citation.evidence` reuses `EvidenceRef`, `model.rs:288-295`) — the fastest credible "doc writer" demo.
- **Publish → status + staleness banner**: after `record_publication`, resolve links at that commit and render a staleness badge count in the Docs tree.
- **Delta syncs later**: every sync is a full snapshot by design (`protocol/src/document.rs:104-111`); add a size guard before large docs hit the frame bound.
- **Embed evaluation**: `Query` and `EmbeddedFile/Workflow/Skill` blocks render as inert markers (`render.rs:88-115`) — evaluating them at render time would make runbooks live.
