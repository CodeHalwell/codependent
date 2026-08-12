-- Connection-level PutArtifact commands still obey the protocol-wide
-- idempotency contract. The journal row and its per-occurrence artifact row are
-- committed in one transaction, so a retry returns the original ArtifactRef
-- instead of minting another metadata occurrence.
CREATE TABLE artifact_upload_commands (
    idempotency_key TEXT PRIMARY KEY,
    client_id TEXT NOT NULL,
    command_id TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(id),
    created_at TEXT NOT NULL
);
