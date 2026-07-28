# Smarter memory — fact extraction into the curate pipeline — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Use superpowers:test-driven-development inside each task: write the failing test FIRST, then the implementation.

**Goal:** Replace "store the whole final reply as one Episodic memory" with discrete-fact extraction via three additive producers — a bounded completed-run breadcrumb (M0), pure heuristic chronicle extraction (M1), an agent-called `memory.remember` tool (M2), and an on-by-default, bounded, fallback-safe LLM extractor (M3) — all fanning into the existing `MemoryStore::curate` pipeline unchanged. Each fact becomes its own `MemoryRecord` with a short standalone one-line `statement` and ≥1 provenance ref.

**Architecture:** The observer (`crates/knowledge/src/observer.rs`) stays a set of PURE `&[SessionEvent] → Vec<CandidateMemory>` (and now `&Value → Vec<CandidateMemory>`) extractors. `harvest_memories` (`crates/codypendentd/src/executor.rs`) is the single fan-in: it loads the ledger, loads+parses the `RunCompleted` chronicle artifact, calls an injected `&dyn FactExtractor`, appends all candidates, re-anchors each to `Repository` scope, and curates each through `MemoryStore::curate` (redaction → scope → dedup >0.92 → contradiction → provenance → retention) exactly as today. The `FactExtractor` seam (trait + `NoopExtractor`) lives in `crates/knowledge` with ZERO model deps; the model-backed `LlmFactExtractor` lives in `crates/runtime` (ADR-009: the only crate that may depend on `agent-framework-rs`) and is selected/built by the daemon. `harvest_memories` stays best-effort — every new step swallows its errors and never fails a finished run.

**Tech Stack:** Rust workspace. `serde_json` for chronicle/value parsing (no `regex` in `crates/knowledge` — code-ref detection is a hand-rolled scanner). `async-trait` (already a dep of both `knowledge` and `runtime`). The LLM path reuses `ModelRegistry::client_for` + `agent_framework_core::client::ChatClient` behind the existing `provider-openai` feature (default on). Tests: pure unit tests (observer, tool parse/format, bounds helpers) + async harvest tests over a `test_pool` with a mock `FactExtractor`.

## Global Constraints

Every task's requirements implicitly include this section (from `docs/superpowers/specs/2026-07-27-smarter-memory-design.md` + the locked decisions D1/D2/D3):

- **ALL candidate facts flow through `MemoryStore::curate` UNCHANGED** (`crates/knowledge/src/memory.rs:333-398`): redaction FIRST (secrets never stored), scope classification, dedup at `> 0.92` trigram cosine, contradiction → supersession, ≥1 provenance required, retention. `curate` and the `memories` table are NOT modified.
- **Each fact = its OWN `MemoryRecord` with a short standalone one-line `statement`** — renders as one `- <statement> (…)` line in `=== MEMORIES ===` (`context.rs:191-198`, capped `MAX_CONTEXT_MEMORIES = 32`). Never pack multiple facts into one statement; nothing carried only in `structured_value` (retrieval never renders it). Correct `MemoryClass` per fact type (spec mapping tables).
- **`harvest_memories` never fails a run** (`executor.rs:660-668`). Every new step (chronicle load/parse, `FactExtractor::extract`) is best-effort and swallows errors; a per-candidate `curate` error is already warned and the loop continues.
- **LLM extraction is on-by-default, bounded, config-visible, total-fallback (D2).** Selection order: (1) configured `memory_extraction_model`; (2) else the run's own resolved model; (3) else `NoopExtractor`. Bounds: input ≤ 32_000 chars (keep the tail), ≤ 10 facts, ≤ 200-char statements, 30s timeout. ANY error/timeout/missing/empty ⇒ contribute nothing; heuristic + agent-tool paths still populate memory; the run NEVER fails. A one-time log note warns that extraction makes a per-run model call and to point `memory_extraction_model` at a cheap model.
- **`FactExtractor::extract` returns `Vec<CandidateMemory>`, never `Result`** — the never-fail contract is structural.
- **No new EXTERNAL/cargo-deny dependency.** The LLM path reuses the model registry. One internal workspace edge is added: `crates/runtime` → `crates/knowledge` (so `LlmFactExtractor` can implement the trait / return `CandidateMemory`). No cycle (`knowledge` does not depend on `runtime`).
- **No protocol/wire/schema/migration change.** `NoteAppended` / `ToolStarted` / `ToolCompleted` already exist; the `memories` table already has `statement` / `structured_value_json` / `provenance_json` / `retention_json` (`migrations/0003_phase2.sql:56-76`). The only golden/snapshot churn possible is the advertised-tool catalog +1 — and it is pinned only by UNIT tests (`agent.rs` `advertised_tools_*`), not a `.snap`/golden-vector file, so no snapshot regen is needed (assert the new entry in those unit tests).
- **`ProposedAction::RecordMemory`** is added to the protocol enum (which is `#[non_exhaustive]` with `#[serde(other)] Unknown`). It is ALWAYS `Decision::Allow`, so it is never serialized into a `ToolProposed` wire event and needs NO golden vector. The policy engine's `evaluate` has a `_ => deny` catch-all (`policy/mod.rs:279`), so an explicit Allow arm MUST be added.
- **Honesty & trust-boundary framing untouched.** Memories remain retrieved *evidence* (`context.rs` preamble), never instructions.
- **Every affected test updated + new tests per task.** Foreign files never touched: `README.md`, `docs/cli-and-tui-user-guide.md`, `docs/docs/*`, `ROADMAP.md`, `.superpowers/`, `.idea/`, any untracked docs.

