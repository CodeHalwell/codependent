-- Run summaries are content-addressed in the control-plane outbox. A run may
-- legally revisit a prior state (Running -> Paused -> Running), so the current
-- values alone are not a monotonic identity for an authoritative write.
ALTER TABLE runs
    ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 0
        CHECK (sync_revision >= 0);

-- SQLite's `IS NOT` comparison is NULL-safe. Increment exactly once whenever
-- any field represented by the run-summary payload changes; unrelated run
-- metadata such as workspace lease provenance does not mint a sync revision.
CREATE TRIGGER runs_increment_sync_revision
AFTER UPDATE OF state, started_at, ended_at, prompt_tokens, completion_tokens, cost_micros ON runs
WHEN OLD.state IS NOT NEW.state
  OR OLD.started_at IS NOT NEW.started_at
  OR OLD.ended_at IS NOT NEW.ended_at
  OR OLD.prompt_tokens IS NOT NEW.prompt_tokens
  OR OLD.completion_tokens IS NOT NEW.completion_tokens
  OR OLD.cost_micros IS NOT NEW.cost_micros
BEGIN
    UPDATE runs
    SET sync_revision = OLD.sync_revision + 1
    WHERE id = NEW.id;
END;
