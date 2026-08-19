//! Integration tests for the secret broker, context binding, and audit invariants.

use chrono::Utc;
use codypendent_secrets::{
    AuditOutcome, LeaseContext, LeasedSecret, SecretBackendKind, SecretBroker, SecretError,
};
use sqlx::SqlitePool;
use std::time::Duration;

async fn setup_test_db() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory db");

    sqlx::query(
        r#"
        CREATE TABLE secret_references (
            id TEXT PRIMARY KEY,
            owner_uid INTEGER NOT NULL,
            name TEXT NOT NULL,
            backend TEXT NOT NULL CHECK (backend IN
                ('environment', 'keychain', 'managed', 'vault', 'workload_identity')),
            locator TEXT NOT NULL,
            capability TEXT NOT NULL,
            organization_id TEXT,
            repository_id TEXT,
            accepted_digest TEXT NOT NULL,
            created_at TEXT NOT NULL,
            rotated_at TEXT,
            revoked_at TEXT,
            revoked_reason TEXT CHECK (revoked_reason IS NULL OR revoked_at IS NOT NULL),
            UNIQUE (owner_uid, name, capability)
        );

        CREATE TABLE secret_leases (
            id TEXT PRIMARY KEY,
            reference_id TEXT NOT NULL REFERENCES secret_references(id),
            principal_uid INTEGER NOT NULL,
            organization_id TEXT,
            repository_id TEXT,
            job_id TEXT NOT NULL,
            capability TEXT NOT NULL,
            issue_key TEXT NOT NULL,
            issued_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            backend_lease_handle TEXT,
            state TEXT NOT NULL CHECK (state IN ('active', 'expired', 'revoked', 'failed')),
            revoked_at TEXT,
            revoked_reason TEXT,
            CHECK (state <> 'revoked' OR revoked_at IS NOT NULL),
            UNIQUE (issue_key)
        );

        CREATE TABLE secret_audit (
            id TEXT PRIMARY KEY,
            reference_id TEXT REFERENCES secret_references(id),
            lease_id TEXT REFERENCES secret_leases(id),
            event TEXT NOT NULL CHECK (event IN
                ('issued', 'used', 'denied', 'expired', 'rotated', 'revoked', 'backend_error')),
            principal_uid INTEGER NOT NULL,
            job_id TEXT,
            capability TEXT,
            outcome_code TEXT NOT NULL,
            requested_name TEXT,
            occurred_at TEXT NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("schema applied");

    pool
}

#[test]
fn leased_material_has_no_debug_or_serialize() {
    let sentinel = "super_secret_sentinel_token_12345";
    let leased = LeasedSecret::from_text(sentinel);
    let debug_str = format!("{leased:?}");

    assert!(!debug_str.contains(sentinel));
    assert!(debug_str.contains("<redacted>"));
    assert_eq!(leased.expose_str().unwrap(), sentinel);
}

#[tokio::test]
async fn issue_is_idempotent_per_context() {
    let pool = setup_test_db().await;
    let broker = SecretBroker::with_default_backends(pool);

    std::env::set_var("TEST_IDEMPOTENT_KEY", "material_abc_123");

    broker
        .register_reference(
            1000,
            "github-token",
            SecretBackendKind::Environment,
            "TEST_IDEMPOTENT_KEY",
            "github.api",
            Some("org-1"),
            Some("repo-1"),
        )
        .await
        .expect("register reference");

    let context = LeaseContext::new(1000, "job-42", "github.api")
        .with_org("org-1")
        .with_repo("repo-1");

    let lease1 = broker
        .issue_lease("github-token", &context, Duration::from_secs(300))
        .await
        .expect("issue lease 1");

    let lease2 = broker
        .issue_lease("github-token", &context, Duration::from_secs(300))
        .await
        .expect("issue lease 2");

    assert_eq!(
        lease1.id, lease2.id,
        "idempotent issue must return the same lease"
    );
    assert_eq!(lease1.issue_key, lease2.issue_key);
}

#[tokio::test]
async fn a_job_cannot_widen_its_capability_after_acceptance() {
    let pool = setup_test_db().await;
    let broker = SecretBroker::with_default_backends(pool);

    broker
        .register_reference(
            1000,
            "api-token",
            SecretBackendKind::Environment,
            "API_KEY",
            "github.api",
            None,
            None,
        )
        .await
        .expect("register reference");

    let context = LeaseContext::new(1000, "job-compromised", "slack.chat");
    let err = broker
        .issue_lease("api-token", &context, Duration::from_secs(300))
        .await
        .expect_err("widening capability must be refused");

    assert!(matches!(err, SecretError::NotFound(_)));
}

#[tokio::test]
async fn an_expired_or_revoked_lease_is_refused_at_use() {
    let pool = setup_test_db().await;
    let broker = SecretBroker::with_default_backends(pool.clone());

    std::env::set_var("TEST_EXPIRY_KEY", "active_secret_val");

    broker
        .register_reference(
            1000,
            "temp-key",
            SecretBackendKind::Environment,
            "TEST_EXPIRY_KEY",
            "test.read",
            None,
            None,
        )
        .await
        .expect("register reference");

    let context = LeaseContext::new(1000, "job-exp", "test.read");

    let lease = broker
        .issue_lease("temp-key", &context, Duration::from_secs(300))
        .await
        .expect("issue lease");

    // Manually expire the lease in the database
    let past = (Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();
    sqlx::query("UPDATE secret_leases SET expires_at = ? WHERE id = ?")
        .bind(&past)
        .bind(&lease.id)
        .execute(&pool)
        .await
        .expect("update expiry");

    let err = broker
        .resolve_lease(&lease.id, &context)
        .await
        .expect_err("expired lease must fail resolution");
    assert!(matches!(err, SecretError::Expired));

    // Test revocation
    let lease2 = broker
        .issue_lease("temp-key", &context, Duration::from_secs(300))
        .await
        .expect("issue lease 2");

    broker
        .revoke_lease(&lease2.id, Some("compromised"))
        .await
        .expect("revoke lease");

    let err2 = broker
        .resolve_lease(&lease2.id, &context)
        .await
        .expect_err("revoked lease must fail resolution");
    assert!(matches!(err2, SecretError::Revoked(_)));
}

#[tokio::test]
async fn audit_records_events_without_values() {
    let pool = setup_test_db().await;
    let broker = SecretBroker::with_default_backends(pool);

    let sentinel = "SENTINEL_SECRET_MATERIAL_NEVER_LOGGED_999";
    std::env::set_var("SENTINEL_VAR", sentinel);

    let reference = broker
        .register_reference(
            1000,
            "sentinel-secret",
            SecretBackendKind::Environment,
            "SENTINEL_VAR",
            "sentinel.cap",
            None,
            None,
        )
        .await
        .expect("register");

    let context = LeaseContext::new(1000, "job-sentinel", "sentinel.cap");
    let lease = broker
        .issue_lease("sentinel-secret", &context, Duration::from_secs(300))
        .await
        .expect("issue");

    let resolved = broker
        .resolve_lease(&lease.id, &context)
        .await
        .expect("resolve");
    assert_eq!(resolved.expose_str().unwrap(), sentinel);

    broker
        .rotate_reference(
            1000,
            &reference.id,
            "SENTINEL_VAR",
            SecretBackendKind::Environment,
        )
        .await
        .expect("rotate");

    broker
        .revoke_reference(1000, &reference.id, Some("test done"))
        .await
        .expect("revoke");

    // Denied call
    let _ = broker
        .issue_lease("unknown-nonexistent", &context, Duration::from_secs(300))
        .await;

    // Check all audit records
    let records = broker.get_audit_records(100).await.expect("get audit");
    assert!(!records.is_empty());

    for rec in records {
        assert!(!rec.outcome_code.contains(sentinel));
        if let Some(ref name) = rec.requested_name {
            assert!(!name.contains(sentinel));
        }
    }
}

/// The schema says `outcome_code` is a dotted code and never a rendered
/// message. Prove it for every row the broker actually writes, across the
/// success, denial, expiry, rotation, revocation and backend-failure paths.
#[tokio::test]
async fn every_audit_outcome_code_is_a_known_dotted_code() {
    let pool = setup_test_db().await;
    let broker = SecretBroker::with_default_backends(pool.clone());

    std::env::set_var("TEST_AUDIT_SHAPE_VAR", "audit_shape_value");

    let reference = broker
        .register_reference(
            1000,
            "shape-secret",
            SecretBackendKind::Environment,
            "TEST_AUDIT_SHAPE_VAR",
            "shape.cap",
            None,
            None,
        )
        .await
        .expect("register");

    let context = LeaseContext::new(1000, "job-shape", "shape.cap");
    let lease = broker
        .issue_lease("shape-secret", &context, Duration::from_secs(300))
        .await
        .expect("issue");
    broker
        .resolve_lease(&lease.id, &context)
        .await
        .expect("use");

    // A backend failure path: a reference whose backend refuses.
    broker
        .register_reference(
            1000,
            "vault-secret",
            SecretBackendKind::Vault,
            "secret/data/x",
            "shape.cap",
            None,
            None,
        )
        .await
        .expect("register vault reference");
    let vault_lease = broker
        .issue_lease("vault-secret", &context, Duration::from_secs(300))
        .await
        .expect("issue vault lease");
    let vault_err = broker
        .resolve_lease(&vault_lease.id, &context)
        .await
        .expect_err("an unconfigured backend must refuse through the broker");
    assert_eq!(vault_err.outcome_code(), "secrets.backend-not-configured");

    // Denials, rotation, revocation.
    let _ = broker
        .issue_lease("no-such-name", &context, Duration::from_secs(300))
        .await;
    let _ = broker.resolve_lease("no-such-lease", &context).await;
    broker
        .rotate_reference(
            1000,
            &reference.id,
            "TEST_AUDIT_SHAPE_VAR",
            SecretBackendKind::Environment,
        )
        .await
        .expect("rotate");
    broker
        .revoke_reference(1000, &reference.id, Some("done"))
        .await
        .expect("revoke");

    let known: Vec<&str> = AuditOutcome::all().iter().map(|o| o.as_str()).collect();
    let records = broker.get_audit_records(500).await.expect("audit");
    assert!(
        records.len() >= 8,
        "expected the full spread of audit paths"
    );
    for rec in records {
        assert!(
            known.contains(&rec.outcome_code.as_str()),
            "`{}` is not a member of the closed AuditOutcome set",
            rec.outcome_code
        );
        assert!(
            !rec.outcome_code.contains(' '),
            "{} is a message",
            rec.outcome_code
        );
    }
}

/// An unconfigured backend must refuse. It must never resolve to material from
/// a different backend, and it must never claim the secret was simply absent.
#[tokio::test]
async fn an_unconfigured_backend_refuses_and_never_substitutes() {
    let pool = setup_test_db().await;
    let broker = SecretBroker::with_default_backends(pool);

    // A value is present in the environment under the same locator text. A
    // silent fallback to the environment backend would find it.
    std::env::set_var("shared-locator", "environment_material_must_not_leak");

    for backend in [
        SecretBackendKind::Managed,
        SecretBackendKind::Vault,
        SecretBackendKind::Keychain,
        SecretBackendKind::WorkloadIdentity,
    ] {
        let name = format!("ref-{}", backend.as_str());
        broker
            .register_reference(
                1000,
                &name,
                backend,
                "shared-locator",
                "sub.cap",
                None,
                None,
            )
            .await
            .expect("register");

        let context = LeaseContext::new(1000, "job-sub", "sub.cap");
        let lease = broker
            .issue_lease(&name, &context, Duration::from_secs(300))
            .await
            .expect("issue");
        let err = broker
            .resolve_lease(&lease.id, &context)
            .await
            .expect_err("an unconfigured backend must refuse");
        let code = err.outcome_code();
        // Keychain answers `keychain-unsupported` on a platform without one and
        // `backend-not-configured` on a platform that has one but no client.
        assert!(
            code == "secrets.backend-not-configured" || code == "secrets.keychain-unsupported",
            "{} did not refuse: {code}",
            backend.as_str()
        );
    }
}

/// Revocation must survive the agent simply asking again.
///
/// `issue_key` is derived from the request context, so a revoked principal
/// re-requesting the same capability lands on the SAME lease row — and the
/// renew path cleared `state` and `revoked_at`, handing back a fresh active
/// lease for the credential that had just been killed. The refusal at redeem
/// did not help: revoke → re-issue → redeem walked around it.
#[tokio::test]
async fn a_revoked_lease_cannot_be_reissued_by_asking_again() {
    let pool = setup_test_db().await;
    let broker = SecretBroker::with_default_backends(pool.clone());

    std::env::set_var("TEST_REISSUE_KEY", "leaked_secret_value");

    broker
        .register_reference(
            1000,
            "reissue-key",
            SecretBackendKind::Environment,
            "TEST_REISSUE_KEY",
            "test.read",
            None,
            None,
        )
        .await
        .expect("register reference");

    let context = LeaseContext::new(1000, "job-reissue", "test.read");

    let lease = broker
        .issue_lease("reissue-key", &context, Duration::from_secs(300))
        .await
        .expect("issue lease");

    broker
        .revoke_lease(&lease.id, Some("credential leaked"))
        .await
        .expect("revoke lease");

    // The kill switch: asking again must NOT resurrect it.
    let err = broker
        .issue_lease("reissue-key", &context, Duration::from_secs(300))
        .await
        .expect_err("a revoked lease must not be reissued");
    assert!(
        matches!(err, SecretError::Revoked(_)),
        "expected Revoked, got {err:?}"
    );

    // And the row is still revoked, not quietly reactivated underneath.
    let err_use = broker
        .resolve_lease(&lease.id, &context)
        .await
        .expect_err("the revoked lease must still be unusable");
    assert!(matches!(err_use, SecretError::Revoked(_)));
}