---

## Shared Interfaces

Exact signatures tasks depend on. A task's implementer sees only their own task; this block is how neighboring tasks agree on names and shapes.

**M0 → (self-contained), M1 (reuses `cap_chars`), in `crates/knowledge/src/observer.rs`:**

```rust
/// Max chars of a completed-run breadcrumb statement (D1). The whole reply is
/// never stored — only its first non-empty line, capped.
const SUMMARY_BREADCRUMB_MAX: usize = 200;
/// Retention (days) of the completed-run breadcrumb (D1) — noise self-expires
/// while real facts (Mechanisms 1–3) keep the default 365-day policy.
const BREADCRUMB_TTL_DAYS: u32 = 30;

/// Truncate `s` to at most `max` chars on a UTF-8 char boundary, appending `…`
/// only when it actually cut. Pure.
fn cap_chars(s: &str, max: usize) -> String;
```

**M1 → M3 (both cite the chronicle artifact + share the class map intent), in `crates/knowledge/src/observer.rs`:**

```rust
/// Discrete heuristic facts from a parsed run chronicle. Pure: data in →
/// candidates out; a missing/misshaped field yields no candidates from that
/// field, never a panic. Every candidate cites the chronicle artifact
/// (`EvidenceRef::Artifact { artifact: chronicle_ref.clone(), source_path: None }`),
/// so provenance is always present without a session id.
pub fn chronicle_candidates(
    chronicle: &serde_json::Value,
    scope: &Scope,
    chronicle_ref: &codypendent_protocol::ArtifactRef,
    run_id: RunId,
    observed_at: chrono::DateTime<chrono::Utc>,
    valid_from: Revision,
    sensitivity: DataClassification,
) -> Vec<CandidateMemory>;
```

Per-class caps (module consts): `const MAX_CHRONICLE_FINDINGS: usize = 8;`, `const MAX_CHRONICLE_CHANGES: usize = 8;`, `const MAX_CHRONICLE_FAILURES: usize = 8;`.

**M2 note format (shared M2 tool ⇄ observer `explicit_proposal_candidates`):**

The tool emits one `NoteAppended` whose text is:
- value ABSENT: `memory.propose: <statement>`
- value PRESENT: `memory.propose: <statement>\u{1e}<compact-json-value>` (a single ASCII Record Separator `\u{1e}` = `0x1E` delimiter)

`explicit_proposal_candidates` splits the post-marker remainder on the FIRST `\u{1e}`: left → `statement` (trimmed), right → `serde_json::from_str` → `structured_value` (on `Ok`; a parse failure leaves `structured_value = None` and folds the whole remainder into the statement). No `\u{1e}` ⇒ today's behavior exactly (backward compatible with existing `memory:` / `memory.propose:` notes).

**M2 tool API, in new `crates/runtime/src/tools/memory.rs`:**

```rust
pub struct MemoryRemember;
impl MemoryRemember {
    pub const NAME: &'static str = "memory.remember";
    /// ASCII Record Separator delimiting the optional structured value tail.
    pub const RECORD_SEPARATOR: char = '\u{1e}';
    #[must_use] pub fn proposed_action() -> codypendent_protocol::ProposedAction; // RecordMemory
    /// The `NoteAppended` text (see the shared note format above).
    #[must_use] pub fn note_text(input: &MemoryRememberInput) -> String;
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRememberInput { pub statement: String, pub value: Option<serde_json::Value> }

/// `statement` required + non-empty; `value` optional (null ⇒ None).
pub fn parse_memory_remember(args: &serde_json::Value) -> Result<MemoryRememberInput, String>;
```

**M3a → M3b (the `FactExtractor` seam), in new `crates/knowledge/src/extractor.rs`:**

```rust
#[async_trait::async_trait]
pub trait FactExtractor: Send + Sync {
    /// Best-effort: distil bounded run context into candidate facts. MUST NOT
    /// error the caller — returns `vec![]` on ANY internal failure.
    async fn extract(&self, input: ExtractionInput<'_>) -> Vec<CandidateMemory>;
}

pub struct ExtractionInput<'a> {
    pub objective: &'a str,
    pub chronicle: &'a serde_json::Value,
    pub transcript_excerpt: &'a str,        // bounded by the impl (tail kept)
    pub scope: &'a Scope,
    pub chronicle_ref: &'a codypendent_protocol::ArtifactRef,
    pub run_id: codypendent_protocol::RunId,
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub valid_from: Revision,
    pub sensitivity: codypendent_protocol::DataClassification,
}

/// The default when no model is configured: produces nothing.
pub struct NoopExtractor;
```

**M3b → daemon (the model-backed impl + config key), in `crates/runtime/src/extractor.rs` (feature `provider-openai`) + `crates/codypendentd/src/routing.rs`:**

