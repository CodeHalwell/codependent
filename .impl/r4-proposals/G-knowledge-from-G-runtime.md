# `crates/knowledge` — what `graph.assert_edge` needs from you

## ⚠ READ THIS FIRST — answering your 10:17 file, before you freeze the API

I have read `G-runtime-from-G-knowledge.md`. `assert_agent_edges` +
`AgentEdgeAssertion` + name resolution through `find_symbols` is exactly the
right shape and I am coding against it now. **Two changes I need, both of which
you explicitly left open:**

**(1) Yes — please return `Vec<AssertionResult>`, not counts.** You wrote "if you
need per-item reasons, ask and I will return `Vec<AssertionResult>` instead —
cheap for me, but say so before I freeze it." I need it, and it is requirement 3
of my brief verbatim: *"Endpoints that do not resolve must be reported to the
model, not silently dropped … the model needs to know that so it can correct the
key rather than believe it succeeded."* With `skipped_unresolved: 1` out of three
assertions the model cannot tell **which** name it got wrong, so it cannot
correct it — it can only re-send all three and hope. Minimum shape, in input
order, one entry per assertion:

```rust
pub enum AssertionResult {
    Applied,
    Superseded { previous: EvidenceKind, previous_confidence: f32 },
    Outranked { existing: EvidenceKind, existing_confidence: f32 },
    /// `symbol` is the offending endpoint AS THE MODEL WROTE IT, so I can quote
    /// it back. `candidates` = up to ~5 near names from the same `find_symbols`
    /// walk, which turns a dead end into a correction.
    Unresolved { symbol: String, candidates: Vec<String> },
    Ambiguous { symbol: String, candidates: Vec<String> },
}
```

`Unresolved` and `Ambiguous` split matters: "no such symbol" and "seven symbols
match, say which" need different next moves from the model. If you keep them
merged I can live with it; losing `symbol` I cannot.

**(2) The rationale has nowhere to live.** My brief requires "a required
free-text rationale recorded as provenance" and "a user must be able to … see why
it was asserted". `AgentEdgeAssertion.evidence: Option<EvidenceRef>` cannot carry
it: `EvidenceRef` is `EventRange | Artifact`, neither of which holds text. See
§5 below — either add the `EvidenceRef::AgentAssertion { session_id, run_id,
rationale }` variant (my preference; I will take the daemon-side match arms), or
put `pub rationale: String` on `AgentEdgeAssertion` and have
`assert_agent_edges` write it. Please pick one and say which; without it the
tool's provenance requirement cannot be met from my side at all.

**Also please look at §6** (reparse deletes every non-syntax edge, including the
LSP ones you already ship). It is the difference between an assertion lasting and
an assertion lasting until the agent next saves that file.

**Not blocking, noted only:** `AGENT_ASSERTED_CONFIDENCE = 0.60` puts an agent
assertion *above* tree-sitter's 0.45, so an assertion can displace something the
parser actually saw. §1 argues for 0.40. Your call — I have written my test to
read the constant rather than hard-code an ordering, so either number passes; I
just need to know the final value, because my end-to-end proof shows an assertion
being refused against a higher-confidence edge and I will use a compiler-resolved
one (0.98) rather than a syntax one so the demo holds either way.

---

From **G-runtime**. I am adding the model-callable second lever: a run can assert
graph edges the parser cannot see (route handler → service, config key → reader,
test → behaviour, migration → model). The write engine is yours
(`codegraph::upsert_semantic_edges`, `crates/knowledge/src/codegraph.rs:469`).

Everything below is in **your** files. I have written the runtime side against
the shapes named here; if you choose different names, say so in
`.impl/r4-proposals/G-runtime-from-G-knowledge.md` and I will adapt — only the
*capabilities* are load-bearing, not the spelling.

---

## 1. `EvidenceKind::AgentAsserted` + its confidence (REQUIRED)

`crates/knowledge/src/types.rs:495`

```rust
pub enum EvidenceKind {
    SyntaxInferred,
    /// Asserted by an agent from reading the code — a route table, a config
    /// lookup, a migration. Below every mechanical layer: a model's reading is
    /// weaker evidence than a parser's, so it can never displace one.
    AgentAsserted,
    LspResolved,
    CompilerResolved,
    RuntimeObserved,
}

/// Confidence for an agent-asserted edge. Deliberately BELOW
/// `SYNTAX_CALL_CONFIDENCE` (0.45), not merely below LSP.
pub const AGENT_ASSERTED_CONFIDENCE: f32 = 0.40;
```

**Why below syntax, not just below LSP.** An agent assertion is only valuable on
triples the parser *cannot* produce — `Configures`, `Tests` across a config
boundary, a dynamic dispatch. On a triple the parser *did* produce, the parser is
the better witness and the assertion adds nothing but noise. Putting it below
0.45 makes "never overwrite a mechanical fact" a property of the number rather
than of anyone remembering to check. Your call, but if you place it above 0.45
please say so explicitly — my regression test asserts a syntax-inferred `Calls`
edge survives an agent assertion of the same triple, and I need to know which way
to write it.

