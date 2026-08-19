//! Concurrency and non-disclosure properties of the control-plane stores.
//!
//! Everything here runs without a database except the tests that explicitly
//! probe `DATABASE_URL` and return early when it is unset, following
//! `tests/migrations_it.rs`. CI runs `cargo test --workspace --all-features`
//! with no PostgreSQL, so a skipped PostgreSQL test must be a pass.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use codypendent_control_plane::{
    audit::{
        compute_action_digest, uncomputed_digest, verify_audit_chain, AuditActorKind, AuditRecord,
    },
    error::ControlPlaneError,
    store::{
        IdempotencyRecord, StreamEvent, SyncDeltaApplication, SyncProjection, SyncReceipt,
        UserIdentity,
    },
    MemoryStore, Organization, PgStore, Store,
};
use codypendent_control_plane_protocol::ids::{AuditRecordId, OrganizationId, Sha256Digest};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

fn sample_record(org_id: Uuid, label: &str) -> AuditRecord {
    AuditRecord {
        id: AuditRecordId::new(),
        organization_id: OrganizationId::from_uuid(org_id),
        actor_kind: AuditActorKind::System,
        actor_id: None,
        action: label.to_string(),
        target_kind: "resource".to_string(),
        target_id: label.to_string(),
        action_digest: compute_action_digest(label.as_bytes()),
        correlation_id: None,
        // Both chain fields are the store's to compute.
        prev_hash: None,
        record_hash: uncomputed_digest(),
        detail: serde_json::json!({ "label": label }),
        occurred_at: Utc::now(),
    }
}

/// Every record links to the one before it, hashes verify, and no two records
/// share a predecessor — i.e. the chain is a line, not a fork.
fn assert_single_verifiable_chain(records: &[AuditRecord], expected_len: usize) {
    assert_eq!(
        records.len(),
        expected_len,
        "lost or duplicated audit records"
    );

    verify_audit_chain(records).expect("concurrent appends must leave one verifiable chain");

    let mut seen_prev: Vec<&Sha256Digest> = Vec::new();
    for record in records.iter().skip(1) {
        let prev = record
            .prev_hash
            .as_ref()
            .expect("only the genesis record may have no predecessor");
        assert!(
            !seen_prev.contains(&prev),
            "two records claim the same predecessor: the chain forked"
        );
        seen_prev.push(prev);
    }

    // The SQL reads order by (occurred_at DESC, id DESC). That order only agrees
    // with the chain while occurred_at increases along the chain.
    for pair in records.windows(2) {
        assert!(
            pair[1].occurred_at > pair[0].occurred_at,
            "chain order must be readable as timestamp order: {} then {}",
            pair[0].occurred_at,
            pair[1].occurred_at
        );
    }
}

/// Defect 1, in-process half. Proves the `Store` contract: many appends racing
/// on one organization still produce a single verifiable chain.
///
/// This does NOT prove the PostgreSQL fix — `MemoryStore` holds one write lock
/// across its read and its push, so it was never able to fork. The PostgreSQL
/// race lived in the gap between two pool connections and is covered by
/// `concurrent_audit_appends_are_atomic_on_postgres` below, which needs a
/// database.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_audit_appends_produce_one_verifiable_chain_in_memory() {
    let store = Arc::new(MemoryStore::new());
    let org_id = Uuid::now_v7();

    let appenders = 8;
    let per_appender = 8;
    let mut handles = Vec::new();
    for appender in 0..appenders {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            for n in 0..per_appender {
                store
                    .append_audit_record(sample_record(org_id, &format!("a{appender}-{n}")))
                    .await
                    .expect("append must succeed");
            }
        }));
    }
    for handle in handles {
        handle.await.expect("appender task panicked");
    }

    let total = appenders * per_appender;
    let mut records = store.list_audit_records(org_id, total + 10).await.unwrap();
    records.reverse(); // newest-first -> chain order

    assert_single_verifiable_chain(&records, total);
}