```rust
// runtime — bounds (D2):
const LLM_EXTRACT_INPUT_MAX: usize = 32_000;
const LLM_EXTRACT_MAX_FACTS: usize = 10;
const LLM_EXTRACT_STATEMENT_MAX: usize = 200;
const LLM_EXTRACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(feature = "provider-openai")]
pub struct LlmFactExtractor { /* client: Arc<dyn ChatClient>, model_id: ModelId */ }
#[cfg(feature = "provider-openai")]
impl LlmFactExtractor {
    pub fn new(client: std::sync::Arc<dyn agent_framework_core::client::ChatClient>, model_id: ModelId) -> Self;
    pub async fn from_registry(models: &ModelRegistry, model_id: ModelId) -> anyhow::Result<Self>;
}
// implements codypendent_knowledge::FactExtractor.

// codypendentd routing.rs — the D2 config key (optional; absent ⇒ fall through to the run's model):
// RoutingConfig gains `pub memory_extraction_model: Option<ModelId>`, parsed from
// routing.toml via a matching `#[serde(default)] memory_extraction_model: Option<ModelId>`
// on RoutingConfigFile; default None.
```

**M3a/M3b harvest seam, in `crates/codypendentd/src/executor.rs`:**

```rust
// harvest_memories gains the run's mode (for D2 model resolution) and delegates:
async fn harvest_memories(&self, session_id: SessionId, run_id: RunId, repository: RepositoryId, mode: AgentMode);
// M3a introduces the testable core taking an injected extractor:
async fn harvest_with(&self, session_id: SessionId, run_id: RunId, repository: RepositoryId, extractor: &dyn FactExtractor);
// M3b builds the extractor via the D2 selection order (Noop in M3a):
async fn build_fact_extractor(&self, mode: AgentMode) -> Box<dyn FactExtractor>;
```

---

## Task 1 — M0: bounded completed-run breadcrumb (stop the whole reply)

**File:** `crates/knowledge/src/observer.rs` (pure). Independently unit-testable.

- [ ] **Test first.** In `observer.rs` tests, add/adjust:
  - `completed_run_yields_bounded_breadcrumb_not_whole_reply`: build a `RunCompleted` `SessionEvent` with `RunDisposition::Completed { summary: Some("<first line>\n<... 500 more chars ...>") }`; assert the single candidate is `MemoryClass::Episodic`, `statement` starts `"Run <id> completed: "`, its statement length ≤ `"Run <id> completed: ".len() + SUMMARY_BREADCRUMB_MAX + 'ellipsis'`, contains only the FIRST non-empty line, ends with `…` (truncation), and `retention == Some(RetentionPolicy { ttl_days: Some(30) })`.
  - `completed_run_none_summary_uses_placeholder`: `summary: None` ⇒ statement `"Run <id> completed: (no summary)"`, retention 30 days.
  - `cap_chars` unit tests: no truncation when under `max` (no `…`), char-boundary-safe truncation on a multibyte string, `…` appended only when cut.
  - Update ANY existing test that asserted the old `"Run <id> completed: {full summary}"` verbatim.
  - Assert Failed/Cancelled arms are UNCHANGED (existing `Run <id> failed: <reason>` / cancelled tests still pass, `retention: None`).
- [ ] Add module consts `SUMMARY_BREADCRUMB_MAX = 200`, `BREADCRUMB_TTL_DAYS = 30`, and pure `fn cap_chars(s: &str, max: usize) -> String` (count chars; if `> max`, find the byte index of the `max`-th char and push `'…'`).
- [ ] Add `use crate::types::RetentionPolicy;` to the existing `use crate::types::{...}` line.
- [ ] In `run_outcome_candidates`, replace the `Completed` arm's statement build and thread a per-arm retention. Compute `(class, statement, retention)`:
  ```rust
  let (class, statement, retention) = match disposition {
      RunDisposition::Completed { summary } => {
          let first = summary
              .as_deref()
              .and_then(|s| s.lines().map(str::trim).find(|l| !l.is_empty()))
              .unwrap_or("(no summary)");
          (
              MemoryClass::Episodic,
              format!("Run {run_id} completed: {}", cap_chars(first, SUMMARY_BREADCRUMB_MAX)),
              Some(RetentionPolicy { ttl_days: Some(BREADCRUMB_TTL_DAYS) }),
          )
      }
      RunDisposition::Failed { reason } => (
          MemoryClass::Failure,
          format!("Run {run_id} failed: {reason}"),
          None,
      ),
      RunDisposition::Cancelled { reason } => (
          MemoryClass::Episodic,
          format!(
              "Run {run_id} cancelled{}",
              reason.as_ref().map(|r| format!(": {r}")).unwrap_or_default()
          ),
          None,
      ),
      _ => continue,
  };
  ```
  and change the `CandidateMemory { … retention: None }` push to `retention`.
- [ ] `cargo test -p codypendent-knowledge observer` green.

---

## Task 2 — M1: heuristic `chronicle_candidates` + harvest wiring

**Files:** `crates/knowledge/src/observer.rs` (new pure fn) + `crates/codypendentd/src/executor.rs` (`harvest_memories` loads+parses the chronicle and calls it). Depends on Task 1's `cap_chars`.

Chronicle field reality (verified in `agent.rs build_chronicle`): findings live in `chronicle["investigations"]` as an array of plain STRINGS; `chronicle["changes"]` entries are `{ "changeset_id", "artifact", "byte_length" }` (NO path/add/del — read what is actually there); `chronicle["actions"]` entries are `{ "tool", "outcome", "artifact" }`; `chronicle["decisions"]` is always `[]` (so decisions are detected from `investigations` lines, per the spec mapping).

- [ ] **Test first.** In `observer.rs` tests, add `chronicle_candidates_*`:
  - a `investigations` line containing a code ref (e.g. `"crates/x/src/a.rs:42 the guard is inverted"`) → exactly one `MemoryClass::Code` candidate whose statement contains `crates/x/src/a.rs:42`; provenance is the chronicle `EvidenceRef::Artifact`.
  - a prose `investigations` line (no code ref, ≥ 4 words) → one `MemoryClass::Semantic` candidate.
  - an `investigations` line starting with a decision marker (`"decided to use sqlx over diesel"`) → one `MemoryClass::Semantic` candidate.
  - a `changes` entry → one `MemoryClass::Episodic` candidate mentioning the changeset + `run_id`.
  - an `actions` entry with `outcome:"failed"` → one `MemoryClass::Failure` candidate `"<tool> failed in run <id>"`; an `outcome:"succeeded"` action yields NO candidate.
  - missing/misshaped fields (`chronicle = json!({})`, `investigations` a string not array, entries missing keys) → `vec![]`, no panic.
  - per-class caps: 20 findings/changes/failures each cap at 8.
  - every candidate's statement is length-bounded (`cap_chars` applied).
- [ ] In `observer.rs`, extend imports: add `ArtifactRef, RunId` to the `codypendent_protocol::{…}` use; add `use chrono::{DateTime, Utc};`.
- [ ] Add caps consts and decision markers const (`const DECISION_MARKERS: [&str; 4] = ["decided", "chose", "will use", "because"];`).
- [ ] Add a pure, regex-free code-ref scanner:
  ```rust
  /// The first `<path>.<ext>:<line>` token in `line` (e.g. `src/a.rs:42`), or None.
  /// Regex-free (this crate has no `regex` dep): scan whitespace-split tokens for
  /// one that has a `.<ext>` then a `:` then ASCII digits.
  fn code_ref(line: &str) -> Option<&str> {
      line.split_whitespace().find(|tok| {
          let Some((path, line_no)) = tok.rsplit_once(':') else { return false };
          !line_no.is_empty()
              && line_no.bytes().all(|b| b.is_ascii_digit())
              && path.rsplit_once('.').is_some_and(|(_, ext)| {
                  !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric())
              })
      })
  }
  ```
- [ ] Add `pub fn chronicle_candidates(...)` per the Shared Interfaces signature. Body:
  - helper closure to build a candidate: `class`, `scope: Some(scope.clone())`, `statement: cap_chars(&raw, SUMMARY_BREADCRUMB_MAX)`, `structured_value: None`, `provenance: vec![EvidenceRef::Artifact { artifact: chronicle_ref.clone(), source_path: None }]`, `confidence: OBSERVED_CONFIDENCE`, `observed_at`, `valid_from: valid_from.clone()`, `sensitivity`, `retention: None`.
  - `investigations`: `chronicle.get("investigations").and_then(Value::as_array)`; for each string line, take up to `MAX_CHRONICLE_FINDINGS`: if `code_ref(line)` is `Some(r)` → `Code`, statement `format!("{r} — {}", line)`; else if it starts (case-insensitively, trimmed) with a `DECISION_MARKERS` entry → `Semantic` (decision text); else if it has ≥ 4 whitespace words → `Semantic` (prose finding); else skip.
  - `changes`: `chronicle.get("changes").and_then(Value::as_array)`, up to `MAX_CHRONICLE_CHANGES`: read `changeset_id` (str) and `byte_length` (u64, optional) defensively; if `changeset_id` present → `Episodic`, statement `format!("Applied changeset {changeset_id} ({byte_length} bytes) in run {run_id}")` (omit the bytes clause if absent).
  - `actions`: `chronicle.get("actions").and_then(Value::as_array)`, up to `MAX_CHRONICLE_FAILURES`: read `tool` (str) + `outcome` (str); if `outcome` ∈ {`"failed"`,`"denied"`,`"rejected"`} → `Failure`, statement `format!("{tool} {outcome} in run {run_id}")`.
- [ ] Re-export from `crates/knowledge/src/lib.rs`: add `chronicle_candidates` to the observer re-exports (match how `extract_candidates` is exported).
- [ ] **Harvest wiring** (`executor.rs harvest_memories`, before the re-anchor loop). After `let mut candidates = extract_candidates(&events, Scope::Session(session_id));` add a best-effort block:
  ```rust
  // Heuristic chronicle facts (M1). Locate the RunCompleted event, load its
  // chronicle artifact, parse it, and append discrete candidates. Every step is
  // best-effort: a miss/parse failure is warned and skipped, never fatal.
  if let Some((chronicle_ref, seq, at)) = events.iter().rev().find_map(|e| match &e.body {
      EventBody::RunCompleted { chronicle, .. } => Some((chronicle.clone(), e.sequence, e.occurred_at)),
      _ => None,
  }) {
      match self.load_chronicle(&chronicle_ref).await {
          Ok(chronicle) => candidates.extend(codypendent_knowledge::chronicle_candidates(
              &chronicle,
              &Scope::Session(session_id),
              &chronicle_ref,
              run_id,
              at,
              codypendent_knowledge::Revision::sequence(seq),
              chronicle_ref.sensitivity,
          )),
          Err(error) => warn!(%session_id, %run_id, %error, "could not load run chronicle for memory harvest"),
      }
  }
  ```
  The existing `for candidate in &mut candidates { candidate.scope = Some(repository_scope.clone()); }` loop then re-anchors these too.
- [ ] Add a small best-effort loader method on `RuntimeExecutor`:
  ```rust
  /// Read + JSON-parse the bytes behind a chronicle `ArtifactRef` (best-effort).
  async fn load_chronicle(&self, chronicle: &ArtifactRef) -> anyhow::Result<serde_json::Value> {
      use tokio::io::AsyncReadExt;
      let mut file = self.artifacts().open(&self.pool, chronicle.id).await?;
      let mut buf = Vec::new();
      file.read_to_end(&mut buf).await?;
      Ok(serde_json::from_slice(&buf)?)
  }
  ```
  Ensure `codypendent_knowledge::Revision` and `chronicle_candidates` are imported (extend the existing `use codypendent_knowledge::{…}` line); `ArtifactRef` is already available via `codypendent_protocol`.
- [ ] Add a harvest test (`executor.rs` tests, over `test_pool`): seed a session ledger with a `RunCompleted` event whose chronicle artifact (stored via the test `ArtifactStore`) has `investigations`/`changes`/`actions`; run harvest; assert the curated memories include the expected `Code`/`Semantic`/`Episodic`/`Failure` statements. (This test also anchors Task 5's fan-in.)
- [ ] `cargo test -p codypendent-knowledge` + `cargo test -p codypendentd harvest` green.

---

## Task 3 — M2: `memory.remember` core tool

**Files:** new `crates/runtime/src/tools/memory.rs`; `crates/runtime/src/tools/mod.rs`; `crates/protocol/src/run.rs` (+`ProposedAction::RecordMemory`); `crates/daemon/src/policy/mod.rs` (Allow arm); `crates/runtime/src/agent.rs` (registration + dispatch + emit); plus the observer `\u{1e}` extension in `crates/knowledge/src/observer.rs`.

- [ ] **Test first — observer extension** (`observer.rs` tests):
  - `explicit_proposal_plain_note_unchanged`: `NoteAppended { text: "memory.propose: use ripgrep" }` → one `Semantic` candidate, `statement == "use ripgrep"`, `structured_value == None` (regression guard).
  - `explicit_proposal_value_tail_populates_structured_value`: text `"memory.propose: db is postgres\u{1e}{\"engine\":\"postgres\"}"` → `statement == "db is postgres"`, `structured_value == Some(json!({"engine":"postgres"}))`.
  - `explicit_proposal_bad_json_tail_falls_back_to_statement`: `"memory.propose: x\u{1e}not json"` → `structured_value == None`, statement is the whole remainder (no panic).
  - legacy `memory:` marker still works.
- [ ] Extend `explicit_proposal_candidates`: after locating the marker, take `let rest = &trimmed[marker.len()..];`. If `rest` contains `'\u{1e}'`, split once: `let (stmt, tail) = rest.split_once('\u{1e}').unwrap();`, `statement = stmt.trim()`, and `structured_value = serde_json::from_str::<serde_json::Value>(tail.trim()).ok();`. Else `statement = rest.trim()`, `structured_value = None`. Keep the empty-statement skip. Set `structured_value` on the pushed `CandidateMemory`.
- [ ] **Test first — tool** (`tools/memory.rs` tests): `parse_memory_remember` requires a non-empty `statement`; `value` optional; `note_text` emits `"memory.propose: <s>"` without value and `"memory.propose: <s>\u{1e}<json>"` with value (assert the compact JSON round-trips via the observer split).
- [ ] Write `crates/runtime/src/tools/memory.rs` per the Shared Interfaces API. `proposed_action()` returns `ProposedAction::RecordMemory`. `note_text` uses `format!("memory.propose: {}", statement)` / `format!("memory.propose: {}{}{}", statement, Self::RECORD_SEPARATOR, value)` (`Display` on `serde_json::Value` is compact JSON).
- [ ] `tools/mod.rs`: add `mod memory;` and `pub use memory::{parse_memory_remember, MemoryRemember, MemoryRememberInput};`.
- [ ] **Protocol:** in `crates/protocol/src/run.rs`, add to `ProposedAction` (before `#[serde(other)] Unknown`):
  ```rust
  /// Record a memory proposal note (the `memory.remember` core tool). Appends a
  /// `NoteAppended` to the run's own ledger — no filesystem/command/network/remote.
  /// Always policy-Allowed; never serialized into a `ToolProposed` (never gated).
  RecordMemory,
  ```