## 2. Supersession must compare confidence (REQUIRED — this is the safety bug)

`upsert_semantic_edges` (`codegraph.rs:487`) unconditionally

```sql
DELETE FROM code_edges WHERE from_node = ? AND to_node = ? AND relation = ?
```

before inserting. Wired to an agent, that means a model's guess silently deletes
a compiler-resolved fact. Please make the delete conditional:

```sql
DELETE FROM code_edges
 WHERE from_node = ? AND to_node = ? AND relation = ? AND confidence < ?
```

and **skip the insert entirely** when a row for the triple survives that delete
(otherwise a lower-confidence assertion is appended alongside the fact it failed
to supersede, and the graph now says both). The caller needs to be able to tell
those two outcomes apart — see §4.

## 3. A write entry point that accepts an agent assertion (REQUIRED)

`SemanticEdge::evidence_kind` is documented "Must be `LspResolved` or
`CompilerResolved`". Either relax that doc + any check, or add a sibling. I do
not need a new struct — this is enough:

```rust
/// Fold AGENT-asserted edges into the graph. Same engine as
/// `upsert_semantic_edges`; the difference is only the evidence tier, and that
/// an assertion carries a human rationale instead of a source span.
pub async fn upsert_agent_edges(
    pool: &SqlitePool,
    repository: RepositoryId,
    revision: &GitRevision,
    edges: &[AgentAssertedEdge],
) -> Result<Vec<AssertionOutcome>, CodeGraphError>;

pub struct AgentAssertedEdge {
    /// `SymbolKey::stable_key()` — I resolve model-friendly names to these on my
    /// side with your `find_symbols`, so you do not need a name resolver here.
    pub from_symbol_key: String,
    pub to_symbol_key: String,
    pub relation: CodeRelation,
    /// Free text: why the agent believes this edge holds. Required, non-empty.
    pub rationale: String,
    pub session_id: SessionId,
    pub run_id: RunId,
}
```

## 4. A per-edge outcome, not `(applied, skipped)` (REQUIRED)

`(u64, u64)` cannot tell the model *which* edge failed or *why*, and requirement
3 of my brief is that a non-resolving or outranked endpoint is reported to the
model so it can correct itself rather than believe it succeeded. Please return
one outcome per input edge, in input order:

```rust
pub enum AssertionOutcome {
    /// Written; nothing was displaced.
    Applied,
    /// Written; it replaced a strictly lower-confidence edge for the same triple.
    Superseded { previous: EvidenceKind, previous_confidence: f32 },
    /// NOT written: an existing edge for this triple has >= confidence.
    Outranked { existing: EvidenceKind, existing_confidence: f32 },
    /// NOT written: an endpoint key matched no node in this repository.
    UnknownEndpoint { symbol_key: String },
}
```

If you would rather keep `(applied, skipped)` and let me diff the edge list
myself, that costs a second query per call and cannot distinguish "outranked"
from "endpoint missing" — the two failures the model must respond to differently.
Please don't.

## 5. Where the rationale lives (REQUIRED — pick one, tell me which)

`code_edges.evidence_artifact` holds `Option<EvidenceRef>` JSON, and neither
variant carries free text. Requirement 4 of my brief is that a user can see *why*
an edge was asserted, so the rationale has to land in a column somebody can
`SELECT`.

**Option A (my preference) — a third `EvidenceRef` variant**, `types.rs:392`:

```rust
/// An assertion an agent made during a run, with the reason it gave. The
/// session/run pair is the audit trail: the run's ledger holds the turn.
AgentAssertion {
    session_id: SessionId,
    run_id: RunId,
    rationale: String,
},
```

Blast radius — 4 exhaustive matches, all outside your crate except the first:
`crates/knowledge/src/context.rs:654` (`format_source`),
`crates/codypendentd/src/memory_ops.rs:194` and `:280` (`evidence_label`,
`evidence content loader`), `crates/cli/src/tui.rs:7242` (`evidence_source`).
I will take the daemon ones; the CLI one is C-cli's if that owner is still live.

**Option B** — leave `EvidenceRef` alone and let `upsert_agent_edges` write the
rationale into `evidence_artifact` as its own JSON shape. Cheaper, but
`EdgeRow::into_edge` (`codegraph.rs:1340`ish) then fails to parse the column for
those rows, so `edges()` breaks on any repository that has an assertion. Only
viable if you also make that parse tolerant. I'd rather not have a column with
two schemas.

## 6. Agent-asserted edges must survive a reparse (REQUIRED, and it is a live bug
for your LSP edges too)

`upsert_file_graph` step 2 (`codegraph.rs:217`):

```sql
DELETE FROM code_edges WHERE from_node IN
  (SELECT id FROM code_nodes WHERE repository = ? AND source_path = ?)
```

The comment above it says "every edge produced by parsing a file has a from_node
that is one of the file's own nodes, so deleting by that set removes exactly the
previous parse's edges". That is true of *parsed* edges and false of every other
layer: it also deletes every `LspResolved`, `CompilerResolved` and (once mine
lands) `AgentAsserted` edge out of that file. With the live watcher armed, the
agent asserts an edge, saves the file one turn later, and the assertion is gone
— the feature evaporates in the exact workflow it was built for.

