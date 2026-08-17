//! Integration tests for Marketplace commands, lifecycle states, and Brokered Secrets.

use codypendent_daemon::db;
use codypendent_daemon::marketplace::MarketplaceStore;
// `MarketplaceStore` here is the marketplace crate's verified store, distinct
// from the daemon's `MarketplaceStore` command facade imported above.
use codypendent_marketplace::{
    MarketplaceStore as VerifiedStore, PublisherTrustTier, TrustManager,
};
use codypendent_secrets::{LeaseContext, SecretBackendKind, SecretBroker};
use std::time::Duration;

/// The bytes a given version "ships". Registration verifies the manifest
/// checksum against these, and `marketplace_versions.content_hash` is UNIQUE —
/// so each version must carry genuinely different content.
fn artifact_for(version: &str) -> Vec<u8> {
    format!("sample-tool artifact payload v{version}").into_bytes()
}

fn artifact_base64(version: &str) -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        artifact_for(version),
    )
}

/// `sha256:<hex>` over the version's artifact, the shape `verify_artifact`
/// requires.
fn artifact_checksum(version: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(artifact_for(version));
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// A real `PluginManifest` (the `[package]` table this test used to send is not
/// the manifest schema). `signature` is left at the packaging placeholder, so
/// the package is unsigned and must be installed with `allow_unsigned = true`.
fn manifest(version: &str) -> String {
    format!(
        r#"
schema_version = 1
id = "sample-tool"
name = "Sample Tool"
version = "{version}"
kind = "wasm-component"
publisher = "publisher-alpha"

[runtime]
command = "sample_tool.wasm"

[security]
checksum = "{checksum}"
signature = "set-during-packaging"
"#,
        version = version,
        checksum = artifact_checksum(version),
    )
}

/// Registration refuses an unknown or untrusted publisher, so the publisher has
/// to exist and be trusted before any install can succeed.
async fn trust_publisher(pool: &sqlx::SqlitePool) {
    let trust = TrustManager::new(VerifiedStore::new(pool.clone()));
    trust
        .register_publisher(
            "publisher-alpha",
            "Publisher Alpha",
            &"11".repeat(32),
            PublisherTrustTier::Trusted,
            Some("operator"),
        )
        .await
        .expect("register trusted publisher");
}

#[tokio::test]
async fn marketplace_lifecycle_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = db::open_database(&temp.path().join("marketplace.db"))
        .await
        .expect("open database");

    let store = MarketplaceStore::new(pool.clone());
    let owner_uid = 1000;

    trust_publisher(&pool).await;
    let manifest_v1 = manifest("0.1.0");
    let artifact_v1 = artifact_base64("0.1.0");

    // 1. Install. Artifact bytes are mandatory: the manifest checksum is
    //    verified against them before anything is written.
    let installed = store
        .install(
            owner_uid,
            "sample-tool",
            Some(&manifest_v1),
            Some(&artifact_v1),
            true,
        )
        .await
        .expect("install package");

    assert_eq!(installed.id, "sample-tool");
    assert_eq!(installed.latest_version, "0.1.0");
    assert_eq!(installed.lifecycle.as_deref(), Some("installed_disabled"));

    // 2. Search
    let search_results = store
        .search(owner_uid, "sample", None)
        .await
        .expect("search packages");
    assert_eq!(search_results.len(), 1);
    assert_eq!(search_results[0].id, "sample-tool");

    // 3. Enable
    let enabled = store
        .enable(owner_uid, "sample-tool", Some("session-123"), None)
        .await
        .expect("enable package");
    assert_eq!(enabled.lifecycle.as_deref(), Some("enabled"));
    assert_eq!(enabled.enabled_scope.as_deref(), Some("session-123"));

    // 4. Update
    let manifest_v2 = manifest("0.2.0");
    let artifact_v2 = artifact_base64("0.2.0");
    let updated = store
        .update(
            owner_uid,
            "sample-tool",
            Some(&manifest_v2),
            Some(&artifact_v2),
            true,
        )
        .await
        .expect("update package");
    assert_eq!(updated.latest_version, "0.2.0");

    // 5. Disable
    let disabled = store
        .disable(owner_uid, "sample-tool")
        .await
        .expect("disable package");
    assert_eq!(disabled.lifecycle.as_deref(), Some("disabled"));

    // 6. Revoke
    let revoked = store
        .revoke(owner_uid, "sample-tool", "operator test revocation")
        .await
        .expect("revoke package");
    assert_eq!(revoked.lifecycle.as_deref(), Some("revoked"));

    // 7. Attempting to enable revoked package must fail
    let enable_revoked = store
        .enable(owner_uid, "sample-tool", None, None)
        .await
        .expect_err("enabling revoked package must fail");
    assert_eq!(enable_revoked.code, "marketplace.package-revoked");
}

#[tokio::test]
async fn secret_broker_lifecycle_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = db::open_database(&temp.path().join("secrets.db"))
        .await
        .expect("open database");

    let broker = SecretBroker::with_default_backends(pool.clone());
    let owner_uid = 1000;

    // 1. Register reference
    let reference = broker
        .register_reference(
            owner_uid,
            "github_token",
            SecretBackendKind::Environment,
            "TEST_GITHUB_TOKEN",
            "github.api.read",
            None,
            None,
        )
        .await
        .expect("register reference");

    assert_eq!(reference.name, "github_token");
    assert_eq!(reference.owner_uid, owner_uid);

    // 2. List references
    let list = broker
        .list_references(owner_uid)
        .await
        .expect("list references");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, reference.id);

    // Another user cannot list owner's secrets
    let other_list = broker.list_references(2000).await.expect("other user list");
    assert!(other_list.is_empty());

    // 3. Issue lease
    let context = LeaseContext {
        principal_uid: owner_uid,
        job_id: "job-1".to_string(),
        capability: "github.api.read".to_string(),
        organization_id: None,
        repository_id: None,
    };
    let lease = broker
        .issue_lease("github_token", &context, Duration::from_secs(60))
        .await
        .expect("issue lease");
    assert_eq!(lease.reference_id, reference.id);

    // 4. Revoke reference
    broker
        .revoke_reference(owner_uid, &reference.id, Some("test revoke"))
        .await
        .expect("revoke reference");

    // After revocation, listing active returns empty
    let list_after = broker
        .list_references(owner_uid)
        .await
        .expect("list after revoke");
    assert!(list_after.is_empty());
}
