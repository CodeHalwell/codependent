-- Persisted dense vectors for registry items (rubric 9 — real embeddings).
--
-- One row per registry item, holding the vector a configured embedding model
-- produced for the item's retrieval text (name + description + intents).
-- `content_hash` is the SHA-256 of that text — not the package hash — so a
-- description edit invalidates the row; `model` + `dims` identify the embedding
-- space, so switching models (or a dims mismatch) invalidates it too. Rows are
-- refreshed by the index-outbox drain (the Chapter 06 indexer worker this table
-- finally gives a consumer) and loaded by context assembly instead of
-- re-embedding every item per call. Derived data: deletable at any time and
-- rebuilt from `registry_items` authority.
CREATE TABLE registry_embeddings (
    item_id TEXT PRIMARY KEY,         -- registry_items.id (no FK: the drain observes deletes via the outbox, after the authoritative row is gone)
    content_hash TEXT NOT NULL,       -- hex SHA-256 of the embedded text
    model TEXT NOT NULL,              -- provider-side embedding model name
    dims INTEGER NOT NULL,            -- vector dimensionality
    vector BLOB NOT NULL,             -- little-endian f32 values, dims * 4 bytes
    updated_at TEXT NOT NULL
);