/// Defect 2. `occurred_at` is stamped by the caller before the store sees it, so
/// under load a record can arrive with a timestamp at or behind the current
/// chain tail. Reading the chain back by timestamp then reports a broken link
/// for records that are perfectly intact.
#[tokio::test]
async fn out_of_order_and_duplicate_timestamps_still_read_back_as_a_chain() {
    let store = MemoryStore::new();
    let org_id = Uuid::now_v7();

    let base = Utc.with_ymd_and_hms(2026, 8, 17, 12, 0, 0).unwrap();
    // Deliberately pathological: identical, then going backwards.
    let offsets = [
        Duration::seconds(10),
        Duration::seconds(10),
        Duration::seconds(10),
        Duration::seconds(3),
        Duration::seconds(0),
    ];

    for (n, offset) in offsets.iter().enumerate() {
        let mut record = sample_record(org_id, &format!("step-{n}"));
        record.occurred_at = base + *offset;
        store.append_audit_record(record).await.unwrap();
    }

    let mut records = store.list_audit_records(org_id, 100).await.unwrap();
    records.reverse();
    assert_single_verifiable_chain(&records, offsets.len());
}

/// Nanosecond precision hashed before an insert into a microsecond column can
/// never be reproduced from the row read back. Normalizing at append time is
/// what makes a PostgreSQL round trip verifiable at all.
#[tokio::test]
async fn appended_timestamps_are_stored_at_postgres_resolution() {
    let store = MemoryStore::new();
    let org_id = Uuid::now_v7();

    let mut record = sample_record(org_id, "sub-microsecond");
    record.occurred_at =
        Utc.with_ymd_and_hms(2026, 8, 17, 12, 0, 0).unwrap() + Duration::nanoseconds(123_456_789);

    let stored = store.append_audit_record(record).await.unwrap();
    assert_eq!(
        stored.occurred_at.timestamp_subsec_nanos() % 1_000,
        0,
        "sub-microsecond precision must be dropped before the hash is computed"
    );
}

/// Defect 4. A caller supplies `(provider, issuer, subject)` themselves, so a
/// distinguishable answer for "already linked" is an oracle for other people's
/// third-party accounts. The refusal must be the uniform absent answer.
#[tokio::test]
async fn a_taken_identity_is_refused_exactly_like_an_absent_one() {
    let store = MemoryStore::new();

    let first_owner = Uuid::now_v7();
    let identity = UserIdentity {
        id: Uuid::now_v7(),
        user_id: first_owner,
        provider: "github".to_string(),
        issuer: "https://github.com".to_string(),
        subject: "12345".to_string(),
        email_at_link: None,
        linked_at: Utc::now(),
        link_audit_id: Uuid::now_v7(),
    };
    store.create_user_identity(identity.clone()).await.unwrap();

    let attacker_attempt = UserIdentity {
        id: Uuid::now_v7(),
        user_id: Uuid::now_v7(),
        ..identity
    };
    let err = store
        .create_user_identity(attacker_attempt)
        .await
        .expect_err("a second user must not be able to claim a linked identity");

    // Byte-identical to the refusal an unauthorized link receives.
    let unauthorized = ControlPlaneError::forbidden("identity", "identity cannot be linked");
    assert_eq!(
        serde_json::to_string(&error_body(err)).unwrap(),
        serde_json::to_string(&error_body(unauthorized)).unwrap(),
        "a claimed identity must be indistinguishable from an absent one"
    );
}

fn error_body(err: ControlPlaneError) -> serde_json::Value {
    match err {
        ControlPlaneError::NotFound { resource, message }
        | ControlPlaneError::Forbidden { resource, message } => {
            serde_json::json!({ "type": "not_found", "resource": resource, "message": message })
        }
        other => serde_json::json!({ "type": "other", "message": other.to_string() }),
    }
}