- [ ] **Policy:** in `crates/daemon/src/policy/mod.rs evaluate`, add an arm joined with the blackboard Allow (or a dedicated `eval_record_memory`): `ProposedAction::RecordMemory => self.eval_blackboard(),` — reuse the existing no-capability Allow decision (or clone it with reason code `policy.record-memory-allowed`). Add a policy unit test asserting `RecordMemory` → `Decision::Allow`.
- [ ] **agent.rs registration:**
  - `offered_tool_names` baseline: add `MemoryRemember::NAME` to the FIRST `vec![Shell::NAME, ReadFile::NAME, Search::NAME, GitDiff::NAME, ApplyPatch::NAME]` (unconditional — NOT gated by github/blackboard).
  - `tool_definitions`: add a `decl(MemoryRemember::NAME, "Save a durable fact, decision, or learning to long-term memory in your own words. Use for a discrete fact worth recalling in future runs — not a summary of what you just did. One fact per call.", json!({"type":"object","properties":{"statement":{"type":"string"},"value":{}},"required":["statement"]}))` entry.
  - `PreparedTool` enum: add `MemoryRemember(MemoryRememberInput)`.
  - `prepare`: add arm `MemoryRemember::NAME => { let input = parse_memory_remember(args)?; Ok(Prepared { action: MemoryRemember::proposed_action(), tool: PreparedTool::MemoryRemember(input) }) }` (unconditional — no run gate).
  - Thread the run actor into execution: change `execute_prepared(&self, prepared: Prepared, run: &RunContext)` → `execute_prepared(&self, prepared: Prepared, run: &RunContext, run_actor: &Actor)` and update its single call site in `run_tool` (`let (observation, artifact, outcome) = self.execute_prepared(prepared, run, run_actor).await;`). Add the dispatch arm `PreparedTool::MemoryRemember(input) => self.execute_memory_remember(input, run, run_actor).await`.
  - Add:
    ```rust
    /// Record the model's memory proposal as a `NoteAppended` on the run's ledger.
    /// Harvest's `explicit_proposal_candidates` later turns it into a Semantic
    /// candidate — no new harvest wiring. The entire side effect is the note.
    async fn execute_memory_remember(
        &self,
        input: MemoryRememberInput,
        run: &RunContext,
        run_actor: &Actor,
    ) -> (String, Option<ArtifactRef>, ToolOutcome) {
        let text = MemoryRemember::note_text(&input);
        match self.emit(run.session_id, run_actor.clone(), EventBody::NoteAppended { text, run_id: Some(run.run_id) }).await {
            Ok(_) => (format!("noted for memory: {}", input.statement), None, ToolOutcome::Succeeded),
            Err(error) => (
                format!("could not record memory: {error}"),
                None,
                ToolOutcome::Failed { message: "memory.emit-failed".to_string() },
            ),
        }
    }
    ```
    Add `MemoryRemember, MemoryRememberInput, parse_memory_remember` to the `crate::tools::{…}` import.
