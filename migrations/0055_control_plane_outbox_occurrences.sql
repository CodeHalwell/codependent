-- Publication policy can narrow, widen, and narrow again. Each sanitized
-- repair must have a fresh daemon sequence because an earlier sequence may
-- already have committed remotely before the local acknowledgement was lost.
-- Payload identity is therefore not a table-level uniqueness constraint;
-- ordinary authoritative reconciliation retains its explicit NOT EXISTS guard.

CREATE TABLE control_plane_outbox_occurrences (
    id TEXT PRIMARY KEY,
    pairing_id TEXT NOT NULL REFERENCES control_plane_pairings(id),
    delta_kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    class TEXT NOT NULL,
    payload_hash TEXT NOT NULL CHECK (length(payload_hash) = 64),
    sequence INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    acknowledged_at TEXT,
    remote_receipt TEXT,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    delivery_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (delivery_state IN ('pending', 'acknowledged', 'rejected')),
    rejected_at TEXT,
    rejection_code TEXT,
    rejection_reason TEXT,
    UNIQUE (pairing_id, sequence)
);

INSERT INTO control_plane_outbox_occurrences (
    id, pairing_id, delta_kind, subject_id, payload, class, payload_hash,
    sequence, created_at, acknowledged_at, remote_receipt, attempts, last_error,
    delivery_state, rejected_at, rejection_code, rejection_reason
)
SELECT
    id, pairing_id, delta_kind, subject_id, payload, class, payload_hash,
    sequence, created_at, acknowledged_at, remote_receipt, attempts, last_error,
    delivery_state, rejected_at, rejection_code, rejection_reason
FROM control_plane_outbox;

DROP TABLE control_plane_outbox;
ALTER TABLE control_plane_outbox_occurrences RENAME TO control_plane_outbox;

CREATE INDEX idx_control_plane_outbox_pending
    ON control_plane_outbox (pairing_id, sequence)
    WHERE delivery_state = 'pending';

CREATE INDEX idx_control_plane_outbox_rejected
    ON control_plane_outbox (pairing_id, sequence)
    WHERE delivery_state = 'rejected';
