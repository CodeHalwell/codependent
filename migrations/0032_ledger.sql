-- Outcome 20: persist a completed run's MEASURED usage where a reader can
-- find it with a plain SELECT, instead of only inside the one-shot
-- content-addressed chronicle artifact that no wire command can fetch back.
--
-- `RunOutcome.usage` (the run's aggregated measured `ModelUsage` — prompt
-- tokens, completion tokens, and cost when a price was known) was computed
-- honestly by the agent loop and then discarded by its caller
-- (`codypendentd/src/executor.rs`'s `.execute_run(...).await.map(|_| ())`), so
-- despite the chronicle showing a real token count, `runs` carried nothing a
-- report or a support query could read.
--
-- All three columns are nullable and mean exactly what `ModelUsage` already
-- means: NULL is "not measured" — no request in the run reported usage, or
-- (for `cost_micros` specifically) tokens were measured but no per-token
-- price was known to convert them — never a fabricated zero a budget or a
-- report could mistake for a genuinely free/silent run. A row with
-- `prompt_tokens` set always has `completion_tokens` set too (`ModelUsage` is
-- measured as a whole, never per-field); `cost_micros` commonly stays NULL
-- while both token columns are populated — the live driver measures tokens
-- but has no price of its own; see `ModelUsage`'s "tokens and cost are
-- decoupled" doc comment in `crates/runtime/src/agent.rs`, which this mirrors.
--
-- Nullable and appended, like `started_at`/`ended_at` before them (0002): a
-- run row written before this migration reads all three as NULL, which is
-- indistinguishable from — and therefore safely defaults to — "not measured".
ALTER TABLE runs ADD COLUMN prompt_tokens INTEGER;
ALTER TABLE runs ADD COLUMN completion_tokens INTEGER;
ALTER TABLE runs ADD COLUMN cost_micros INTEGER;