- [ ] **Registry builtin (optional parity):** in `crates/knowledge/src/builtin.rs builtin_tools()`, add a `tool("memory.remember", "Save a durable fact, decision, or learning to long-term memory in your own words — one discrete fact per call, not a run summary.", &["remember this fact","save a decision to memory","note a learning for future runs"], &["memory","remember","fact","decision","learning"], Vec::new())` entry (System-scoped, FirstParty, Safe — no capability).
- [ ] **Runtime tests** (`agent.rs` tests):
  - `advertised_tools_includes_memory_remember_for_a_solo_run`: extend/mirror `advertised_tools_excludes_workflow_and_github_tools_for_a_solo_run` — assert `names.contains(&MemoryRemember::NAME)` for a plain (no-github, non-workflow) run (this is the additive "catalog +1" assertion; no snapshot file exists).
  - a `prepare`/`execute_prepared` round-trip test: prepare `memory.remember` with `{"statement":"x"}` → `Decision::Allow`; execute emits a `NoteAppended` whose text starts `"memory.propose: x"`.
- [ ] `cargo test -p codypendent-protocol -p codypendent-daemon -p codypendent-knowledge -p codypendent-runtime` green.

---

## Task 4 — M3a: `FactExtractor` trait + `NoopExtractor` + harvest injection

