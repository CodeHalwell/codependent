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
}

/// The production [`DeliveryStore`], backed by the shared SQLite database.
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
