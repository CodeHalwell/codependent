# Smarter memory — design

**Date:** 2026-07-27 · **Status:** draft (pre-implementation) · **Branch:** `claude/smarter-memory`

## Problem

After every run, the memory fabric stores the model's **entire final reply verbatim**
as a single low-signal `Episodic` row. The chain is verified:

1. The agent loop finishes on `ModelStep::Finish { summary }`, where `summary` is the
   whole final reply (`crates/runtime/src/agent.rs:1035`, produced from
   `response.text()` at `agent.rs:2572-2584`).
2. `summary` becomes `RunDisposition::Completed { summary: Some(summary) }` and is
   emitted in the `RunCompleted` event alongside the chronicle artifact
   (`agent.rs:1083-1109`; chronicle assembled by `build_chronicle` at
   `agent.rs:2187-2218`).
3. The observer's `run_outcome_candidates` turns that into
   `CandidateMemory { class: Episodic, statement: format!("Run {run_id} completed: {summary}") }`
   (`crates/knowledge/src/observer.rs:187-243`, specifically the `Completed` arm at
   `observer.rs:198-207`), which `harvest_memories` curates and stores
   (`crates/codypendentd/src/executor.rs:669-714`).

Result: `episodic | "Run <id> completed: <whole reply>"` rows that bury any real
signal. The user wants memory to capture **discrete facts, decisions, and learnings**
instead, using **a mixture of all three** extraction mechanisms below, all composing
through the existing `MemoryStore::curate` pipeline.

## Goals

