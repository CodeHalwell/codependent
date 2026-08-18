//! Atomic replay-idempotency store for webhook deliveries.
//!
//! GitHub retries deliveries; every delivery carries a unique
//! `X-GitHub-Delivery` GUID. Recording that GUID *before* producing any internal
//! event makes ingestion replay-safe: a redelivered payload (same GUID) is
//! acknowledged but never normalized a second time. Ingestion also reserves a
//! signed-content fingerprint to prevent a captured body being replayed under a
//! forged GUID. Both keys are reserved together: a crash or store failure can
//! never commit one and permanently burn the legitimate retry of the other.

use std::collections::HashSet;

use super::ingest::{EndpointConfig, EndpointResolver};
use super::WebhookError;

/// Atomically reserves a delivery GUID and signed-content fingerprint.
#[async_trait::async_trait]
pub trait DeliveryStore: Send + Sync {
    /// Reserve both replay keys, returning `true` only when neither has been
    /// seen. Implementations must insert both or neither.
    async fn reserve_if_new(
        &self,
        delivery_id: &str,
        event_type: &str,
        content_fingerprint: &str,
    ) -> Result<bool, WebhookError>;

    /// Release a reservation whose delivery was **never dispatched**, so the
    /// sender's retry is judged on its merits instead of being answered as a
    /// duplicate of something that never happened.
    ///
    /// Reserving before dispatch is what makes replay protection sound; keeping
    /// the reservation after a dispatch that *failed* is what made a transient
    /// sink error permanent data loss — the caller returns 5xx, GitHub redelivers
    /// the same GUID, and dedup answers 200 without ever producing the event.
    /// Implementations must remove **both** keys, and must be a no-op for keys
    /// that are not present.
    ///
    /// Only ever called on a dispatch that returned an error, so it can never
    /// widen the window for a delivery that was acted on.
    async fn release(
        &self,
        delivery_id: &str,
        content_fingerprint: &str,
    ) -> Result<(), WebhookError>;
}

/// The production [`DeliveryStore`], backed by the shared SQLite database.
#[derive(Clone)]
pub struct SqliteDeliveryStore {
    pool: sqlx::SqlitePool,
}

impl SqliteDeliveryStore {
    /// Wrap a pool. The `webhook_deliveries` table is created by migration
    /// `0005_phase3.sql`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl DeliveryStore for SqliteDeliveryStore {
    async fn reserve_if_new(
        &self,
        delivery_id: &str,
        event_type: &str,
        content_fingerprint: &str,
    ) -> Result<bool, WebhookError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM webhook_deliveries \
             WHERE delivery_id = ? OR delivery_id = ?)",
        )
        .bind(delivery_id)
        .bind(content_fingerprint)
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            tx.rollback().await?;
            return Ok(false);
        }

        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO webhook_deliveries (delivery_id, event_type, received_at) \
             VALUES (?, ?, ?), (?, 'signed-content', ?)",
        )
        .bind(delivery_id)
        .bind(event_type)
        .bind(&now)
        .bind(content_fingerprint)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn release(
        &self,
        delivery_id: &str,
        content_fingerprint: &str,
    ) -> Result<(), WebhookError> {
        // Both keys or neither, in one immediate transaction — the mirror image
        // of the reservation, so a crash mid-release can never leave the GUID
        // burnt while the fingerprint is free (or the reverse), which would make
        // the retry undeliverable in a way no operator could see.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM webhook_deliveries WHERE delivery_id = ? OR delivery_id = ?")
            .bind(delivery_id)
            .bind(content_fingerprint)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl EndpointResolver for SqliteDeliveryStore {
    async fn resolve_endpoint(
        &self,
        endpoint_id: &str,
    ) -> Result<Option<EndpointConfig>, WebhookError> {
        let row: Option<(String, String, String, i64, i64)> = sqlx::query_as(
            "SELECT endpoint_id, scheme, signing_key_ref, body_limit_bytes, replay_window_seconds \
             FROM automation_endpoints WHERE endpoint_id = ? AND disabled_at IS NULL",
        )
        .bind(endpoint_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(
            |(endpoint_id, scheme, signing_key_ref, body_limit_bytes, replay_window_seconds)| {
                EndpointConfig {
                    endpoint_id,
                    scheme,
                    signing_key_ref,
                    body_limit_bytes: body_limit_bytes as usize,
                    replay_window_seconds: replay_window_seconds as u64,
                }
            },
        ))
    }
}

/// An in-memory [`DeliveryStore`] for tests: a `HashSet` guarded by an async
/// mutex, with the same first-seen semantics as the SQLite store.
#[derive(Default)]
pub struct InMemoryDeliveryStore {
    seen: tokio::sync::Mutex<HashSet<String>>,
}

#[async_trait::async_trait]
impl DeliveryStore for InMemoryDeliveryStore {
    async fn reserve_if_new(
        &self,
        delivery_id: &str,
        _event_type: &str,
        content_fingerprint: &str,
    ) -> Result<bool, WebhookError> {
        let mut seen = self.seen.lock().await;
        if seen.contains(delivery_id) || seen.contains(content_fingerprint) {
            return Ok(false);
        }
        seen.insert(delivery_id.to_string());
        seen.insert(content_fingerprint.to_string());
        Ok(true)
    }

    async fn release(
        &self,
        delivery_id: &str,
        content_fingerprint: &str,
    ) -> Result<(), WebhookError> {
        let mut seen = self.seen.lock().await;
        seen.remove(delivery_id);
        seen.remove(content_fingerprint);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_reservation_is_atomic_and_dedups_both_keys() {
        let store = InMemoryDeliveryStore::default();
        assert!(store
            .reserve_if_new("guid-1", "push", "body-1")
            .await
            .unwrap());
        assert!(!store
            .reserve_if_new("guid-2", "push", "body-1")
            .await
            .unwrap());
        assert!(store
            .reserve_if_new("guid-2", "push", "body-2")
            .await
            .unwrap());
    }
}
