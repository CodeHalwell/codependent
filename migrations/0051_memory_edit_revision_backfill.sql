-- Backfill pre-fix `edit:<uuid>` memory revisions into canonical `seq:` form.
--
-- Before v0.12.0, `MemoryStore::correct` stamped `valid_from = edit:<uuid>`
-- instead of claiming a sequence revision. Revisions are compared as TEXT
-- against `seq:<20 digits>` and 'e' < 's', so every correction sorted BEFORE
-- every real revision. `MemoryStore::query` refuses a non-sequence revision
-- outright (`MemoryError::NonOrderableRevision`), so affected facts are
-- currently unreadable rather than silently misordered.
--
-- REPAIR SCOPE IS DELIBERATELY NARROW. A token is rewritten only when no
-- orderable revision exists anywhere downstream of it. For those, appending
-- above the store's current maximum cannot invert an interval.
--
-- A token that DOES have an orderable revision downstream — a correction made
-- under <= v0.11, then corrected again under v0.12.0 — has no representable
-- slot. The sequence space is dense (`insert_row_claiming_revision` claims
-- MAX + 1), so there is no integer between such a token's floor and its
-- orderable ceiling. Those tokens are left exactly as they are, which leaves
-- their pre-existing mis-ranking in place: `MemoryError::NonOrderableRevision`
-- guards the `as_of` ARGUMENT, not stored rows, so an `edit:` revision in the
-- table mis-compares silently rather than failing closed. That is a known,
-- tested residual. Assigning them MAX + k instead would be strictly worse: it
-- pushes the intermediate row's `valid_from` ABOVE its own `valid_until`,
-- hiding that row at every revision and making two contradicting versions of
-- the same fact visible at once. One version visible but wrong beats two.
--
-- Idempotent: a second run finds no `edit:` tokens and rewrites nothing.

CREATE TEMP TABLE memory_edit_revision_backfill AS
WITH RECURSIVE
revisions(revision) AS (
    SELECT valid_from FROM memories
    UNION ALL
    SELECT valid_until FROM memories
),
base(sequence) AS (
    SELECT COALESCE(MAX(CAST(substr(revision, 5) AS INTEGER)), 0)
      FROM revisions
     WHERE revision IS NOT NULL
       AND length(revision) = 24
       AND substr(revision, 1, 4) = 'seq:'
       AND substr(revision, 5) NOT GLOB '*[^0-9]*'
),
edits(token) AS (
    SELECT DISTINCT revision
      FROM revisions
     WHERE revision IS NOT NULL
       AND length(revision) = 41
       AND substr(revision, 1, 5) = 'edit:'
),
-- Every revision reachable by following the supersession chain forward from a
-- token: the row it opens is closed by `valid_until`, which in turn opens the
-- next row. The depth guard bounds a cyclic store rather than trusting one.
downstream(token, revision, depth) AS (
    SELECT e.token, m.valid_until, 1
      FROM edits AS e
      JOIN memories AS m ON m.valid_from = e.token
     WHERE m.valid_until IS NOT NULL
    UNION ALL
    SELECT d.token, m.valid_until, d.depth + 1
      FROM downstream AS d
      JOIN memories AS m ON m.valid_from = d.revision
     WHERE m.valid_until IS NOT NULL
       AND d.depth < 10000
),
blocked(token) AS (
    SELECT DISTINCT token
      FROM downstream
     WHERE length(revision) = 24
       AND substr(revision, 1, 4) = 'seq:'
       AND substr(revision, 5) NOT GLOB '*[^0-9]*'
),
repairable(token) AS (
    SELECT token FROM edits
    EXCEPT
    SELECT token FROM blocked
)
SELECT
    entry.token AS token,
    printf(
        'seq:%020d',
        (SELECT sequence FROM base)
            + (SELECT COUNT(*) FROM repairable AS earlier WHERE earlier.token <= entry.token)
    ) AS replacement
FROM repairable AS entry;

UPDATE memories
   SET valid_from = (
        SELECT replacement FROM memory_edit_revision_backfill
         WHERE token = memories.valid_from
   )
 WHERE valid_from IN (SELECT token FROM memory_edit_revision_backfill);

UPDATE memories
   SET valid_until = (
        SELECT replacement FROM memory_edit_revision_backfill
         WHERE token = memories.valid_until
   )
 WHERE valid_until IN (SELECT token FROM memory_edit_revision_backfill);

DROP TABLE temp.memory_edit_revision_backfill;
