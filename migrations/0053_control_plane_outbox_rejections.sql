-- A per-delta rejection is not the same as a failed delivery attempt. Deltas
-- the control plane can never accept must leave the pending queue while
-- retaining the exact bounded rejection evidence for operator inspection.
ALTER TABLE control_plane_outbox
    ADD COLUMN delivery_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (delivery_state IN ('pending', 'acknowledged', 'rejected'));

ALTER TABLE control_plane_outbox ADD COLUMN rejected_at TEXT;
ALTER TABLE control_plane_outbox ADD COLUMN rejection_code TEXT;
ALTER TABLE control_plane_outbox ADD COLUMN rejection_reason TEXT;

-- Preserve the terminal state of rows acknowledged before this migration.
UPDATE control_plane_outbox
SET delivery_state = 'acknowledged'
WHERE acknowledged_at IS NOT NULL;

DROP INDEX idx_control_plane_outbox_pending;

CREATE INDEX idx_control_plane_outbox_pending
    ON control_plane_outbox (pairing_id, sequence)
    WHERE delivery_state = 'pending';

CREATE INDEX idx_control_plane_outbox_rejected
    ON control_plane_outbox (pairing_id, sequence)
    WHERE delivery_state = 'rejected';