1. **Stop storing the whole reply.** End the whole-reply-verbatim behaviour at
   `observer.rs:198-207`. Replace it with a tightly bounded breadcrumb (see
   [Mechanism 0](#mechanism-0--stop-the-whole-reply-shared-change)).
2. **Heuristic fact extraction** — pure, no-LLM extractors over the run's chronicle
   and event trace, producing discrete `Semantic` / `Code` / `Episodic` / `Failure`
   / `Procedural` candidates ([Mechanism 1](#mechanism-1--heuristic-extraction)).
3. **Agent-curated `memory.remember` tool** — a core runtime tool the model calls to
   save a fact in its own words, flowing through the dormant `explicit_proposal`
   seam ([Mechanism 2](#mechanism-2--agent-curated-memoryremember-tool)).
4. **Best-effort LLM extraction at harvest** — an optional model call that distills
   the transcript/chronicle into discrete `{statement, evidence, confidence}` facts,
   with a graceful heuristic fallback ([Mechanism 3](#mechanism-3--llm-extraction-at-harvest)).

Each fact becomes its **own** `MemoryRecord` with a short standalone one-line
`statement` and ≥1 provenance `EvidenceRef`, so retrieval renders it as one line in
the `=== MEMORIES ===` block (`crates/knowledge/src/context.rs:187-198`, capped
`MAX_CONTEXT_MEMORIES = 32` at `context.rs:273`). All facts pass through
`MemoryStore::curate` unchanged (`crates/knowledge/src/memory.rs:333-398`), so
redaction, dedup, and contradiction handling are automatic and class-agnostic.

## Architecture

```
                          run reaches terminal state
                                     │
                    spawn_run worker (executor.rs:825)
                                     │
                          harvest_memories(...)
                                     │
        ┌────────────────────────────┼─────────────────────────────┐
        │                            │                              │
  load_events(session)      load chronicle artifact         build FactExtractor
        │                    (best-effort parse)         (LLM client or NoopExtractor)
        │                            │                              │
        ▼                            ▼                              ▼
 extract_candidates(events)   chronicle_candidates(&chronicle)  extractor.extract(input)
  • repeated_command → Proc   • findings  → Semantic/Code        • best-effort model call
  • run_outcome → Episodic★   • changes   → Episodic             • {statement,evidence,conf}
  • explicit_proposal →Sem    • decisions → Semantic               → CandidateMemory
    (picks up memory.remember   • failures → Failure              • ERROR/None ⇒ []  (fallback)
     notes emitted mid-loop)
        │                            │                              │
        └────────────────────────────┴─────────────────────────────┘
                                     │
                     re-anchor scope → Repository (existing)
                                     │
                       for each: MemoryStore::curate
             (redact → scope → dedup >0.92 → contradiction → provenance → retention)
                                     │
                      Accepted / Superseded ⇒ "remembered: …" note
                     (Redacted / Duplicate / Rejected ⇒ dropped)

★ run_outcome Completed arm is CHANGED to a bounded breadcrumb (Mechanism 0).
```

The three mechanisms are **additive producers** of `CandidateMemory` into the same
`harvest_memories` fan-in. `curate` is the single shared sink; overlap between
heuristic, agent-tool, and LLM facts deduplicates there (trigram cosine > `0.92`,
`memory.rs:363`) with no cross-mechanism coordination.

## Components

### Mechanism 0 — stop the whole reply (shared change)

**File:** `crates/knowledge/src/observer.rs`, `run_outcome_candidates`
(`observer.rs:187-243`), the `RunDisposition::Completed { summary }` arm at
`observer.rs:198-207`.

**Decision (chosen): replace with a bounded breadcrumb, do not drop entirely.**
Keep an `Episodic` breadcrumb per completed run so the invariant "every run produces
a curated memory whose provenance opens to its source" (asserted in the
`harvest_memories` doc, `executor.rs:660-668`) still holds and existing tests that
expect a completed-run episodic keep passing — but the statement is a **tightly
bounded summary**, never the whole reply. The real signal now comes from Mechanisms
1–3.

Bounding rule (pure, deterministic):

```
fn breadcrumb(summary: Option<&str>) -> String {
    // first non-empty line, trimmed, hard-capped
    let first = summary
        .and_then(|s| s.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("(no summary)");
    let capped = cap_chars(first, SUMMARY_BREADCRUMB_MAX); // 200 chars, ellipsis on truncation
    format!("Run {run_id} completed: {capped}")
}
```

- `SUMMARY_BREADCRUMB_MAX = 200` chars (new module const in `observer.rs`).
- `cap_chars` truncates on a UTF-8 char boundary and appends `…` when it cuts.
- The breadcrumb candidate gets a **shorter retention** so noise self-expires while
  real facts persist: set `retention: Some(RetentionPolicy { ttl_days: Some(30) })`
  on this candidate only (facts from Mechanisms 1–3 keep the default 365-day policy
  by leaving `retention: None`). `RetentionPolicy` is at
  `crates/knowledge/src/types.rs:374-386`.
- The `Failed { reason }` and `Cancelled { reason }` arms are **unchanged** — those
  strings are already short and bounded.

Rationale for not dropping entirely: dropping is also acceptable per the brief, but
the breadcrumb keeps a navigable "this run happened, here is its chronicle" pointer
that provenance can open, at ~200 chars and 30-day TTL — negligible noise, no
invariant churn.

### Mechanism 1 — heuristic extraction

**File:** `crates/knowledge/src/observer.rs` (new pure extractors) +
`crates/codypendentd/src/executor.rs` (`harvest_memories` loads the chronicle and
calls them).

The existing three extractors (`repeated_command_candidates` → `Procedural`,
`run_outcome_candidates` → `Episodic`, `explicit_proposal_candidates` → `Semantic`)
operate purely over `&[SessionEvent]` (`observer.rs:67-77`). The **chronicle**
(objective / `investigations` = findings / `actions` / `changes` — `build_chronicle`
at `agent.rs:2187-2218`) is persisted only as a JSON artifact referenced by the
`RunCompleted` event, not inlined in events. So heuristic chronicle extraction needs
the chronicle **content**, which `harvest_memories` must load and parse.

**New pure function** in `observer.rs`:

```
/// Discrete heuristic facts from a parsed run chronicle. Pure: data in →
/// candidates out. `chronicle_ref` is the artifact the RunCompleted event cited,
/// used as every candidate's provenance (always available, no session id needed).
pub fn chronicle_candidates(
    chronicle: &serde_json::Value,
    scope: &Scope,
    chronicle_ref: &ArtifactRef,
    observed_at: DateTime<Utc>,
    valid_from: Revision,
    sensitivity: DataClassification,
) -> Vec<CandidateMemory>
```

The chronicle is a plain `serde_json::Value` (not a typed struct), so this reads its
fields defensively (`get(...).and_then(Value::as_array)` etc.); a missing/misshaped
field yields no candidates from that field, never a panic.

Extractors within `chronicle_candidates` (each emits one candidate per matched item,
capped per class — see Constraints):

| Source field        | Detection (pure)                                                                 | Class       | Statement shape |
|---------------------|----------------------------------------------------------------------------------|-------------|-----------------|
| `investigations[]`  | line contains a `path:line` reference (regex `[\w./-]+\.[a-z]+:\d+`)             | `Code`      | `"<path>:<line> — <bounded surrounding finding>"` |
| `investigations[]`  | line with no code ref but ≥ N words (a prose finding)                             | `Semantic`  | bounded finding text |
| `investigations[]`  | line beginning with a decision marker (`decided`, `chose`, `will use`, `because`)| `Semantic`  | bounded decision text |
| `changes[]`         | a changeset digest entry (path + add/del)                                        | `Episodic`  | `"Changed <path> (+A −D) in run <id>"` |
| `actions[]`         | an action digest whose outcome is `failed`/`denied`/`rejected` for a tool        | `Failure`   | `"<tool> <outcome> in run <id>"` |

Notes on the mapping:
- **file:line findings → `Code`** (the most specific class for a fact anchored to a
  code location); prose findings and decisions → `Semantic` (durable asserted facts);
  changes made → `Episodic` (something that happened this run); tool failures →
  `Failure`. Repeatable procedures remain the job of the existing
  `repeated_command_candidates` (`Procedural`). Learnings the model states in prose
  are `Semantic`; a learning phrased as a repeatable "how-to" is better captured by
  the agent tool (Mechanism 2) or the LLM extractor (Mechanism 3) as `Procedural`.
- Every candidate cites `EvidenceRef::Artifact { artifact: chronicle_ref.clone(), source_path: None }`
  (mirrors the existing `run_outcome_candidates` at `observer.rs:229-232`), so
  provenance is always present without a session id.
- `confidence = OBSERVED_CONFIDENCE` (0.6, the existing const at `observer.rs:44`).
- `sensitivity` is inherited from the chronicle artifact's classification (same as
  `run_outcome_candidates` at `observer.rs:238`) so a sensitive run cannot become a
  less-restricted memory.
- Each statement is independently capped (`SUMMARY_BREADCRUMB_MAX`-style bound) so no
  single fact re-imports a wall of text.

**`harvest_memories` change** (`executor.rs:669-714`): after `load_events`, locate the
`RunCompleted` event, take its `chronicle` `ArtifactRef`, load the bytes via
`self.artifacts()` (the `ArtifactStore` the executor already holds — used by
`recovery::fail_run` at `executor.rs:787`), `serde_json::from_slice` them, and call
`chronicle_candidates(...)`. Every step is best-effort: a load/parse failure is
warned and skipped (no chronicle candidates), never fatal — consistent with the
existing swallow-errors contract. Append these candidates to the vector before the
existing re-anchor-to-`Repository` + `curate` loop.

### Mechanism 2 — agent-curated `memory.remember` tool

A **dormant seam** exists: `explicit_proposal_candidates` (`observer.rs:245-287`)
already turns a `NoteAppended` note whose text starts with `memory.propose:` or
`memory:` (the `PROPOSE_MARKERS` const, `observer.rs:53`) into a `Semantic`
candidate citing the note's own event. No tool emits such a note today. This
mechanism adds the tool.

**Tool identity & schema.** New core tool `memory.remember`:

```
name: "memory.remember"
description: "Save a durable fact, decision, or learning to long-term memory in your
              own words. Use for a discrete fact worth recalling in future runs —
              not a summary of what you just did. One fact per call."
parameters (JSON schema):
  {
    "type": "object",
    "properties": {
      "statement": { "type": "string" },          // the one-line fact, required
      "value":     {}                              // optional structured JSON value
    },
    "required": ["statement"]
  }
```

**Execution → note.** When dispatched, the tool emits a `NoteAppended` event into the
run's ledger via the agent loop's own `self.emit(...)`
(`crates/runtime/src/agent.rs:1118-1127`), text `format!("memory.propose: {statement}")`,
`run_id: Some(run.run_id)`. That is the entire side effect. At harvest,
`ledger::load_events` reloads the note and `explicit_proposal_candidates` turns it
into a `Semantic` candidate — no new harvest wiring needed. The tool returns a short
observation (`"noted for memory: <statement>"`) so the model gets confirmation. The
run-loop wrapper `run_tool` (`agent.rs:1182-1363`) already emits the surrounding
`ToolStarted` / `ToolCompleted`; the note append sits between them and is picked up
because harvest reads all persisted events.

**Optional `value`.** To carry the optional structured value (and, later, a class
hint) without breaking the existing marker seam, `explicit_proposal_candidates` gets
a **minimal backward-compatible extension**: if the text after the marker parses as
`<statement>\n\x1e<json>` (an ASCII Record-Separator `\x1e` delimiter the tool
inserts only when `value` is present), the JSON tail populates
`CandidateMemory.structured_value`; otherwise the whole remainder is the statement
exactly as today (existing notes and `memory:` usages are unaffected). The class
stays `Semantic` (the existing seam's class). `structured_value` is redacted by
`curate` gate (a) just like the statement (`memory.rs:343-348`), and is *not*
surfaced by retrieval render — so the standalone one-line requirement is met by
`statement` alone.

**Registration (verified against the registry & runtime tool set):**

- **Runtime dispatch** (`crates/runtime/src/tools/`): add a small module
  `tools/memory.rs` exposing `MemoryRemember` with `NAME = "memory.remember"`, an
  input parser, and a `proposed_action` builder; re-export from `tools/mod.rs`
  (`mod.rs:30-66`).
- **Offered set — CORE, not workflow-gated.** Add `MemoryRemember::NAME` to the
  **baseline** vector in `FrameworkAgentRuntime::offered_tool_names`
  (`agent.rs:804-825`) — the first `vec![Shell, ReadFile, Search, GitDiff, ApplyPatch]`
  that is returned unconditionally for every run. It is NOT behind the `github`
  client check or `offers_blackboard` (`agent.rs:791-796`) gate. This is the single
  source of truth the model-facing advertisement (`advertised_tools`,
  `agent.rs:2445-2451`) and dispatch (`prepare`) agree on, so being in the baseline
  makes it always advertised and always dispatchable (post the FIX-1
  advertise/execute-parity change).
- **Static catalog:** add a `decl(MemoryRemember::NAME, …)` entry to
  `tool_definitions()` (`agent.rs:2279-2430`) with the schema above.
- **Dispatch arm:** add a `MemoryRemember::NAME =>` arm to `prepare`
  (`agent.rs:1368-1498`) building `PreparedTool::MemoryRemember(input)`, and a
  matching arm to `execute_prepared` (`agent.rs:1544-1718`) that performs the
  `self.emit(NoteAppended …)` and returns the observation. Extend the `PreparedTool`
  enum (`agent.rs:1882`).
- **Policy / capability:** the tool touches no filesystem and spawns no process; it
  only appends a note. Model its `ProposedAction` as a benign, no-capability action
  evaluated `Decision::Allow` at low risk (mirror how the blackboard post's action is
  built and passes policy — `agent.rs:1470-1481`). If no existing `ProposedAction`
  variant fits a "record a memory note" action, add a `ProposedAction::RecordMemory`
  variant with a default-`Allow`, `RiskClass::Safe`/`Low` policy rule. **This is the
  one place a new internal type may be needed; it is internal to the daemon, not on
  the wire.**
- **Registry builtin (optional, for discoverability):** add a `memory.remember`
  entry to `builtin_tools()` (`crates/knowledge/src/builtin.rs:42+`) as a
  `System`-scoped, `FirstParty`, low-risk built-in so it can be ranked/disclosed in
  the `=== TOOLS ===` context block. Optional because core tools are advertised
  regardless of registry disclosure; include it for parity with the other built-ins.

**Protocol / wire:** none required. `ToolStarted` / `ToolCompleted` /
`NoteAppended` already exist (`crates/protocol/src/events.rs:70`). Adding a tool only
changes the internal, per-run advertised set. **Flag:** any golden snapshot of the
advertised tool catalog or `tool_definitions` will gain one additive entry — update
those goldens; the change is purely additive.

### Mechanism 3 — LLM extraction at harvest

`harvest_memories` is already `async` and already swallows all errors
(`executor.rs:660-668`). Add a **best-effort model call** there that distills the
run's chronicle/transcript into discrete facts, each becoming a `CandidateMemory`
through `curate`.

**The knowledge crate makes zero model calls today**, so we do NOT put a model client
in `crates/knowledge`. Instead:

- **Seam (in `crates/knowledge`):** define a trait and a no-op default:

  ```
  #[async_trait]
  pub trait FactExtractor: Send + Sync {
      /// Best-effort: distil bounded run context into candidate facts.
      /// MUST NOT error the caller — return Ok(vec![]) on any internal failure.
      async fn extract(&self, input: ExtractionInput<'_>) -> Vec<CandidateMemory>;
  }

  pub struct ExtractionInput<'a> {
      pub objective: &'a str,
      pub chronicle: &'a serde_json::Value,
      pub transcript_excerpt: &'a str,   // bounded, see Constraints
      pub scope: &'a Scope,
      pub chronicle_ref: &'a ArtifactRef,
      pub observed_at: DateTime<Utc>,
      pub valid_from: Revision,
      pub sensitivity: DataClassification,
  }

  /// The default when no model is configured: produces nothing.
  pub struct NoopExtractor;
  ```

  Returning `Vec` (not `Result`) makes the never-fail contract structural.

- **Implementation (in the runtime/daemon layer, which already depends on
  `ModelRegistry` / `ChatClient`):** `LlmFactExtractor` wraps an
  `Arc<dyn ChatClient>` built via `ModelRegistry::client_for` (`models.rs:287-290`).
  It sends a fixed extraction prompt asking for a JSON array of
  `{ kind, statement, evidence, confidence }`, parses defensively, and maps each into
  a `CandidateMemory`. Any error (build failure, timeout, non-JSON, empty) ⇒ log +
  return `vec![]`.

- **`harvest_memories` wiring:** build the extractor once; if construction fails or
  no model is configured, use `NoopExtractor`. Call `extractor.extract(input)` and
  append the results to the candidate vector alongside Mechanisms 0/1/2, then curate
  all uniformly.

**Model-selection decision (chosen).** Selection order, fail-safe at every step:

1. A **configured dedicated/utility extraction model** if present — reuse the
   Phase-7 utility-model seam. Config key: an optional `memory_extraction_model`
   (a `ModelId`) read alongside routing config in `load_model_registry` /
   `RuntimeExecutor` (`executor.rs:936-961`). This lets an operator point extraction
   at a cheap/local model, keeping cost off the expensive coding model.
2. Else the **run's own resolved model** (`model_id` already resolved in `execute`,
   `executor.rs:380-452`) — so the feature works out of the box with zero extra
   config. To keep it best-effort at harvest (which runs *after* `execute` returns),
   harvest re-resolves through the same `load_registry()` + `resolve_model` path
   rather than threading the client out of the loop.
3. Else (no model configured at all — a bare environment, the documented
   `models.toml`-absent case at `executor.rs:942-948`) ⇒ `NoopExtractor`, heuristic +
   agent-tool paths still work with zero model calls.

**Fallback (chosen).** On ANY error, missing config, timeout, or empty output, the
LLM path contributes nothing and harvest proceeds on Mechanisms 0/1/2. Never fails
the run — honours the existing swallow-errors contract.

**Bounding (chosen).**
- Input: transcript excerpt hard-capped to `LLM_EXTRACT_INPUT_MAX ≈ 32_000` chars
  (~8k tokens); when over, keep the tail (most recent turns) and drop the head.
  Chronicle passed as-is (already compact).
- Output: at most `LLM_EXTRACT_MAX_FACTS = 10` facts accepted; extras dropped.
- Each `statement` capped to 200 chars; `confidence` clamped to `[0,1]`; a fact with
  an empty statement is dropped.
- Wall-clock: the model call is wrapped in a `tokio::time::timeout`
  (`LLM_EXTRACT_TIMEOUT = 30s`); on elapse ⇒ `vec![]`. A hung model never wedges
  harvest.

**Provenance & class for LLM facts.** Each fact cites
`EvidenceRef::Artifact { artifact: chronicle_ref, source_path: None }` (always
available). The model returns a `kind`, mapped:

| `kind` from model | `MemoryClass` |
|-------------------|---------------|
| `finding` / `fact`| `Semantic`    |
| `decision`        | `Semantic`    |
| `learning` / `procedure` | `Procedural` |
| `failure` / `pitfall`    | `Failure`    |
| `preference`      | `Preference`  |
| (unknown / absent)| `Semantic`    |

`confidence` comes from the model (clamped); if absent, default `OBSERVED_CONFIDENCE`
(0.6). `sensitivity` inherited from the chronicle artifact.

## Data flow (fact → durable record)

Every candidate from Mechanisms 0–3 is a `CandidateMemory`
(`memory.rs:409-419`) with `class`, `scope` (re-anchored to `Repository` by harvest,
`executor.rs:689-693`), a short `statement`, optional `structured_value`, ≥1
`provenance`, `confidence`, `observed_at`, `valid_from`, `sensitivity`, `retention`.

`harvest_memories` curates each through `MemoryStore::curate` (`memory.rs:333-398`),
whose gate order is normative:

1. **secret / sensitivity filter FIRST** (`detect_secret`, `memory.rs:339-348`,
   645-741) — over both `statement` and `structured_value`; a secret ⇒ `Redacted`,
   nothing stored. This is why the agent tool can safely append a `memory.propose:`
   note and the LLM can emit a statement — a secret can never leak past this gate.
2. **scope classification** (`classify_scope`).
3. **dedup** — same-scope/same-class live memory > `0.92` trigram cosine ⇒
   `Duplicate`. This is where overlapping facts from heuristic + LLM + agent tool
   collapse automatically, with no cross-mechanism coordination.
4. **contradiction → supersession** — an evidence-bearing candidate contradicting a
   live same-scope/same-class memory supersedes it (never deletes).
5. **provenance** — zero `EvidenceRef` ⇒ `Rejected` ("evidence-free"). Every
   mechanism above attaches ≥1 ref, so none is rejected here.
6. **retention + insert** — default 365 days, or the 30-day breadcrumb TTL from
   Mechanism 0.

Accepted / Superseded records get a `"remembered: <statement>"` note
(`executor.rs:697-708`); Redacted / Duplicate / Rejected are silently dropped
(`executor.rs:709-710`).

Retrieval (`assemble_context`, `context.rs:218-267`) later queries `System` +
`Repository` scopes, projects each `MemoryRecord` to one `ContextMemory` line
(`context.rs:97-108`), and renders `- <statement> (confidence …, rev …; source: …)`
in the capped `=== MEMORIES ===` block. Because each fact is its own record with a
standalone `statement`, each surfaces as its own retrievable line.

## Error handling

- **Harvest is best-effort throughout** (`executor.rs:669`, "a harvesting error must
  not turn a finished run into a failed one"). Every new step preserves this:
  - chronicle artifact load/parse failure ⇒ warn + skip chronicle candidates.
  - `FactExtractor::extract` returns `Vec` (cannot error the caller); the LLM impl
    swallows internally and returns `vec![]`.
  - a `curate` error on one candidate is already warned and the loop continues
    (`executor.rs:711`).
- **`memory.remember` tool:** a malformed args payload is a `prepare` error surfaced
  as a normal `ToolCompleted { Failed }` (the existing path at `agent.rs:1194-1210`);
  it never crashes the loop. An `emit` failure while writing the note propagates as a
  loop error exactly like any other `self.emit` (consistent with existing tools).
- **Secrets:** the redaction gate runs first and unconditionally for every candidate
  regardless of mechanism, so no mechanism can store a secret.
- **Observer purity preserved:** `chronicle_candidates` and the extended
  `explicit_proposal_candidates` remain pure functions (data in → candidates out),
  so they stay directly unit-testable without a daemon.

## Testing

- **Mechanism 0 (bounded breadcrumb):** unit-test `run_outcome_candidates` (and/or a
  `breadcrumb` helper) — a long multi-line summary yields a single ≤200-char
  first-line statement with `…`; a `None` summary yields `"(no summary)"`; the
  candidate carries the 30-day retention; Failed/Cancelled arms unchanged.
- **Mechanism 1 (heuristic):** unit-test `chronicle_candidates` over hand-built
  chronicle `Value`s — a `path:line` finding → one `Code` candidate with the right
  statement + chronicle-artifact provenance; a prose finding → `Semantic`; a change
  entry → `Episodic`; a failed action → `Failure`; a missing/misshaped field → no
  candidates, no panic; per-class caps enforced. All pure, no daemon.
- **Mechanism 2 (agent tool):** (a) unit-test the note→candidate path: a
  `NoteAppended` "memory.propose: X" → one `Semantic` candidate with statement "X"
  (existing `explicit_proposal_candidates` test surface); the `\x1e`-delimited
  `value` variant → `structured_value` populated, statement clean; a plain
  `memory:` note still works (backward compat). (b) runtime test:
  `offered_tool_names` includes `memory.remember` for a plain (non-workflow,
  no-github) run; `advertised_tools` surfaces it; a `prepare`/`execute_prepared`
  round-trip emits a `NoteAppended` with the `memory.propose:` prefix.
- **Mechanism 3 (LLM):** inject a **mock `FactExtractor`** into a harvest test:
  (a) a mock returning two facts ⇒ two candidates curated; (b) a mock returning
  `vec![]` (simulating error/timeout/no-model) ⇒ harvest still completes and the
  heuristic + agent-tool candidates are curated (the **fallback assertion**);
  (c) input-bounding: an over-long transcript is truncated to the tail before the
  extractor sees it; (d) output-bounding: > 10 facts are capped, over-long
  statements are truncated.
- **Shared:** an existing `curate` dedup test extended to show a heuristic fact and
  an identical LLM fact collapse to one stored record (cross-mechanism dedup).
- **No regression:** existing observer/harvest tests updated for the bounded
  breadcrumb; the additive tool goldens (advertised catalog) regenerated.

## Constraints

- **Harvest never fails a run.** Every new step is best-effort and swallows errors,
  preserving the contract at `executor.rs:660-668`.
- **All facts flow through `curate` unchanged.** No mechanism bypasses redaction,
  dedup, or contradiction; `memory.rs:333-398` is untouched.
- **Each fact = its own retrievable one-line `statement` with ≥1 provenance.** No
  packing multiple facts into one statement; nothing hidden only in
  `structured_value_json` (retrieval never renders it). Every candidate carries ≥1
  `EvidenceRef` (chronicle artifact or note event range).
- **Secrets never stored.** Redaction is the first curate gate and runs for every
  candidate regardless of source.
- **LLM extraction is optional and fallback-safe.** Heuristic (Mechanism 1) + agent
  tool (Mechanism 2) + bounded breadcrumb (Mechanism 0) all work with **zero model
  calls** when no extraction model is configured (`NoopExtractor`).
- **Bounding is mandatory** on the LLM path: input ≤ ~32k chars (tail kept), ≤ 10
  facts out, ≤ 200-char statements, 30s timeout.
- **Per-class heuristic caps:** `chronicle_candidates` emits at most a small fixed
  number per class per run (proposed: 8 findings, 8 changes, 8 failures) so a
  pathological chronicle cannot flood the ledger; the 32-memory context ceiling
  (`context.rs:273`) is a display cap, not a storage cap, so storage discipline lives
  here.
- **Protocol / wire / goldens:** no wire change. `NoteAppended`, `ToolStarted`,
  `ToolCompleted` already exist. The only golden churn is **additive**: the advertised
  tool catalog / `tool_definitions` gains the `memory.remember` entry. A possible new
  **internal** `ProposedAction::RecordMemory` variant is daemon-internal, not on the
  wire.

## Open questions (for review)

1. **Breadcrumb vs. drop (Mechanism 0).** Chosen: keep a 200-char, 30-day-TTL
   `Episodic` breadcrumb (preserves the "every run yields a curated memory" invariant
   and existing tests). Confirm this over dropping the completed-run episodic
   entirely.
2. **Extraction model default (Mechanism 3).** Chosen: dedicated
   `memory_extraction_model` if configured, else the run's own model, else
   `NoopExtractor`. Confirm using the run's (potentially expensive) coding model as
   the fallback is acceptable, versus "utility model or nothing" (which would make
   the LLM path dead until an operator configures it).
3. **`value` seam extension (Mechanism 2).** Chosen: a minimal `\x1e`-delimited
   backward-compatible extension to `explicit_proposal_candidates` so the tool's
   optional `value` reaches `structured_value`, keeping class `Semantic`. Confirm
   this over the strictly-minimal alternative (tool emits statement-only; drop the
   `value` arg) — the strictly-minimal alternative needs no observer change at all.
