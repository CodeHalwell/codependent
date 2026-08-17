//! Tombstone tracking, draining, and publication revocation.
//!
//! Reconnecting daemons must consume unacknowledged tombstones before publishing
//! new deltas. Sealing a batch while unacknowledged tombstones exist is strictly
//! refused to prevent resurrecting deleted facts.

use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::publication::PublicationClass;

/// Reason why a fact was tombstoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TombstoneReason {
    #[serde(rename = "deleted")]
    Deleted,
    #[serde(rename = "narrowed")]
    Narrowed,
    #[serde(rename = "revoked")]
    Revoked,
}

impl TombstoneReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deleted => "deleted",
            Self::Narrowed => "narrowed",
            Self::Revoked => "revoked",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "deleted" => Self::Deleted,
            "narrowed" => Self::Narrowed,
            "revoked" => Self::Revoked,
            _ => Self::Deleted,
        }
    }
}

/// Subject kind for a tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TombstoneSubjectKind {
    #[serde(rename = "node")]
    Node,
    #[serde(rename = "edge")]
    Edge,
    #[serde(rename = "repository")]
    Repository,
}

impl TombstoneSubjectKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Edge => "edge",
            Self::Repository => "repository",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "node" => Self::Node,
            "edge" => Self::Edge,
            "repository" => Self::Repository,
            _ => Self::Node,
        }
    }
}

/// A tombstone recording a deleted, narrowed, or revoked shared fact.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GraphTombstone {
    pub id: String,
    pub repository_id: String,
    pub subject_kind: TombstoneSubjectKind,
    pub subject_id: String,
    pub reason: TombstoneReason,
    pub published_class: PublicationClass,
    pub created_at: String,
    pub created_by_uid: i64,
    pub acknowledged_at: Option<String>,
    pub remote_receipt: Option<String>,
}

#[derive(Debug, Error)]
pub enum TombstoneError {
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("unacknowledged tombstones pending: {count} unacknowledged tombstone(s) must be drained before sealing batch")]
    UnacknowledgedPending { count: usize },
    #[error("tombstone not found: {0}")]
    NotFound(String),
}

/// Tombstone manager operations.
pub struct TombstoneManager;

