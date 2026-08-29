-- A control-plane event cursor is repository-scoped. A single cursor shared
-- by two repositories can advance past an event that was never queried from
-- the other repository, permanently dropping it. Rebuild the table with the
-- repository in the key and include the server's `sync` stream.

ALTER TABLE control_plane_sync_cursors RENAME TO control_plane_sync_cursors_legacy;

CREATE TABLE control_plane_sync_cursors (
    pairing_id TEXT NOT NULL REFERENCES control_plane_pairings(id),
    repository_id TEXT NOT NULL,
    stream TEXT NOT NULL CHECK (stream IN (
        'sync', 'notifications', 'approvals', 'schedules', 'runner-events', 'policy'
    )),
    cursor TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (pairing_id, repository_id, stream)
);

-- Legacy cursors were not repository-safe. Retain them under the empty scope
-- for diagnostics and backwards-compatible callers, but never apply them to a
-- newly scoped repository pull.
INSERT INTO control_plane_sync_cursors (
    pairing_id, repository_id, stream, cursor, updated_at
)
SELECT pairing_id, '', stream, cursor, updated_at
FROM control_plane_sync_cursors_legacy;

DROP TABLE control_plane_sync_cursors_legacy;
