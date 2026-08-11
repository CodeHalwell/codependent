-- Idempotent daemon-owned Remote UI plugin lifecycle outcomes. Package bytes
-- live in the content-addressed plugin store; this table only remembers the
-- authenticated command identity/body digest and exact reply for safe retry.
CREATE TABLE ui_plugin_commands (
    idempotency_key TEXT PRIMARY KEY,
    client_id TEXT NOT NULL,
    body_hash TEXT NOT NULL,
    result_json TEXT,
    created_at TEXT NOT NULL
);