impl TombstoneManager {
    /// Inserts a new tombstone into `graph_tombstone`.
    pub async fn record_tombstone(
        pool: &SqlitePool,
        repository_id: &str,
        subject_kind: TombstoneSubjectKind,
        subject_id: &str,
        reason: TombstoneReason,
        published_class: PublicationClass,
        created_by_uid: i64,
    ) -> Result<GraphTombstone, TombstoneError> {
        let id = Uuid::now_v7().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO graph_tombstone \
             (id, repository_id, subject_kind, subject_id, reason, published_class, created_at, created_by_uid, acknowledged_at, remote_receipt) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL)"
        )
        .bind(&id)
        .bind(repository_id)
        .bind(subject_kind.as_str())
        .bind(subject_id)
        .bind(reason.as_str())
        .bind(published_class.as_str())
        .bind(&now)
        .bind(created_by_uid)
        .execute(pool)
        .await?;

        Ok(GraphTombstone {
            id,
            repository_id: repository_id.to_string(),
            subject_kind,
            subject_id: subject_id.to_string(),
            reason,
            published_class,
            created_at: now,
            created_by_uid,
            acknowledged_at: None,
            remote_receipt: None,
        })
    }

    /// Lists unacknowledged tombstones ordered by `created_at`.
    pub async fn list_unacknowledged(
        pool: &SqlitePool,
        repository_id: &str,
    ) -> Result<Vec<GraphTombstone>, TombstoneError> {
        let rows = sqlx::query(
            "SELECT id, repository_id, subject_kind, subject_id, reason, published_class, \
                    created_at, created_by_uid, acknowledged_at, remote_receipt \
             FROM graph_tombstone \
             WHERE repository_id = ? AND acknowledged_at IS NULL \
             ORDER BY created_at ASC",
        )
        .bind(repository_id)
        .fetch_all(pool)
        .await?;

        let mut tombstones = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.get("id");
            let repo_id: String = r.get("repository_id");
            let sk: String = r.get("subject_kind");
            let sid: String = r.get("subject_id");
            let reason_str: String = r.get("reason");
            let class_str: String = r.get("published_class");
            let created_at: String = r.get("created_at");
            let uid: i64 = r.get("created_by_uid");
            let ack_at: Option<String> = r.get("acknowledged_at");
            let receipt: Option<String> = r.get("remote_receipt");

            tombstones.push(GraphTombstone {
                id,
                repository_id: repo_id,
                subject_kind: TombstoneSubjectKind::parse(&sk),
                subject_id: sid,
                reason: TombstoneReason::parse(&reason_str),
                published_class: PublicationClass::from_str_lenient(&class_str),
                created_at,
                created_by_uid: uid,
                acknowledged_at: ack_at,
                remote_receipt: receipt,
            });
        }

        Ok(tombstones)
    }

    /// Acknowledges tombstones with a receipt.
    pub async fn acknowledge_tombstones(
        pool: &SqlitePool,
        tombstone_ids: &[String],
        remote_receipt: &str,
    ) -> Result<usize, TombstoneError> {
        if tombstone_ids.is_empty() {
            return Ok(0);
        }

        let now = chrono::Utc::now().to_rfc3339();
        let mut total = 0;
        for id in tombstone_ids {
            let res = sqlx::query(
                "UPDATE graph_tombstone \
                 SET acknowledged_at = ?, remote_receipt = ? \
                 WHERE id = ? AND acknowledged_at IS NULL",
            )
            .bind(&now)
            .bind(remote_receipt)
            .bind(id)
            .execute(pool)
            .await?;
            total += res.rows_affected() as usize;
        }

        Ok(total)
    }

    /// Enforces that all tombstones are drained/acknowledged before sealing a batch.
    pub async fn ensure_tombstones_drained(
        pool: &SqlitePool,
        repository_id: &str,
    ) -> Result<(), TombstoneError> {
        let count_row = sqlx::query(
            "SELECT COUNT(*) as count FROM graph_tombstone \
             WHERE repository_id = ? AND acknowledged_at IS NULL",
        )
        .bind(repository_id)
        .fetch_one(pool)
        .await?;

        let count: i64 = count_row.get("count");
        if count > 0 {
            return Err(TombstoneError::UnacknowledgedPending {
                count: count as usize,
            });
        }

        Ok(())
    }

    /// Revokes an explicitly published node or edge.
    ///
    /// Writes a tombstone with reason `'revoked'`, updates `graph_publication`
    /// decision to `'retracted'`, and removes the shared projection row.
    pub async fn revoke_publication(
        pool: &SqlitePool,
        repository_id: &str,
        subject_kind: TombstoneSubjectKind,
        subject_id: &str,
        actor_uid: i64,
    ) -> Result<GraphTombstone, TombstoneError> {
        let mut tx = pool.begin().await?;

        // Determine the published class from the shared projection or graph_publication
        let published_class = match subject_kind {
            TombstoneSubjectKind::Node => {
                let row = sqlx::query(
                    "SELECT class FROM shared_graph_node WHERE shared_node_id = ? AND repository_id = ?",
                )
                .bind(subject_id)
                .bind(repository_id)
                .fetch_optional(&mut *tx)
                .await?;

                if let Some(r) = row {
                    let c: String = r.get("class");
                    PublicationClass::from_str_lenient(&c)
                } else {
                    PublicationClass::MetadataShared
                }
            }
            TombstoneSubjectKind::Edge => {
                let row =
                    sqlx::query("SELECT class FROM shared_graph_edge WHERE shared_edge_id = ?")
                        .bind(subject_id)
                        .fetch_optional(&mut *tx)
                        .await?;

                if let Some(r) = row {
                    let c: String = r.get("class");
                    PublicationClass::from_str_lenient(&c)
                } else {
                    PublicationClass::MetadataShared
                }
            }
            TombstoneSubjectKind::Repository => PublicationClass::MetadataShared,
        };

        // 1. Record tombstone
        let tombstone_id = Uuid::now_v7().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO graph_tombstone \
             (id, repository_id, subject_kind, subject_id, reason, published_class, created_at, created_by_uid) \
             VALUES (?, ?, ?, ?, 'revoked', ?, ?, ?)",
        )
        .bind(&tombstone_id)
        .bind(repository_id)
        .bind(subject_kind.as_str())
        .bind(subject_id)
        .bind(published_class.as_str())
        .bind(&now)
        .bind(actor_uid)
        .execute(&mut *tx)
        .await?;

        // 2. Update publication decision to retracted
        sqlx::query(
            "UPDATE graph_publication \
             SET decision = 'retracted' \
             WHERE subject_id = ? AND subject_kind = ?",
        )
        .bind(subject_id)
        .bind(subject_kind.as_str())
        .execute(&mut *tx)
        .await?;

        // 3. Remove or clear from shared projection
        match subject_kind {
            TombstoneSubjectKind::Node => {
                sqlx::query("DELETE FROM shared_graph_node WHERE shared_node_id = ?")
                    .bind(subject_id)
                    .execute(&mut *tx)
                    .await?;
            }
            TombstoneSubjectKind::Edge => {
                sqlx::query("DELETE FROM shared_graph_edge WHERE shared_edge_id = ?")
                    .bind(subject_id)
                    .execute(&mut *tx)
                    .await?;
            }
            TombstoneSubjectKind::Repository => {
                sqlx::query("DELETE FROM shared_graph_node WHERE repository_id = ?")
                    .bind(repository_id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(
                    "DELETE FROM shared_graph_edge WHERE from_repository_id = ? OR to_repository_id = ?",
                )
                .bind(repository_id)
                .bind(repository_id)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;

        Ok(GraphTombstone {
            id: tombstone_id,
            repository_id: repository_id.to_string(),
            subject_kind,
            subject_id: subject_id.to_string(),
            reason: TombstoneReason::Revoked,
            published_class,
            created_at: now,
            created_by_uid: actor_uid,
            acknowledged_at: None,
            remote_receipt: None,
        })
    }
}
