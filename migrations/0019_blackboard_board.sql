-- Phase B (kanban on the blackboard, rubric 10): extend `blackboard_items` for
-- board use — a column/status, an assignee, and a within-column ordinal — plus
-- the board scope marker for repository task boards.
--
-- Scope design. `blackboard_items.workflow_run_id` is `NOT NULL REFERENCES
-- workflow_runs(id)` (0010) and migrations are append-only, so making the FK
-- nullable would need a full table rebuild. The cleaner shape kept here: a
-- repository task board is a **synthetic workflow run** whose id is
-- `board:<canonical repo path>` (see `codypendent_protocol::board_scope_id`),
-- inserted on first board write with the terminal state `completed` so startup
-- recovery and every drive path ignore it. Board cards then reference that run
-- like any other item, and the existing read command, subscription hub, and
-- supersession machinery serve the board unchanged. `board_scope` records the
-- repository the board serves (NULL for ordinary workflow artifacts) so board
-- items are distinguishable without parsing the run id.
ALTER TABLE blackboard_items ADD COLUMN board_scope TEXT;
-- The board column (todo | doing | review | done, or a validated free string).
-- NULL for non-card artifacts.
ALTER TABLE blackboard_items ADD COLUMN status TEXT;
-- Who the card is assigned to, if anyone.
ALTER TABLE blackboard_items ADD COLUMN assignee TEXT;
-- Position within the status column (lower sorts first). NULL for non-cards.
ALTER TABLE blackboard_items ADD COLUMN ordinal INTEGER;

-- Column-grouped board reads: live cards of one run/board, per status, in order.
CREATE INDEX ix_blackboard_items_board
    ON blackboard_items (workflow_run_id, status, ordinal);
