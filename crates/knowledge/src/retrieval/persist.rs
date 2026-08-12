//! Persisted registry vectors + the index-outbox drain (rubric 9).
//!
//! Two jobs, both over the `registry_embeddings` table (migration 0022):
//!
//! 1. **Persistence** — store one dense vector per registry item, keyed by the
//!    SHA-256 of its retrieval text and stamped with the embedding model + dims,
//!    so [`semantic_indexes`] loads vectors instead of re-embedding every item on
//!    every `assemble_context`, and a description edit / model switch / dims
//!    mismatch is detected as staleness and re-embedded.
//! 2. **The drain** — the first real consumer of the Chapter 06 `index_outbox`:
//!    [`drain_outbox`] claims unprocessed rows, refreshes the vector rows for
//!    changed registry items (deleting rows for removed items), and stamps every
//!    row processed so the outbox stops growing without bound. Non-registry
//!    kinds pass through [`handle_non_registry_event`], the deliberate hook where
//!    document/memory/symbol indexing plugs in later.
//!
//! Everything here is derived data over `registry_items` authority: deletable at
//! any time, rebuilt by replaying the outbox (or by [`reconcile_embeddings`],
//! the full-scan form the daemon runs at startup so items written while no
//! embedding model was configured are picked up).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use codypendent_protocol::RegistryItemId;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::outbox::{self, OutboxRow};
use crate::registry::Registry;
use crate::types::RegistryItem;

use super::embed::{EmbedError, SemanticEmbedder};
use super::{embedding_text, HashingEmbedder, RetrievalIndexes, VectorIndex};

/// A failure loading/storing persisted vectors or draining the outbox.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// The registry read behind a drain step failed.
    #[error(transparent)]
    Registry(#[from] crate::registry::RegistryError),
    /// The embedding model call failed (the drain leaves the rows unprocessed
    /// and retries on the next pass).
    #[error(transparent)]
    Embed(#[from] EmbedError),
}

/// One persisted vector row, as loaded for staleness checks and index builds.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEmbedding {
    /// Hex SHA-256 of the text the vector was computed from.
    pub content_hash: String,
    /// The embedding model that produced it.
    pub model: String,
    /// The vector's dimensionality.
    pub dims: usize,
    /// The vector itself.
    pub vector: Vec<f32>,
}

