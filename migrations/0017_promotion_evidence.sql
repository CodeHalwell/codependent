-- Server-consumed evidence for promotion gates. Callers no longer submit a
-- bare pass/fail boolean: the regression verdict is derived from a persisted
-- SuiteReport, and canary regression is derived from recorded metrics.

CREATE TABLE eval_suite_reports (
    id TEXT PRIMARY KEY,
    candidate_id TEXT NOT NULL REFERENCES promotion_candidates(id) ON DELETE CASCADE,
    artifact_kind TEXT NOT NULL,
    artifact_name TEXT NOT NULL,
    artifact_version INTEGER NOT NULL,
    suite TEXT NOT NULL,
    routing_policy TEXT NOT NULL,
    report_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX ix_eval_suite_reports_candidate_created
    ON eval_suite_reports (candidate_id, created_at DESC);

CREATE TABLE promotion_regression_evidence (
    candidate_id TEXT PRIMARY KEY REFERENCES promotion_candidates(id) ON DELETE CASCADE,
    report_id TEXT NOT NULL REFERENCES eval_suite_reports(id),
    regressed INTEGER NOT NULL CHECK (regressed IN (0, 1)),
    failures_json TEXT NOT NULL,
    evaluated_at TEXT NOT NULL
);

CREATE TABLE promotion_canary_evidence (
    id TEXT PRIMARY KEY,
    candidate_id TEXT NOT NULL REFERENCES promotion_candidates(id) ON DELETE CASCADE,
    metrics_json TEXT NOT NULL,
    sample_count INTEGER NOT NULL CHECK (sample_count > 0),
    regressed INTEGER NOT NULL CHECK (regressed IN (0, 1)),
    observed_at TEXT NOT NULL
);

CREATE INDEX ix_promotion_canary_evidence_candidate
    ON promotion_canary_evidence (candidate_id, observed_at);