/// Defect 5. The idempotency primitives are unused by any route, but they are
/// storage a route can rely on: first writer wins, and an expired key reads as
/// absent rather than replaying a stale response.
#[tokio::test]
async fn idempotency_records_are_first_writer_wins_and_expire() {
    let store = MemoryStore::new();
    let principal = Uuid::now_v7();

    let live = IdempotencyRecord {
        principal_kind: "user".to_string(),
        principal_id: principal,
        key: "key-1".to_string(),
        request_hash: hex::decode(compute_action_digest(b"body-a").as_str()).unwrap(),
        response_status: 201,
        response_body: serde_json::json!({ "id": "first" }),
        created_at: Utc::now(),
        expires_at: Utc::now() + Duration::hours(1),
    };
    assert!(store.save_idempotency_record(live.clone()).await.unwrap());

    let loser = IdempotencyRecord {
        response_status: 500,
        response_body: serde_json::json!({ "id": "second" }),
        ..live.clone()
    };
    assert!(
        !store.save_idempotency_record(loser).await.unwrap(),
        "a second writer for a live key must be told it lost, not overwrite the answer"
    );

    let replayed = store
        .get_idempotency_record("user", principal, "key-1")
        .await
        .unwrap()
        .expect("a live key must replay");
    assert_eq!(replayed.response_status, 201);
    assert_eq!(replayed.response_body, serde_json::json!({ "id": "first" }));

    let expired = IdempotencyRecord {
        key: "key-2".to_string(),
        expires_at: Utc::now() - Duration::seconds(1),
        ..live
    };
    store.save_idempotency_record(expired).await.unwrap();
    assert!(
        store
            .get_idempotency_record("user", principal, "key-2")
            .await
            .unwrap()
            .is_none(),
        "an expired key must read as absent, never replay a stale response"
    );
}

/// Repository reads must be tenant-scoped in the query, not filtered afterwards.
#[tokio::test]
async fn repository_lookup_is_scoped_to_the_organization() {
    let store = MemoryStore::new();
    let owning_org = Uuid::now_v7();
    let other_org = Uuid::now_v7();

    let repo = codypendent_control_plane::Repository {
        id: Uuid::now_v7(),
        organization_id: owning_org,
        federated_id: "fed-1".to_string(),
        display_name: "repo".to_string(),
        max_publication_class: "metadata-shared".to_string(),
        max_classification: "internal".to_string(),
        policy_version: 1,
        created_at: Utc::now(),
    };
    let repo_id = repo.id;
    store.create_repository(repo).await.unwrap();

    assert!(store
        .get_repository_in_org(owning_org, repo_id)
        .await
        .unwrap()
        .is_some());
    assert!(
        store
            .get_repository_in_org(other_org, repo_id)
            .await
            .unwrap()
            .is_none(),
        "a repository in another tenant must be absent, not fetched and then filtered"
    );
}

/// Defect 1, the half that needs a database. Two appends on separate pool
/// connections used to read the same `prev_hash` and both commit, forking the
/// chain permanently. Skipped without `DATABASE_URL`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_audit_appends_are_atomic_on_postgres() {
    let db_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!(
                "DATABASE_URL not set; skipping live PostgreSQL audit chain concurrency test"
            );
            return;
        }
    };

    let appenders = 8;
    let per_appender = 8;

    let pool = PgPoolOptions::new()
        // More connections than appenders, so the appends really are concurrent
        // rather than serialized by pool starvation.
        .max_connections(appenders as u32 + 2)
        .connect(&db_url)
        .await
        .expect("failed to connect to PostgreSQL");

    let store = Arc::new(PgStore::new(pool));
    store.run_migrations().await.expect("migrations must apply");

    let org_id = Uuid::now_v7();
    store
        .create_organization(Organization {
            id: org_id,
            slug: format!("audit-race-{}", org_id.simple()),
            display_name: "audit race".to_string(),
            max_publication_class: "metadata-shared".to_string(),
            max_classification: "internal".to_string(),
            data_residency: None,
            retention_days: None,
            policy_version: 1,
            created_at: Utc::now(),
        })
        .await
        .expect("organization must be created for the audit foreign key");

    let mut handles = Vec::new();
    for appender in 0..appenders {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            for n in 0..per_appender {
                store
                    .append_audit_record(sample_record(org_id, &format!("pg-{appender}-{n}")))
                    .await
                    .expect("append must succeed");
            }
        }));
    }
    for handle in handles {
        handle.await.expect("appender task panicked");
    }

    let total = appenders * per_appender;
    let mut records = store.list_audit_records(org_id, total + 10).await.unwrap();
    records.reverse();

    assert_single_verifiable_chain(&records, total);
}

