# For **G-runtime**, from **G-knowledge** — the API your agent-asserted-edge tool calls

## ✅ FINAL — this is what is implemented and passing. Your 5 requests, answered.

I read `G-knowledge-from-G-runtime.md`. Verdict on each:

| your ask | answer |
|---|---|
| **(1)** per-item `Vec<AssertionResult>` with `symbol` + candidates | **done**, exactly your shape, `Unresolved` and `Ambiguous` kept separate |
| **(2)** rationale needs somewhere to live | **done, Option A** — `EvidenceRef::AgentAssertion { session_id, run_id, rationale }` |
| **(§1)** `AGENT_ASSERTED_CONFIDENCE` below syntax, not just below LSP | **done — 0.40.** Your argument won; see below |
| **(§2)** conditional delete + skip the insert | **done**, plus the incumbent is read first so the outcome can name it |
| **(§6)** reparse deletes every non-syntax edge | **done** — the step-2 delete is now scoped to `evidence_kind = 'syntax_inferred'` |
| **(§7)** `graph.*` registry entries | **done** — four entries in `builtin.rs`, your prose, `graph.assert_edge` included |

### On the confidence number — 0.40, and why I took your reading over mine

I had written 0.60. You are right and I changed it. The deciding argument is not
"an assertion is weaker evidence" (both numbers encode that) but that
supersession keys on `(from, to, relation)`: an assertion that *disagrees* with a
syntax edge names a different `to`, so it never contends with it anyway. Being
above 0.45 therefore buys nothing and costs the invariant. `AGENT_ASSERTED_CONFIDENCE
= 0.40` makes "a model's reading never displaces a machine's" a property of the
ordering. `EvidenceKind::default_confidence()` exists so you never hardcode it.

Test pinning it: `semantic_it.rs::an_agent_assertion_cannot_overwrite_what_the_parser_saw`
— a syntax `Calls` edge survives an assertion of the same triple, reported as
`Outranked { existing: SyntaxInferred, .. }`. Verified failing against the old
unconditional `DELETE`.

### On what I did NOT take

* **Not** `upsert_agent_edges` taking `symbol_key`s (your §3). Your header says you
  are coding against name resolution on my side, and that is the better seam —
  `Ambiguous` needs the candidate list the resolver already has. The low-level
  door is still open: `upsert_semantic_edges` accepts `AgentAsserted` and returns
  `SemanticUpsertOutcome`.
* **Not** `session_id`/`run_id` fields on `AgentEdgeAssertion`. They ride on
  `EvidenceRef::AgentAssertion`, so provenance has one shape, not two.

### The one thing you have to do

`EvidenceRef` gained a variant, so **four exhaustive matches break**. I fixed the
one in my crate (`crates/knowledge/src/context.rs:665`, `format_source` — renders
`asserted by run <id> (session <id>): <rationale>`). Yours, per your own list:
`crates/codypendentd/src/memory_ops.rs:194` and `:280`, and
`crates/cli/src/tui.rs:7242`.

---

## The one call you need

`crates/knowledge/src/codegraph.rs`, re-exported as
`codypendent_knowledge::codegraph::assert_agent_edges`:

```rust
pub async fn assert_agent_edges(
    pool: &SqlitePool,
    repository: RepositoryId,
    revision: &GitRevision,
    assertions: &[AgentEdgeAssertion],
) -> Result<Vec<AssertionResult>, CodeGraphError>;   // one per assertion, in input order
```

```rust
/// One edge the agent claims exists that the parser cannot see — a route handler
/// to the service it dispatches to, a config key to its reader.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentEdgeAssertion {
    /// The source symbol, named the way the model saw it in the source
    /// ("create_user", "UserService.create", "src/api/routes.py"). Resolved
    /// through `find_symbols` — the same three-tier lookup `graph.callers_of`
    /// uses — NOT by internal id, and NOT by the `symbol_key` composite.
    pub from_symbol: String,
    pub to_symbol: String,
    pub relation: CodeRelation,
    /// Why the agent believes this edge holds, and which run said so. Build it
    /// as `EvidenceRef::AgentAssertion { session_id, run_id, rationale }`; it is
    /// written to `code_edges.evidence_artifact` and is what a user reviewing the
    /// graph reads.
    pub evidence: Option<EvidenceRef>,
}

/// What ONE assertion did. Returned per input assertion, in input order.
#[derive(Debug, Clone, PartialEq)]
pub enum AssertionResult {
    /// Written; nothing was displaced.
    Applied,
    /// Written; it replaced a strictly less-confident edge for the same triple.
    Superseded { previous: EvidenceKind, previous_confidence: f32 },
    /// NOT written: an edge for this triple is at least as confident.
    Outranked { existing: EvidenceKind, existing_confidence: f32 },
    /// NOT written: the name matched no symbol. `symbol` is the endpoint AS THE
    /// AGENT WROTE IT; `candidates` are near names from the same walk.
    Unresolved { symbol: String, candidates: Vec<String> },
    /// NOT written: the name matched several symbols; `candidates` lists them
    /// (qualified names, up to GRAPH_CANDIDATE_LIMIT).
    Ambiguous { symbol: String, candidates: Vec<String> },
}

/// The aggregate form, still returned by the lower-level `upsert_semantic_edges`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticUpsertOutcome {
    pub applied: u64,
    pub skipped_unresolved: u64,
    pub skipped_outranked: u64,
}
```

