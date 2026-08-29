//! PostgreSQL pairing transaction regression tests.
//!
//! These run when `DATABASE_URL` is present. CI and release workflows provide a
//! real PostgreSQL service so the foreign-key ordering and concurrent
//! single-use behaviour cannot silently fall back to `MemoryStore` coverage.

use std::sync::Arc;

use chrono::Utc;
use codypendent_control_plane::{
    auth::hash_token,
    store::{PairingChallenge, PairingCompletion},
    Membership, Organization, PgStore, Store, User,
};
use sqlx::{postgres::PgPoolOptions, Row};
use uuid::Uuid;

fn completion(daemon_id: Uuid, token: &str, now: chrono::DateTime<Utc>) -> PairingCompletion {
    PairingCompletion {
        daemon_id,
        display_name: format!("daemon-{daemon_id}"),
        consent_manifest_hash: hash_token("manifest"),
        max_publication_class: "metadata-shared".to_string(),
        accepts_remote_approvals: false,
        accepts_runner_dispatch: false,
        credential_id: Uuid::now_v7(),
        credential_audience: "control-plane".to_string(),
        credential_purpose: "sync".to_string(),
        credential_token_hash: hash_token(token),
        completed_at: now,
        credential_expires_at: now + chrono::Duration::days(365),
    }
}

#[tokio::test]
async fn pairing_is_fk_safe_atomic_and_single_use_in_postgres() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set; skipping live PostgreSQL pairing transaction test");
            return;
        }
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("connect to PostgreSQL");
    let store = Arc::new(PgStore::new(pool));
    store.run_migrations().await.expect("migrations");

    let now = Utc::now();
    let user_id = Uuid::now_v7();
    let org_id = Uuid::now_v7();
    store
        .create_user(User {
            id: user_id,
            display_name: "Pairing user".to_string(),
            primary_email: None,
            state: "active".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();
    store
        .create_organization(Organization {
            id: org_id,
            slug: format!("pairing-{org_id}"),
            display_name: "Pairing test".to_string(),
            max_publication_class: "metadata-shared".to_string(),
            max_classification: "internal".to_string(),
            data_residency: None,
            retention_days: None,
            policy_version: 1,
            created_at: now,
        })
        .await
        .unwrap();
    store
        .add_membership(Membership {
            organization_id: org_id,
            user_id,
            state: "active".to_string(),
            joined_at: Some(now),
            created_at: now,
        })
        .await
        .unwrap();

    let pairing_code = format!("cp_pair_{}", Uuid::now_v7());
    let code_hash = hash_token(&pairing_code);
    store
        .create_pairing_challenge(PairingChallenge {
            code_hash: code_hash.clone(),
            organization_id: org_id,
            initiated_by: user_id,
            requested_scope: serde_json::json!({
                "max_publication_class": "metadata-shared",
                "accepts_remote_approvals": false,
                "accepts_runner_dispatch": false,
                "repositories": []
            }),
            created_at: now,
            expires_at: now + chrono::Duration::minutes(15),
            consumed_at: None,
            daemon_id: None,
        })
        .await
        .unwrap();

    let first_id = Uuid::now_v7();
    let second_id = Uuid::now_v7();
    let first = store.complete_pairing(&code_hash, completion(first_id, "first", now));
    let second = store.complete_pairing(&code_hash, completion(second_id, "second", now));
    let (first, second) = tokio::join!(first, second);
    let outcomes = [first.unwrap(), second.unwrap()];
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_some()).count(),
        1
    );

    let daemon_count: i64 = sqlx::query("SELECT count(*) FROM daemons WHERE organization_id = $1")
        .bind(org_id)
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get(0);
    let credential_count: i64 = sqlx::query(
        "SELECT count(*) FROM workload_credentials wc JOIN daemons d ON d.id = wc.daemon_id WHERE d.organization_id = $1",
    )
    .bind(org_id)
    .fetch_one(store.pool())
    .await
    .unwrap()
    .get(0);
    assert_eq!(daemon_count, 1);
    assert_eq!(credential_count, 1);

    // A challenge is not a 15-minute policy freeze. If the organization is
    // narrowed before exchange, completion must re-check the live ceiling in
    // the same transaction and create neither row.
    sqlx::query("UPDATE organizations SET max_publication_class = 'private-local' WHERE id = $1")
        .bind(org_id)
        .execute(store.pool())
        .await
        .unwrap();
    let narrowed_code_hash = hash_token("narrowed-pairing-code");
    store
        .create_pairing_challenge(PairingChallenge {
            code_hash: narrowed_code_hash.clone(),
            organization_id: org_id,
            initiated_by: user_id,
            requested_scope: serde_json::json!({
                "max_publication_class": "metadata-shared",
                "accepts_remote_approvals": false,
                "accepts_runner_dispatch": false,
                "repositories": []
            }),
            created_at: now,
            expires_at: now + chrono::Duration::minutes(15),
            consumed_at: None,
            daemon_id: None,
        })
        .await
        .unwrap();
    let narrowed_daemon_id = Uuid::now_v7();
    assert!(store
        .complete_pairing(
            &narrowed_code_hash,
            completion(narrowed_daemon_id, "narrowed", now),
        )
        .await
        .unwrap()
        .is_none());
    assert!(store
        .get_daemon(narrowed_daemon_id)
        .await
        .unwrap()
        .is_none());
}