/// The hex SHA-256 of an item's retrieval text — the persistence key that makes
/// a description/intent edit invalidate its stored vector.
#[must_use]
pub fn embedding_content_hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Encode a vector as little-endian f32 bytes (the `vector BLOB` column).
fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Decode a `vector BLOB` back into f32s. A ragged byte length (not a multiple
/// of 4) truncates to whole values — the dims check upstream treats such a row
/// as stale rather than erroring.
fn decode_vector(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Load every persisted vector, keyed by item id. The caller filters by
/// model/hash/dims per item (staleness is per-row, not per-table).
pub async fn load_embeddings(
    pool: &SqlitePool,
) -> Result<HashMap<RegistryItemId, StoredEmbedding>, PersistError> {
    let rows: Vec<(String, String, String, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT item_id, content_hash, model, dims, vector FROM registry_embeddings",
    )
    .fetch_all(pool)
    .await?;
    let mut out = HashMap::with_capacity(rows.len());
    for (item_id, content_hash, model, dims, vector) in rows {
        let Ok(id) = item_id.parse::<RegistryItemId>() else {
            continue; // an unparseable id is dead derived data; the drain replaces it
        };
        out.insert(
            id,
            StoredEmbedding {
                content_hash,
                model,
                dims: usize::try_from(dims).unwrap_or(0),
                vector: decode_vector(&vector),
            },
        );
    }
    Ok(out)
}

/// Insert or replace one item's persisted vector.
pub async fn upsert_embedding(
    pool: &SqlitePool,
    item_id: RegistryItemId,
    content_hash: &str,
    model: &str,
    vector: &[f32],
    now: DateTime<Utc>,
) -> Result<(), PersistError> {
    sqlx::query(
        "INSERT OR REPLACE INTO registry_embeddings \
         (item_id, content_hash, model, dims, vector, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(item_id.to_string())
    .bind(content_hash)
    .bind(model)
    .bind(vector.len() as i64)
    .bind(encode_vector(vector))
    .bind(now.to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// Drop one item's persisted vector (its authoritative row was removed).
pub async fn delete_embedding(
    pool: &SqlitePool,
    item_id: RegistryItemId,
) -> Result<(), PersistError> {
    sqlx::query("DELETE FROM registry_embeddings WHERE item_id = ?")
        .bind(item_id.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

/// Whether `stored` is fresh for (`model`, `content_hash`): same model, same
/// text hash, and the dims recorded actually match the vector's length (a model
/// change usually flips the hash comparison via `model`; the explicit dims check
/// additionally catches a provider that changed output width under one name).
fn is_fresh(stored: &StoredEmbedding, model: &str, content_hash: &str) -> bool {
    stored.model == model
        && stored.content_hash == content_hash
        && stored.dims == stored.vector.len()
}

/// What one maintenance pass did — logged by the daemon job, asserted by tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainReport {
    /// Outbox rows stamped processed this pass.
    pub processed: usize,
    /// Registry vectors recomputed and persisted.
    pub embedded: usize,
    /// Persisted vectors dropped because their item was removed.
    pub deleted: usize,
    /// Non-registry outbox rows passed to the (currently no-op) hook.
    pub deferred_kinds: usize,
}

/// The plug point for non-registry outbox kinds (`memory_changed`,
/// `symbol_changed`, `document_changed`, `artifact_created`). Document indexing
/// (and the rest) slots in here later; today the row is acknowledged so the
/// outbox drains instead of growing forever — the derived indexes for those
/// kinds are still rebuilt from authority on demand, so acknowledging loses
/// nothing.
fn handle_non_registry_event(row: &OutboxRow) {
    tracing::trace!(kind = %row.event_kind, entity = %row.entity_id, "no derived index consumes this outbox kind yet");
}

/// Drain up to `limit` unprocessed `index_outbox` rows.
///
/// For each `registry_item_changed` row: re-read the item from authority; if it
/// still exists and `embedder` is configured, refresh its persisted vector when
/// stale (model/hash/dims mismatch — the re-embed trigger); if it was removed,
/// drop the vector row. Other kinds go through [`handle_non_registry_event`].
/// Every handled row is stamped processed. An embedding failure aborts the pass
/// with rows left unprocessed, so the next pass retries — the outbox is the
/// at-least-once retry queue.
///
/// With no `embedder` the drain still consumes rows (there is nothing to
/// persist for the hashing model — it embeds inline at query time); a later
/// [`reconcile_embeddings`] pass backfills vectors once a model is configured.
pub async fn drain_outbox(
    pool: &SqlitePool,
    embedder: Option<&dyn SemanticEmbedder>,
    limit: i64,
) -> Result<DrainReport, PersistError> {
    let rows = outbox::unprocessed(pool, limit).await?;
    let mut report = DrainReport::default();
    let registry = Registry::new();
    for row in rows {
        if row.event_kind == "registry_item_changed" {
            if let Ok(item_id) = row.entity_id.parse::<RegistryItemId>() {
                match registry.get(pool, item_id).await? {
                    Some(item) => {
                        if let Some(embedder) = embedder {
                            report.embedded +=
                                refresh_item_embedding(pool, embedder, &item).await?;
                        }
                    }
                    None => {
                        delete_embedding(pool, item_id).await?;
                        report.deleted += 1;
                    }
                }
            }
        } else {
            handle_non_registry_event(&row);
            report.deferred_kinds += 1;
        }
        outbox::mark_processed(pool, &row.id, Utc::now()).await?;
        report.processed += 1;
    }
    Ok(report)
}

/// Refresh one item's persisted vector if stale; returns how many vectors were
/// written (0 or 1).
async fn refresh_item_embedding(
    pool: &SqlitePool,
    embedder: &dyn SemanticEmbedder,
    item: &RegistryItem,
) -> Result<usize, PersistError> {
    let text = embedding_text(item);
    let hash = embedding_content_hash(&text);
    let stored: Option<(String, String, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT content_hash, model, dims, vector FROM registry_embeddings WHERE item_id = ?",
    )
    .bind(item.id.to_string())
    .fetch_optional(pool)
    .await?;
    if let Some((content_hash, model, dims, vector)) = stored {
        let stored = StoredEmbedding {
            content_hash,
            model,
            dims: usize::try_from(dims).unwrap_or(0),
            vector: decode_vector(&vector),
        };
        if is_fresh(&stored, embedder.model(), &hash) {
            return Ok(0);
        }
    }
    let mut vectors = embedder.embed_batch(std::slice::from_ref(&text)).await?;
    let Some(vector) = vectors.pop().filter(|v| !v.is_empty()) else {
        return Err(PersistError::Embed(EmbedError(
            "embedding model returned no vector".to_string(),
        )));
    };
    upsert_embedding(pool, item.id, &hash, embedder.model(), &vector, Utc::now()).await?;
    Ok(1)
}

/// Full-scan reconcile: persist a fresh vector for every listed registry item
/// whose stored row is missing or stale for `embedder`'s model. Returns how
/// many vectors were written. The daemon runs this at startup (and after model
/// changes), so items written while embeddings were unconfigured — whose outbox
/// rows an earlier drain already consumed — still get vectors.
pub async fn reconcile_embeddings(
    pool: &SqlitePool,
    embedder: &dyn SemanticEmbedder,
) -> Result<usize, PersistError> {
    let items = Registry::new().list(pool).await?;
    let stored = load_embeddings(pool).await?;
    let mut written = 0;
    for item in &items {
        let text = embedding_text(item);
        let hash = embedding_content_hash(&text);
        let fresh = stored
            .get(&item.id)
            .is_some_and(|row| is_fresh(row, embedder.model(), &hash));
        if fresh {
            continue;
        }
        written += refresh_item_embedding(pool, embedder, item).await?;
    }
    Ok(written)
}

/// Build the derived retrieval indexes for `items`, plus the query vector for
/// `query_text`, preferring the configured semantic model.
///
/// - **No `semantic` model** (or any model failure): both indexes are built
///   exactly as before — the offline [`HashingEmbedder`] embeds every item and
///   the query inline — and the returned query vector is `None`, so
///   [`retrieve`](super::retrieve) embeds the query through the retained
///   hashing embedder. Behavior-identical to the pre-embedding code.
/// - **With a model**: item vectors come from `registry_embeddings` where fresh
///   (the drain keeps them so); items missing a fresh vector — plus the query —
///   are embedded in ONE batch call, and the freshly computed item vectors are
///   persisted so the next assembly is warm. The returned query vector is in
///   the same space as the index, and the caller passes it through
///   [`RetrievalQuery::query_vector`](super::RetrievalQuery::query_vector) so
///   the funnel never mixes embedding spaces.
///
/// Every model-path failure (endpoint down, dims mismatch, bad response)
/// degrades to the hashing pair with a warning — retrieval is an aid, never a
/// gate on running.
pub async fn semantic_indexes(
    pool: &SqlitePool,
    items: &[RegistryItem],
    semantic: Option<&dyn SemanticEmbedder>,
    query_text: &str,
) -> Result<(RetrievalIndexes, Option<Vec<f32>>), super::RetrievalError> {
    let Some(embedder) = semantic else {
        return Ok((
            RetrievalIndexes::build(items, HashingEmbedder::new())?,
            None,
        ));
    };
    match semantic_vectors(pool, items, embedder, query_text).await {
        Ok((vector_index, query_vector)) => {
            let indexes =
                RetrievalIndexes::from_parts(items, vector_index, HashingEmbedder::new())?;
            Ok((indexes, Some(query_vector)))
        }
        Err(error) => {
            tracing::warn!(model = embedder.model(), %error, "semantic embedding unavailable; falling back to the hashing embedder");
            Ok((
                RetrievalIndexes::build(items, HashingEmbedder::new())?,
                None,
            ))
        }
    }
}

/// The semantic half of [`semantic_indexes`]: assemble the item vector index
/// from persisted + freshly batched vectors, and embed the query in the same
/// call. Fails (for the caller to degrade on) rather than mixing spaces.
async fn semantic_vectors(
    pool: &SqlitePool,
    items: &[RegistryItem],
    embedder: &dyn SemanticEmbedder,
    query_text: &str,
) -> Result<(VectorIndex, Vec<f32>), PersistError> {
    let stored = load_embeddings(pool).await?;

    // Split items into (persisted-fresh) and (must-embed-now).
    let mut fresh: Vec<(RegistryItemId, Vec<f32>)> = Vec::new();
    let mut missing: Vec<(&RegistryItem, String, String)> = Vec::new(); // (item, text, hash)
    for item in items {
        let text = embedding_text(item);
        let hash = embedding_content_hash(&text);
        match stored.get(&item.id) {
            Some(row) if is_fresh(row, embedder.model(), &hash) => {
                fresh.push((item.id, row.vector.clone()));
            }
            _ => missing.push((item, text, hash)),
        }
    }

    // One batch call: every missing item's text, then the query, last.
    let mut batch: Vec<String> = missing.iter().map(|(_, text, _)| text.clone()).collect();
    batch.push(query_text.to_string());
    let mut vectors = embedder.embed_batch(&batch).await?;
    if vectors.len() != batch.len() {
        return Err(PersistError::Embed(EmbedError(format!(
            "embedding model returned {} vectors for {} inputs",
            vectors.len(),
            batch.len()
        ))));
    }
    let query_vector = vectors.pop().expect("batch always includes the query");

    let mut index = VectorIndex::new();
    // A persisted row is only usable if it is the SAME WIDTH as the query we
    // will score it against. An endpoint that changes output width while
    // keeping its model name passes `is_fresh` (each row's recorded dims match
    // its own blob), and `VectorIndex` scores a width mismatch as zero rather
    // than failing — silently draining the dense signal out of the ranking.
    // Comparing against the live query width turns that into a re-embed.
    let width = query_vector.len();
    let (usable, stale_width): (Vec<_>, Vec<_>) = fresh
        .into_iter()
        .partition(|(_, vector)| vector.len() == width);
    for (id, vector) in usable {
        index.insert(id, vector);
    }
    if !stale_width.is_empty() {
        // Re-embed exactly the rows whose stored width no longer matches, so a
        // width change costs one extra batch rather than a degraded ranking.
        let widened: Vec<String> = stale_width
            .iter()
            .filter_map(|(id, _)| items.iter().find(|item| item.id == *id))
            .map(embedding_text)
            .collect();
        let refreshed = embedder.embed_batch(&widened).await?;
        let now = Utc::now();
        for ((id, _), vector) in stale_width.iter().zip(refreshed) {
            if let Some(item) = items.iter().find(|item| item.id == *id) {
                let text = embedding_text(item);
                let hash = embedding_content_hash(&text);
                upsert_embedding(pool, item.id, &hash, embedder.model(), &vector, now).await?;
                index.insert(item.id, vector);
            }
        }
    }
    let now = Utc::now();
    for ((item, _text, hash), vector) in missing.iter().zip(vectors) {
        // Persist opportunistically so the next assembly (and the drain) find
        // the row fresh; the drain remains the authoritative refresher.
        upsert_embedding(pool, item.id, hash, embedder.model(), &vector, now).await?;
        index.insert(item.id, vector);
    }
    Ok((index, query_vector))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_blob_round_trips_exactly() {
        let vector = vec![0.0_f32, 1.5, -2.25, f32::MIN_POSITIVE];
        assert_eq!(decode_vector(&encode_vector(&vector)), vector);
    }

    #[test]
    fn ragged_blob_decodes_whole_values_only() {
        let mut bytes = encode_vector(&[1.0, 2.0]);
        bytes.push(0xFF); // a trailing partial value
        assert_eq!(decode_vector(&bytes), vec![1.0, 2.0]);
    }

    #[test]
    fn freshness_requires_model_hash_and_dims_agreement() {
        let stored = StoredEmbedding {
            content_hash: "abc".to_string(),
            model: "m1".to_string(),
            dims: 2,
            vector: vec![0.1, 0.2],
        };
        assert!(is_fresh(&stored, "m1", "abc"));
        assert!(!is_fresh(&stored, "m2", "abc"), "a model change is stale");
        assert!(!is_fresh(&stored, "m1", "xyz"), "a text change is stale");
        let mismatched = StoredEmbedding {
            dims: 3,
            ..stored.clone()
        };
        assert!(
            !is_fresh(&mismatched, "m1", "abc"),
            "a dims/vector mismatch is stale"
        );
    }
}