**Files:** new `crates/knowledge/src/extractor.rs`; `crates/knowledge/src/lib.rs` (module + re-exports); `crates/codypendentd/src/executor.rs` (`harvest_with` core + `build_fact_extractor` returning Noop + thread `mode`). Zero model deps.

- [ ] **Test first** (`extractor.rs` tests): `NoopExtractor::extract` returns `vec![]` for any input (build a minimal `ExtractionInput`).
- [ ] Write `crates/knowledge/src/extractor.rs` per the Shared Interfaces: the `FactExtractor` trait (`#[async_trait::async_trait]`), `ExtractionInput<'a>`, `NoopExtractor` + its impl returning `Vec::new()`. Import `CandidateMemory` from `crate::memory`, `Scope`/`Revision` from `crate::types`, and `ArtifactRef`/`RunId`/`DataClassification` from `codypendent_protocol`.
- [ ] `lib.rs`: add `mod extractor;` and `pub use extractor::{ExtractionInput, FactExtractor, NoopExtractor};`.
- [ ] **Harvest refactor** (`executor.rs`):
  - Split `harvest_memories` into the public entry (builds the extractor, delegates) and `harvest_with(&self, session_id, run_id, repository, extractor: &dyn FactExtractor)` holding the load-events → append-heuristics → append-extractor → re-anchor → curate loop.
  - In `harvest_with`, after the M1 chronicle block and BEFORE the re-anchor loop, when a chronicle was loaded, build an `ExtractionInput` (objective from `chronicle["objective"].as_str().unwrap_or("")`, `transcript_excerpt` from a cheap join of the ledger's note/tool-observation texts — see Task 5's `tail_cap`; in M3a pass the join un-capped, the extractor caps) and `candidates.extend(extractor.extract(input).await);`.
  - Add `async fn build_fact_extractor(&self, _mode: AgentMode) -> Box<dyn FactExtractor> { Box::new(codypendent_knowledge::NoopExtractor) }` (M3b replaces the body).
  - Change `harvest_memories(&self, session_id, run_id, repository)` → add `mode: AgentMode`; body: `let extractor = self.build_fact_extractor(mode).await; self.harvest_with(session_id, run_id, repository, extractor.as_ref()).await;`.
  - Update the `spawn_run` call site: capture `let mode = launch.mode;` BEFORE `launch` is moved into the worker (`AgentMode` is `Copy`), and call `.harvest_memories(session_id, run_id, repository, mode)`.
  - Import `FactExtractor`, `NoopExtractor`, `ExtractionInput` (extend the `use codypendent_knowledge::{…}`); `AgentMode` from `codypendent_protocol` (already imported for `execute`).
- [ ] **Fallback test** (`executor.rs` tests): a harvest over a seeded ledger + chronicle with a mock extractor returning `vec![]` still curates the heuristic + `memory.propose:` candidates (asserts M1/M2 survive an empty M3). Define a test `struct MockExtractor(Vec<CandidateMemory>)` implementing `FactExtractor` (returns a clone of its Vec) — call `harvest_with(..., &MockExtractor(vec![]))`.
- [ ] `cargo test -p codypendent-knowledge -p codypendentd` green.

