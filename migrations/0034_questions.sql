-- Adoption 03: durable question parking (the `user.ask` tool). A question is an
-- approval card with options instead of allow/deny, and it parks the same way:
-- a pending row + a ledger event, resurfaced on restart, expired when its run
-- can never consume the answer.
CREATE TABLE questions (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    questions_json TEXT NOT NULL,          -- Vec<QuestionPrompt>
    state TEXT NOT NULL,                   -- pending | answered | rejected | expired
    answers_json TEXT,                     -- Vec<Vec<String>> when answered
    feedback TEXT,                         -- optional rejection feedback
    resolved_by TEXT,
    asked_at TEXT NOT NULL,
    resolved_at TEXT
);

CREATE INDEX idx_questions_pending ON questions(state, run_id);
