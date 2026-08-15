-- Adoption 05 (STEP 5.6): a session may be a fork of another at an
-- Adoption-04 checkpoint. All columns nullable: every pre-existing session
-- reads NULL = "not a fork". fork_base_commit / fork_checkpoint_sha /
-- fork_checkpoint_kind are denormalized from run_checkpoints at fork time so
-- worktree allocation for the fork's runs needs no join against a row the
-- source session owns.
ALTER TABLE sessions ADD COLUMN forked_from_session_id TEXT;
ALTER TABLE sessions ADD COLUMN forked_at_sequence INTEGER;
ALTER TABLE sessions ADD COLUMN fork_base_commit TEXT;
ALTER TABLE sessions ADD COLUMN fork_checkpoint_sha TEXT;
ALTER TABLE sessions ADD COLUMN fork_checkpoint_kind TEXT;