---

## Task 5 — M3b: `LlmFactExtractor` + D2 selection/bounds/config + wiring

**Files:** new `crates/runtime/src/extractor.rs` (feature `provider-openai`); `crates/runtime/src/lib.rs`; `crates/runtime/Cargo.toml` (add `codypendent-knowledge` workspace dep); `crates/codypendentd/src/routing.rs` (config key); `crates/codypendentd/src/executor.rs` (`build_fact_extractor` real body). Depends on Task 4.

- [ ] **Cargo:** add `codypendent-knowledge = { workspace = true }` to `crates/runtime/Cargo.toml` `[dependencies]` (internal edge; no external/cargo-deny dep). Confirm `cargo tree` shows no cycle and `cargo deny check` stays clean.
- [ ] **Test first — bounds/mapping helpers** (`extractor.rs` tests, pure, no live model):
  - `tail_cap(s, max)` keeps the LAST `max` chars (char-boundary-safe) and returns the whole string when under `max`.
  - `parse_facts(json_text)`: given a JSON array of `{kind,statement,evidence,confidence}`, maps kinds → `MemoryClass` per the table (`finding`/`fact`/`decision`/unknown → `Semantic`, `learning`/`procedure` → `Procedural`, `failure`/`pitfall` → `Failure`, `preference` → `Preference`), clamps `confidence` to `[0,1]` (absent → `0.6`), truncates statements to `LLM_EXTRACT_STATEMENT_MAX`, DROPS empty statements, and caps the output at `LLM_EXTRACT_MAX_FACTS`. Non-JSON / non-array / missing fields → `vec![]`, never panic.
  - a `parse_facts` → `CandidateMemory` builder test: provenance is the chronicle `EvidenceRef::Artifact`, scope/observed_at/valid_from/sensitivity inherited from the `ExtractionInput`, `retention: None`.