/// A sync delta's receipt must not outlive a failure to apply its effect.
///
/// The receipt used to be its own autocommit, written before the projection
/// and the stream event. Anything that failed afterwards — a dropped
/// connection, a killed process, a constraint violation — left durable proof
/// of an effect that had never happened. The daemon's retry then hit the
/// duplicate short-circuit, was handed that receipt, and marked its outbox
/// entry acknowledged: the delta was silently lost, and nothing reported it.
///
/// The failure here is a real one, forced through a real transaction: the
/// stream event names an organization that does not exist, so its foreign key
/// rejects the insert *after* the receipt row has already been written inside
/// the transaction. The assertion is that no receipt survives.
///
/// Skipped without `DATABASE_URL`, following `tests/migrations_it.rs` — a
/// rollback can only be observed against a database that has transactions.
#[tokio::test]
async fn a_failed_sync_delta_leaves_no_receipt_behind() {
    let db_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set; skipping live PostgreSQL rollback test");
            return;
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to PostgreSQL");
    let store = PgStore::new(pool);
    store.run_migrations().await.expect("migrations must apply");

    let now = Utc::now();
    let user_id = Uuid::now_v7();
    let org_id = Uuid::now_v7();
    let daemon_id = Uuid::now_v7();

    store
        .create_user(codypendent_control_plane::store::User {
            id: user_id,
            display_name: "Pairer".to_string(),
            primary_email: None,
            state: "active".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("user must be stored");
    store
        .create_organization(Organization {
            id: org_id,
            slug: format!("rollback-{org_id}"),
            display_name: "Rollback".to_string(),
            max_publication_class: "org-shared".to_string(),
            max_classification: "internal".to_string(),
            data_residency: None,
            retention_days: None,
            policy_version: 1,
            created_at: now,
        })
        .await
        .expect("organization must be stored");
    store
        .register_daemon(codypendent_control_plane::Daemon {
            id: daemon_id,
            organization_id: org_id,
            paired_by: user_id,
            display_name: "Workstation".to_string(),
            consent_manifest_hash: vec![0u8; 32],
            max_publication_class: "org-shared".to_string(),
            accepts_remote_approvals: false,
            accepts_runner_dispatch: false,
            state: "active".to_string(),
            paired_at: Some(now),
            revoked_at: None,
            last_seen_at: Some(now),
            created_at: now,
        })
        .await
        .expect("daemon must be stored");

    let sequence = 1;
    let application = SyncDeltaApplication {
        receipt: SyncReceipt {
            id: Uuid::now_v7(),
            daemon_id,
            daemon_sequence: sequence,
            delta_kind: "session-summary".to_string(),
            payload_hash: vec![0u8; 32],
            class: "org-shared".to_string(),
            accepted_at: now,
        },
        projection: SyncProjection::None,
        event: StreamEvent {
            id: 0,
            // No such organization: the foreign key rejects this insert, and it
            // is reached only after the receipt row exists in the transaction.
            organization_id: Uuid::now_v7(),
            repository_id: None,
            stream: "sync".to_string(),
            payload: serde_json::json!({ "delta_kind": "session-summary" }),
            created_at: now,
        },
    };

    let result = store.apply_sync_delta(application).await;
    assert!(
        result.is_err(),
        "an unsatisfiable foreign key must surface as an error, not a silent success"
    );

    let receipt = store
        .get_sync_receipt(daemon_id, sequence)
        .await
        .expect("reading the receipt back must succeed");
    assert!(
        receipt.is_none(),
        "the receipt rolled back with its effect; a surviving one would be \
         handed to the daemon's retry and silently acknowledge a lost delta"
    );
}