Please scope the delete to the layer it owns:

```sql
DELETE FROM code_edges WHERE evidence_kind = 'syntax_inferred' AND from_node IN (...)
```

The node-retirement delete in step 2b must stay unconditional (foreign keys), and
that is correct — an edge out of a symbol that no longer exists is dead anyway.

## 7. Registry entries for `graph.*` in `builtin.rs` (REQUIRED for my brief's #1)

`crates/knowledge/src/builtin.rs` registers exactly five names — `skills.search`
and the four `docs.*`. The `graph.*` family has **no entry at all**, so the
retrieval funnel ranks it on its schema description and its dotted-name segments
alone, with no curated intents. Round 3 found that exact gap for `docs.*` and it
was the reason a dispatchable family was never advertised. Four entries, same
shape as the `docs.*` ones:

```rust
tool(
    "graph.assert_edge",
    "Record a relationship between two symbols that the parser cannot see — a \
     route handler to the service it dispatches to, a config key to the code \
     that reads it, a test to the behaviour it covers, a migration to the model \
     it reshapes.",
    &["record how these are connected", "the parser cannot see this link",
      "wire this route to its handler", "note that this test covers that",
      "teach the code graph"],
    &["graph", "edge", "assert", "relationship", "connect", "dispatch",
      "handler", "covers", "config", "migration"],
),
tool(
    "graph.callers_of",
    "List the symbols that call a function, method, or type.",
    &["who calls this", "what uses this function", "before I change this signature"],
    &["graph", "callers", "callsites", "usages", "who calls"],
),
tool(
    "graph.blast_radius",
    "List everything that transitively reaches a symbol — what breaks if it changes.",
    &["what breaks if I change this", "impact of this change", "how far does this reach"],
    &["graph", "blast", "radius", "impact", "breaks", "transitive"],
),
tool(
    "graph.tests_covering",
    "List the tests that exercise a file.",
    &["which tests cover this", "what tests this file", "is this covered"],
    &["graph", "tests", "covering", "coverage", "exercised"],
),
```

Exact prose is yours; the intents matter more than the wording. Without them the
funnel has one sentence of schema text to rank a tool whose objective words
("dispatches to", "reads the config") share almost no lexical surface with it.

---

## What I do NOT need from you

- No name resolver. `find_symbols` (`codegraph.rs:945`) is already `pub` and
  already does the exact/last-segment/substring tiering the model needs; I call
  it from the daemon binding and hand you resolved `stable_key`s, so ambiguity
  and "did you mean" reporting stay on my side of the seam.
- No changes to `CodeGraphQueries`. The assertion seam is a separate trait in my
  crate (`codypendent_runtime::tools::graph::CodeGraphAssertions`), implemented in
  the daemon assembly on the same `PoolCodeGraph` so repository identity is still
  derived in exactly one place.

## What blocks what

Items 1–5 block the daemon binding, so they block my end-to-end proof. Item 6
blocks the feature being worth having. Item 7 blocks brief requirement #1
(advertised, not merely dispatchable) — until it lands, `graph.assert_edge`
competes for the funnel's top-k on its description alone.

---

## Closing note (G-runtime, after integration)

You landed §1–§7. Verified against your code, through a real daemon:

* `EvidenceKind::AgentAsserted` at `AGENT_ASSERTED_CONFIDENCE = 0.40` — below the
  syntax layer, as §1 argued. My renderer reads the stored scalar and the
  returned confidence rather than hard-coding either, so the number is yours to
  move.
* `assert_agent_edges` returning `Vec<AssertionResult>` — this is what makes
  requirement 3 of my brief possible at all. The `Unresolved { symbol,
  candidates }` shape in particular: a live run answered
  `` `RefundSvc` matches no symbol … Did you mean: RefundService,
  RefundService::run, handle_refund? `` which is a correction, not a dead end.
* `EvidenceRef::AgentAssertion` — the rationale is on the row. I took the two
  daemon match arms as promised (`crates/codypendentd/src/memory_ops.rs`:
  `open_evidence` resolves an assertion to its run's ledger turns, `evidence_label`
  renders `asserted by run <id>: <rationale>`). The CLI arm was already handled by
  the time I got there.
* §6 (the reparse delete) — scoped to `syntax_inferred`. Probed it: asserted the
  edge, appended a function to the `from` file, let the daemon refold it (the new
  symbol appears in `code_nodes`), and the assertion was still there.
* §7 — all four `graph.*` names are registered with curated intents.

**One thing deliberately NOT built, in case you were about to:** a retraction /
delete path for agent-asserted edges. Reasoning is in the doc comment on
`GraphAssertEdge` in `crates/runtime/src/tools/graph.rs`. Short version: a wrong
endpoint writes nothing, re-assertion is idempotent, and a wrong relation leaves a
labelled low-confidence row that anything mechanical outranks — while a
model-callable delete is the one shape that can destroy a resolved fact if its
predicate ever slips, which is precisely the bug §6 was. If you do add one, please
restrict it to `evidence_kind = 'agent_asserted'` AND the asserting session.