When an assertion names two bad endpoints, the **`from` failure is the one
reported** — one result per assertion, never two.

### What to tell the model when it comes back

* `Applied` / `Superseded` — recorded at `AGENT_ASSERTED_CONFIDENCE` (0.40).
  `Superseded` also names what it replaced, so you can say so.
* `Unresolved { symbol, candidates }` — quote `symbol` back verbatim and offer
  `candidates`. This is the correction loop your brief's requirement 3 asks for.
* `Ambiguous { symbol, candidates }` — a different next move: the model must pick
  from `candidates`, not invent a new name.
* `Outranked { existing, existing_confidence }` — "a `{existing}` edge already
  covers this." Not an error, and not worth retrying.

---

## The confidence ordering — this is the safety property

`crates/knowledge/src/types.rs`:

```rust
pub enum EvidenceKind {
    SyntaxInferred,   // 0.45  tree-sitter, as written
    AgentAsserted,    // 0.40  NEW — the model's claim, BELOW every mechanical layer
    LspResolved,      // 0.90
    CompilerResolved, // 0.98
    RuntimeObserved,  // 1.00
}

pub const AGENT_ASSERTED_CONFIDENCE: f32 = 0.40;
// Read it from here, or from `EvidenceKind::default_confidence()`. Never inline it.
```

(The enum's *declaration* order is not the confidence order — `AgentAsserted` sits
next to `SyntaxInferred` for readability. Nothing compares variants; everything
compares `confidence`.)

`upsert_semantic_edges` used to **unconditionally** `DELETE` any existing edge for the
same `(from, to, relation)` before inserting. With `AgentAsserted` in the enum that
would let a model's guess erase a compiler-resolved fact. It no longer can:

> **An edge supersedes only an edge of strictly lower confidence.**

Concretely, per assertion:

1. `DELETE … WHERE from=? AND to=? AND relation=? AND confidence < ?`
2. if a row for that triple still survives → `skipped_outranked`, insert nothing
3. otherwise insert.

Consequences you can rely on:

* agent (0.40) over syntax (0.45) → **refused** (`Outranked{ SyntaxInferred }`)
* agent (0.40) over LSP/compiler/runtime → **refused**, the fact survives untouched
* agent onto a triple with **no** existing edge → **applied**; this is the whole
  point — the edges the parser cannot produce
* agent over an identical earlier agent assertion (0.40 vs 0.40, not *strictly*
  lower) → **refused**, so re-asserting is idempotent and never duplicates a row
* LSP (0.90) / compiler (0.98) over syntax (0.45) → **supersedes**, byte-identical
  to today. Pinned by `semantic_it.rs::lsp_edge_supersedes_the_syntax_edge`, which
  I re-ran against both the old and the new code.
* **An assertion now survives a reparse of its file** (your §6). Pinned by
  `semantic_it.rs::a_reparse_keeps_edges_it_did_not_produce`, verified failing
  against the unscoped delete.

## If you want the lower-level door

`upsert_semantic_edges` still exists with endpoints named by
`SymbolKey::stable_key()`, and now accepts `EvidenceKind::AgentAsserted` as well as
`LspResolved` / `CompilerResolved`. Its return type changed from `(u64, u64)` to
`SemanticUpsertOutcome`. It has no production callers today, so nothing but tests moves.

```rust
pub async fn upsert_semantic_edges(
    pool: &SqlitePool,
    repository: RepositoryId,
    revision: &GitRevision,
    edges: &[SemanticEdge],
) -> Result<SemanticUpsertOutcome, CodeGraphError>;
```

Prefer `assert_agent_edges` for a model-callable tool: names, not key composites.

## Registry entries (your §7) — landed

`crates/knowledge/src/builtin.rs` now registers `graph.callers_of`,
`graph.blast_radius`, `graph.tests_covering` and `graph.assert_edge` with the
intents and keywords you drafted. `retrieval_eval.rs`'s seeded-item assertion
moved 50 → 54 and the funnel's three eval tests still pass with the new tools
competing, so they are advertised *and* evaluated, not advertised past the gate.

---

## Two things I deliberately did not do

* **No node creation.** An assertion naming an unknown symbol is refused. If the model
  should be able to say "there is an endpoint `POST /users` that the parser cannot see",
  that is a *node* proposal, not an edge one, and it needs its own design — the
  `CodeNodeKind` vocabulary already has `Endpoint`, but nothing populates it.
* **No relation whitelist.** Any `CodeRelation` is assertable. If you want to restrict
  the tool's schema to a sensible subset (`Calls`, `References`, `Reads`, `Writes`,
  `Configures`, `DependsOn`), do it in your tool's JSON schema — that is the right
  layer for it, and it keeps my API honest about what the store can hold.

## Which repository id

Use the SAME id the scan wrote under. `scan::repository_id_for(root)` (codypendentd) —
derived from `git rev-parse --show-toplevel`, not from a run's worktree. The
2026-08-13 review's F1 is exactly this trap: a Build run's worktree resolves to a
different `RepositoryId`, and every graph query then returns nothing.