- [ ] Write `crates/runtime/src/extractor.rs` behind `#[cfg(feature = "provider-openai")]`:
  - bounds consts (Shared Interfaces).
  - `pure` helpers `tail_cap`, `parse_facts(text, &ExtractionInput) -> Vec<CandidateMemory>`, `kind_to_class(&str) -> MemoryClass`.
  - `LlmFactExtractor { client: Arc<dyn ChatClient>, model_id: ModelId }`, `new` / `from_registry` (mirror `FrameworkModelDriver::from_registry`).
  - `impl FactExtractor for LlmFactExtractor` with `extract` wrapping an inner future in `tokio::time::timeout(LLM_EXTRACT_TIMEOUT, …)`; ANY `Err`/elapsed/empty ⇒ `tracing::warn!` + `Vec::new()`. Inner: build `Vec<Message>` (a fixed system prompt: "Extract at most 10 discrete, standalone facts as a JSON array of {kind, statement, evidence, confidence}. kind ∈ finding|decision|learning|failure|preference. No prose outside the array."; a user message with the objective, the compact chronicle, and `tail_cap(input.transcript_excerpt, LLM_EXTRACT_INPUT_MAX)`), call `self.client.get_streaming_response(messages, ChatOptions::new()).await?`, collect `update.text_content()` across the stream into a `String` (mirror `next_step`'s stream drain), then `parse_facts(&text, &input)`.
- [ ] `crates/runtime/src/lib.rs`: `#[cfg(feature = "provider-openai")] pub use extractor::LlmFactExtractor;` and `pub mod extractor;` (or `mod extractor;` with the gated re-export).
- [ ] **Config key** (`crates/codypendentd/src/routing.rs`): add `pub memory_extraction_model: Option<ModelId>` to `RoutingConfig` (default `None` in `Default`), a matching `#[serde(default)] memory_extraction_model: Option<ModelId>` on `RoutingConfigFile`, and set it in `load`'s `Ok(file) => Self { …, memory_extraction_model: file.memory_extraction_model }`. Add a load test: a `routing.toml` with `memory_extraction_model = "cheap-local"` parses to `Some(ModelId("cheap-local"))`; absent → `None`.
- [ ] **`build_fact_extractor` real body** (`executor.rs`), gated so a non-`provider-openai` build stays Noop:
  ```rust
  async fn build_fact_extractor(&self, mode: AgentMode) -> Box<dyn FactExtractor> {
      #[cfg(feature = "provider-openai")]
      {
          let (registry, policy) = match self.load_registry() {
              Ok(rp) => rp,
              Err(_) => return Box::new(codypendent_knowledge::NoopExtractor), // no model ⇒ Noop
          };
          // D2 selection: (1) configured extraction model, (2) run's resolved model, (3) Noop.
          let configured = RoutingConfig::load(&self.paths).memory_extraction_model;
          let model_id = match configured.filter(|id| registry.get(id).is_some()) {
              Some(id) => id,
              None => match resolve_model(&registry, &policy, mode).await {
                  Ok(resolved) => resolved.id,
                  Err(_) => return Box::new(codypendent_knowledge::NoopExtractor),
              },
          };
          // D2 config visibility: warn ONCE per process that extraction makes a
          // per-run model call, so an operator points `memory_extraction_model` at a cheap model.
          static NOTE: std::sync::Once = std::sync::Once::new();
          NOTE.call_once(|| tracing::info!(
              "memory extraction makes a best-effort per-run model call; set `memory_extraction_model` in routing.toml to a cheap/local model to keep cost off the coding model"
          ));
          match codypendent_runtime::LlmFactExtractor::from_registry(&registry, model_id).await {
              Ok(extractor) => Box::new(extractor),
              Err(error) => {
                  tracing::warn!(%error, "could not build memory extraction client; extraction disabled for this run");
                  Box::new(codypendent_knowledge::NoopExtractor)
              }
          }
      }
      #[cfg(not(feature = "provider-openai"))]
      { let _ = mode; Box::new(codypendent_knowledge::NoopExtractor) }
  }
  ```
  Ensure `resolve_model` (already imported) and `RoutingConfig` (already imported) are in scope.
- [ ] **Harvest tests** (`executor.rs`, with the `MockExtractor` from Task 4):
  - a mock returning two facts ⇒ two additional curated candidates (distinct statements ⇒ two more stored records).
  - the fallback assertion (mock `vec![]`) is already Task 4's test — keep it.
  - bounds are covered by the pure `tail_cap` / `parse_facts` tests above (no live model needed).
- [ ] `cargo test -p codypendent-runtime -p codypendentd` green; `cargo build -p codypendent-runtime --no-default-features` (Noop path) compiles.

---

## Task 6 — Hygiene + cross-mechanism integration

**Files:** tests only (+ any confirm-only checks). No new production code beyond assertions.

- [ ] **Advertised-catalog check:** confirm NO golden/snapshot file pins `tool_definitions` (only the `agent.rs advertised_tools_*` unit tests) — the Task 3 assertion covers the +1. Run `git grep -n "\.snap\|golden" crates/protocol/tests` to confirm `memory.remember` need not be added to `golden_vectors.rs`, and that `ProposedAction::RecordMemory` (always-Allow, never on the wire) needs no golden vector.
- [ ] **No wire/schema/migration change:** confirm `crates/protocol/src/events.rs` `EventBody` is unchanged (`NoteAppended`/`ToolStarted`/`ToolCompleted` already existed) and `migrations/` is untouched (`statement`/`structured_value_json`/`provenance_json`/`retention_json` already exist). `cargo test -p codypendent-protocol` (golden vectors) green.
- [ ] **Cross-mechanism dedup test** (`memory.rs` or `executor.rs` tests): a heuristic `Semantic` candidate and an identical LLM `Semantic` candidate (same statement, same `Repository` scope) collapse to ONE stored record via `curate` (`Curation::Duplicate` on the second) — extends an existing dedup test surface.
- [ ] **Integration test** (`executor.rs` tests, over `test_pool`): seed a session with (a) a `RunCompleted` event + chronicle artifact carrying a `Code`-worthy `investigations` line, (b) a `NoteAppended { "memory.propose: prefer sqlx over diesel" }`, and run `harvest_with(..., &MockExtractor(vec![<one distinct Semantic fact>]))`. Assert: three distinct curated statements land as separate `MemoryRecord`s (Code from chronicle, Semantic from the note, Semantic from the extractor), each retrievable as its OWN one-line statement via `assemble_context(pool, repository, "…", &[Scope::Repository(id)])` → `manifest.memories` (each a distinct `- <statement>` line, ≤ `MAX_CONTEXT_MEMORIES`). Confirms the fan-in + curate + one-line-per-fact contract end-to-end.
- [ ] **cargo deny** clean (no new external dep — only the internal `runtime → knowledge` edge). Full `cargo test` workspace green; `cargo clippy --all-targets` clean on Linux posture (no dead code; gate any provider-only helper behind `#[cfg(feature = "provider-openai")]`).

---

## Self-review (writing-plans)

- **Spec + decisions coverage:** M0 (D1 breadcrumb, 200-char, 30-day) ✓ Task 1; M1 heuristic chronicle mapping (findings→Code/Semantic, decisions→Semantic, changes→Episodic, failures→Failure) + per-class caps + best-effort harvest load ✓ Task 2; M2 tool + `\u{1e}` value seam (D3) + CORE baseline registration ✓ Task 3; M3a trait/Noop returning `Vec` + injection ✓ Task 4; M3b `LlmFactExtractor` + D2 selection order + bounds (32k/10/200/30s) + total fallback + config key + one-time note ✓ Task 5; hygiene + cross-mechanism dedup + integration ✓ Task 6.
- **Placeholder scan:** no `TODO`/`...`/`unimplemented!` in the plan's code; every signature is concrete and grounded in the real files read.
- **`FactExtractor` trait consistency (M3a ⇄ M3b):** identical `async fn extract(&self, input: ExtractionInput<'_>) -> Vec<CandidateMemory>` in the Shared Interfaces and both tasks; `NoopExtractor` and `LlmFactExtractor` implement exactly that. `ExtractionInput` carries `run_id` so extractor statements can match the chronicle/breadcrumb "in run <id>" phrasing.
- **Note format consistency (M2 ⇄ observer):** `memory.propose: <statement>[\u{1e}<json>]` defined once in Shared Interfaces; `MemoryRemember::note_text` produces it and `explicit_proposal_candidates` splits it on the first `\u{1e}` — round-tripped by a shared test.
- **Signature reconciliation with reality:** `chronicle_candidates` gained a `run_id` param (chronicle JSON has no run id, but statements need it); `execute_prepared` gained `run_actor` (it must emit the note with the run's actor); `harvest_memories` gained `mode` (D2 needs the run's model); `code_ref` is a regex-free scanner (this crate forbids `regex`); the change-statement reads `changeset_id`/`byte_length` (the real `changes[]` shape — NOT the spec's aspirational path/add/del).
