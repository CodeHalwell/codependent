-- Outcome 11 (live measured routing): raw per-task-class outcomes from REAL
-- runs, so `codypendent_routing::ModelPerformance.task_class_success`
-- (`crates/routing/src/profile.rs`) stops being permanently empty.
--
-- Before this migration, `task_class_success` had exactly one non-test
-- constructor in the workspace (`BenchOutcome::into_profile`,
-- `crates/runtime/src/bench.rs`), which always wrote `Default::default()` (an
-- empty map) — a one-shot local bench measures timing and scripted probes,
-- never a task-class-specific outcome. `ModelPerformance::predicted_success`
-- therefore always fell back to the single overall `reliability` number (the
-- pass rate of one "declare an immutable Rust binding" prompt asked ten
-- times), so `crates/routing/src/classify.rs`'s nine-class rule-based
-- classifier — fully built, unit-tested, and threaded through
-- `TaskNode.classification` all the way to a rendered trace note — never
-- actually changed which model routing picked. Every task class scored
-- identically for every model. The 2026-08-13 review
-- (`2026-08-13-verticals/sandbox-eval-routing.md`, 11.1/11.3) names this the
-- headline "data produced, never consumed" defect of outcome 11.
--
-- This table is the missing writer's storage: one row per observed (model,
-- endpoint, task_class) outcome from an ACTUAL run — not a bench probe.
-- `crates/daemon/src/model_profiles.rs::ModelProfileStore::record_outcome` is
-- the sole writer; it inserts a row here AND, in the same transaction,
-- recomputes that (model, endpoint, task_class)'s aggregate success rate from
-- every row on file and folds it into the existing `model_profiles.profile_json`
-- row's `performance.task_class_success` map — so a fresh `ModelProfileStore::get`
-- reflects it with no separate read-time join. Kept as durable raw history
-- (rather than only updating a running rate in place) so the aggregate can be
-- recomputed exactly, inspected, or re-weighted later without having thrown
-- the underlying observations away — the same append-only discipline
-- `learning_records` (0024) and the ledger tables use.
--
-- `task_class` stores `TaskClass::as_str()` (`crates/routing/src/classify.rs`)
-- verbatim — the exact string key `task_class_success` is keyed by — so a
-- reader never needs a lookup table to join the two. Not a foreign key to
-- `model_profiles`: `record_outcome` requires the profile row to already
-- exist (mirrors `cache_capabilities`'s precedent, migration 0014) and
-- enforces it at the application layer, so this table's own schema stays
-- simple and this migration adds no coupling to 0014's column set.
--
-- Append-only (migrations never edit an existing file): a fresh DB creates
-- the table; an existing DB gains it empty. Nothing reads or writes it until
-- a caller invokes `record_outcome`, so this changes no existing behavior by
-- itself.
CREATE TABLE model_task_outcomes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model_id TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    -- `TaskClass::as_str()`, e.g. 'small-bug-fix', 'doc-update', 'general'.
    task_class TEXT NOT NULL,
    -- 1 = the run succeeded at this task class, 0 = it did not.
    success INTEGER NOT NULL CHECK (success IN (0, 1)),
    -- The run this observation came from — provenance, and de-duplication
    -- (`record_outcome` is called once per terminal run; a retried call for
    -- the SAME run_id must not double-count).
    run_id TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);

-- The lookup `record_outcome`'s aggregation query and a future "why this
-- rate" inspection both need: every observation for one (model, endpoint,
-- task_class).
CREATE INDEX idx_model_task_outcomes_lookup
    ON model_task_outcomes(model_id, endpoint, task_class);

-- De-duplicates a retried `record_outcome` call for the same run — a
-- crashed-and-retried write must not count the same run twice toward the
-- aggregate.
CREATE UNIQUE INDEX idx_model_task_outcomes_run
    ON model_task_outcomes(model_id, endpoint, task_class, run_id);
