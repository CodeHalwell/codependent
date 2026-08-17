//! Lifecycle state machine, update permissions, revocation, and catalog integration tests (Milestone 5, Tasks 5.4 & 5.5).

use codypendent_marketplace::{
    checksum_of, signing_digest, CompatibilityChecker, ContentAddressedStore,
    InstallLifecycleState, MarketplaceCatalog, MarketplaceLifecycleManager, MarketplaceStore,
    PackageVerifier, PublisherTrustTier, TrustManager, TrustedPublishers,
};
use codypendent_sandbox::{parse_manifest, PluginManifest};
use ed25519_dalek::{Signer, SigningKey};
use flate2::write::GzEncoder;
use flate2::Compression;
use sqlx::sqlite::SqlitePoolOptions;
use tar::{Builder, Header};
use tempfile::tempdir;

fn build_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        for (path, content) in entries {
            let mut header = Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder.append_data(&mut header, *path, *content).unwrap();
        }
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn build_signed_package(
    id: &str,
    publisher: &str,
    version: &str,
    capabilities_toml: &str,
    artifact_bytes: &[u8],
    signing_key: &SigningKey,
) -> (String, PluginManifest) {
    let checksum = checksum_of(artifact_bytes);
    let toml_unsigned = format!(
        r#"
schema_version = 1
id = "{id}"
name = "Test Package {id}"
version = "{version}"
kind = "wasm-component"
publisher = "{publisher}"
scopes = ["workspace"]
{capabilities_toml}
[runtime]
command = "main.wasm"
[security]
checksum = "{checksum}"
signature = ""
"#
    );

    let unsigned_manifest = parse_manifest(&toml_unsigned).expect("valid unsigned manifest");
    let digest = signing_digest(&unsigned_manifest);
    let signature = signing_key.sign(&digest);
    let sig_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        signature.to_bytes(),
    );

    let toml_signed = format!(
        r#"
schema_version = 1
id = "{id}"
name = "Test Package {id}"
version = "{version}"
kind = "wasm-component"
publisher = "{publisher}"
scopes = ["workspace"]
{capabilities_toml}
[runtime]
command = "main.wasm"
[security]
checksum = "{checksum}"
signature = "{sig_b64}"
"#
    );

    let signed_manifest = parse_manifest(&toml_signed).expect("valid signed manifest");
    (toml_signed, signed_manifest)
}

async fn setup_test_db() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .connect("sqlite::memory:")
        .await
        .unwrap();

    let migration_sql = include_str!("../../../migrations/0046_marketplace.sql");
    sqlx::raw_sql(migration_sql).execute(&pool).await.unwrap();

    pool
}

#[tokio::test]
async fn lifecycle_state_machine_full_flow() {
    let pool = setup_test_db().await;
    let store = MarketplaceStore::new(pool);
    let tmp = tempdir().unwrap();
    let cas = ContentAddressedStore::new(tmp.path()).unwrap();
    let verifier = PackageVerifier::new();
    let compatibility = CompatibilityChecker::new("0.9.0").unwrap();
    let lifecycle = MarketplaceLifecycleManager::new(store.clone(), cas, verifier, compatibility);

    let signing_key = SigningKey::from_bytes(&rand::random());
    let pub_bytes = signing_key.verifying_key().to_bytes();
    let pub_hex = hex::encode(pub_bytes);
    let pub_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pub_bytes);

    let trust_manager = TrustManager::new(store.clone());
    trust_manager
        .register_publisher(
            "alice",
            "Alice Corp",
            &pub_hex,
            PublisherTrustTier::Trusted,
            Some("operator"),
        )
        .await
        .unwrap();

    let mut trust_store = TrustedPublishers::default();
    trust_store.add("alice", &pub_b64).unwrap();

    let artifact = build_tar_gz(&[("main.wasm", b"\x00asm\x01\x00\x00\x00")]);
    let (manifest_toml, _) =
        build_signed_package("calc-tool", "alice", "1.0.0", "", &artifact, &signing_key);

    // 1. Install -> Always starts InstalledDisabled (inert)
    let (mut plugin, install) = lifecycle
        .install(
            &manifest_toml,
            &artifact,
            "https://cdn.trusted.io/calc-1.0.0.tar.gz",
            1000,
            &trust_store,
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        plugin.state(),
        codypendent_sandbox::LifecycleState::InstalledDisabled
    );
    assert_eq!(install.lifecycle, InstallLifecycleState::InstalledDisabled);
    assert!(!plugin.is_active());

    // Attempting to enable directly without smoke testing must fail
    assert!(lifecycle
        .enable(&mut plugin, &install.id, "workspace")
        .await
        .is_err());

    // 2. Smoke Test -> Transitions to SmokeTested
    lifecycle
        .smoke_test(&mut plugin, &install.id)
        .await
        .unwrap();
    assert_eq!(
        plugin.state(),
        codypendent_sandbox::LifecycleState::SmokeTested
    );

    let db_install = store.get_install_by_id(&install.id).await.unwrap().unwrap();
    assert_eq!(db_install.lifecycle, InstallLifecycleState::SmokeTested);

    // 3. Enable at scope -> Transitions to Enabled
    // Undeclared scope fails
    assert!(lifecycle
        .enable(&mut plugin, &install.id, "undeclared-scope")
        .await
        .is_err());

    // Declared scope succeeds
    lifecycle
        .enable(&mut plugin, &install.id, "workspace")
        .await
        .unwrap();
    assert_eq!(plugin.state(), codypendent_sandbox::LifecycleState::Enabled);
    assert!(plugin.is_active());
    assert_eq!(plugin.enabled_scope(), Some("workspace"));

    let db_install = store.get_install_by_id(&install.id).await.unwrap().unwrap();
    assert_eq!(db_install.lifecycle, InstallLifecycleState::Enabled);
    assert_eq!(db_install.enabled_scope.as_deref(), Some("workspace"));

    // 4. Disable -> the sandbox has only a terminal `revoke`, so the row records
    //    a revocation rather than claiming a reversible `disabled` state.
    lifecycle.disable(&mut plugin, &install.id).await.unwrap();
    assert!(!plugin.is_active());

    let db_install = store.get_install_by_id(&install.id).await.unwrap().unwrap();
    assert_eq!(db_install.lifecycle, InstallLifecycleState::Revoked);

    // 5. Revoke -> Transitions to Revoked
    lifecycle
        .revoke(&mut plugin, &install.id, "security violation")
        .await
        .unwrap();
    assert_eq!(plugin.state(), codypendent_sandbox::LifecycleState::Revoked);

    let db_install = store.get_install_by_id(&install.id).await.unwrap().unwrap();
    assert_eq!(db_install.lifecycle, InstallLifecycleState::Revoked);
    assert!(db_install.revoked_at.is_some());
    assert_eq!(
        db_install.revoked_reason.as_deref(),
        Some("security violation")
    );
}

#[tokio::test]
async fn update_permission_expansion_detection_and_receipt_flow() {
    let pool = setup_test_db().await;
    let store = MarketplaceStore::new(pool);
    let tmp = tempdir().unwrap();
    let cas = ContentAddressedStore::new(tmp.path()).unwrap();
    let verifier = PackageVerifier::new();
    let compatibility = CompatibilityChecker::new("0.9.0").unwrap();
    let lifecycle = MarketplaceLifecycleManager::new(store.clone(), cas, verifier, compatibility);

    let signing_key = SigningKey::from_bytes(&rand::random());
    let pub_bytes = signing_key.verifying_key().to_bytes();
    let pub_hex = hex::encode(pub_bytes);
    let pub_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pub_bytes);

    let trust_manager = TrustManager::new(store.clone());
    trust_manager
        .register_publisher("acme", "Acme", &pub_hex, PublisherTrustTier::Trusted, None)
        .await
        .unwrap();

    let mut trust_store = TrustedPublishers::default();
    trust_store.add("acme", &pub_b64).unwrap();

    let artifact_v1 = build_tar_gz(&[("main.wasm", b"\x00asm\x01\x00\x00\x00")]);
    let (v1_toml, _) = build_signed_package(
        "file-tool",
        "acme",
        "1.0.0",
        "[capabilities]\nfilesystem_read = [\"/tmp\"]",
        &artifact_v1,
        &signing_key,
    );

    let (mut plugin, install) = lifecycle
        .install(
            &v1_toml,
            &artifact_v1,
            "https://cdn.acme.com/v1.tar.gz",
            1000,
            &trust_store,
            None,
            None,
        )
        .await
        .unwrap();

    // Smoke test and enable v1
    lifecycle
        .smoke_test(&mut plugin, &install.id)
        .await
        .unwrap();
    lifecycle
        .enable(&mut plugin, &install.id, "workspace")
        .await
        .unwrap();

    // 1. Safe update (v1.0.1 with identical capabilities) -> Applies automatically without receipt
    let artifact_v1_0_1 = build_tar_gz(&[("main.wasm", b"\x00asm\x01\x00\x00\x01")]);
    let (v1_0_1_toml, _) = build_signed_package(
        "file-tool",
        "acme",
        "1.0.1",
        "[capabilities]\nfilesystem_read = [\"/tmp\"]",
        &artifact_v1_0_1,
        &signing_key,
    );

    let update_res = lifecycle
        .update(
            &mut plugin,
            &install.id,
            &v1_0_1_toml,
            &artifact_v1_0_1,
            &trust_store,
        )
        .await
        .unwrap();
    assert!(update_res.is_none());
    assert_eq!(plugin.manifest().version, "1.0.1");

    // 2. Permission-expanding update (v2.0.0 adds filesystem_write) -> Blocks with receipt
    let artifact_v2 = build_tar_gz(&[("main.wasm", b"\x00asm\x01\x00\x00\x02")]);
    let (v2_toml, _) = build_signed_package(
        "file-tool",
        "acme",
        "2.0.0",
        "[capabilities]\nfilesystem_read = [\"/tmp\"]\nfilesystem_write = [\"/data\"]",
        &artifact_v2,
        &signing_key,
    );

    let update_err = lifecycle
        .update(
            &mut plugin,
            &install.id,
            &v2_toml,
            &artifact_v2,
            &trust_store,
        )
        .await
        .unwrap_err();

    let receipt_id = match update_err {
        codypendent_marketplace::MarketplaceError::UpdateExpandsPermissions { receipt, diff } => {
            assert!(diff.contains("filesystem_write"));
            receipt
        }
        other => panic!("expected UpdateExpandsPermissions, got {other:?}"),
    };

    // Pending receipt exists in DB
    let pending_receipt = store
        .get_pending_receipt(&install.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending_receipt.id, receipt_id);
    assert_eq!(pending_receipt.decision, "pending");
    assert!(pending_receipt.expands_permissions);

    // 3. Approve-then-substitute defense: approving a different candidate manifest than reviewed fails closed
    let (tampered_v2_toml, _) = build_signed_package(
        "file-tool",
        "acme",
        "2.0.0",
        "[capabilities]\nfilesystem_read = [\"/tmp\"]\nfilesystem_write = [\"/etc\"]",
        &artifact_v2,
        &signing_key,
    );
    let sub_err = lifecycle
        .approve_update(
            &mut plugin,
            &install.id,
            &receipt_id,
            &tampered_v2_toml,
            &artifact_v2,
            "operator",
            &trust_store,
        )
        .await;
    assert!(sub_err.is_err());

    // 4. Legitimate approval succeeds
    lifecycle
        .approve_update(
            &mut plugin,
            &install.id,
            &receipt_id,
            &v2_toml,
            &artifact_v2,
            "operator",
            &trust_store,
        )
        .await
        .unwrap();

    assert_eq!(plugin.manifest().version, "2.0.0");
    let approved_receipt = store.get_receipt(&receipt_id).await.unwrap().unwrap();
    assert_eq!(approved_receipt.decision, "approved");
}

#[tokio::test]
async fn publisher_revocation_disables_installed_and_invalidates_pending() {
    let pool = setup_test_db().await;
    let store = MarketplaceStore::new(pool);
    let tmp = tempdir().unwrap();
    let cas = ContentAddressedStore::new(tmp.path()).unwrap();
    let verifier = PackageVerifier::new();
    let compatibility = CompatibilityChecker::new("0.9.0").unwrap();
    let lifecycle = MarketplaceLifecycleManager::new(store.clone(), cas, verifier, compatibility);
    let trust_manager = TrustManager::new(store.clone());

    let signing_key = SigningKey::from_bytes(&rand::random());
    let pub_bytes = signing_key.verifying_key().to_bytes();
    let pub_hex = hex::encode(pub_bytes);
    let pub_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pub_bytes);

    trust_manager
        .register_publisher(
            "malicious-pub",
            "Malicious Corp",
            &pub_hex,
            PublisherTrustTier::Trusted,
            None,
        )
        .await
        .unwrap();

    let mut trust_store = TrustedPublishers::default();
    trust_store.add("malicious-pub", &pub_b64).unwrap();

    let artifact = build_tar_gz(&[("main.wasm", b"\x00asm\x01\x00\x00\x00")]);
    let (v1_toml, _) = build_signed_package(
        "bad-app",
        "malicious-pub",
        "1.0.0",
        "",
        &artifact,
        &signing_key,
    );

    let (mut plugin, install) = lifecycle
        .install(
            &v1_toml,
            &artifact,
            "https://bad.com/pkg.tar.gz",
            1000,
            &trust_store,
            None,
            None,
        )
        .await
        .unwrap();

    lifecycle
        .smoke_test(&mut plugin, &install.id)
        .await
        .unwrap();
    lifecycle
        .enable(&mut plugin, &install.id, "workspace")
        .await
        .unwrap();

    // Trigger an expanding update to create a pending receipt. The v2 artifact
    // must differ byte-for-byte from v1: `marketplace_versions.content_hash` is
    // UNIQUE, so two versions cannot share one artifact.
    let artifact_v2 = build_tar_gz(&[("main.wasm", b"\x00asm\x01\x00\x00\x02")]);
    let (v2_toml, _) = build_signed_package(
        "bad-app",
        "malicious-pub",
        "2.0.0",
        "[capabilities]\nsubprocess = true",
        &artifact_v2,
        &signing_key,
    );
    let update_err = lifecycle
        .update(
            &mut plugin,
            &install.id,
            &v2_toml,
            &artifact_v2,
            &trust_store,
        )
        .await
        .unwrap_err();
    let receipt_id = match update_err {
        codypendent_marketplace::MarketplaceError::UpdateExpandsPermissions { receipt, .. } => {
            receipt
        }
        other => panic!("expected UpdateExpandsPermissions, got {other:?}"),
    };

    // Revoke publisher
    trust_manager
        .revoke_publisher("malicious-pub", "compromised private key", "operator")
        .await
        .unwrap();

    // 1. Publisher is marked revoked in DB
    let pub_record = store.get_publisher("malicious-pub").await.unwrap().unwrap();
    assert!(pub_record.revoked_at.is_some());

    // 2. Installed package is retroactively marked revoked in DB
    let install_record = store.get_install_by_id(&install.id).await.unwrap().unwrap();
    assert_eq!(install_record.lifecycle, InstallLifecycleState::Revoked);

    // 3. Pending receipt is invalidated
    let receipt_record = store.get_receipt(&receipt_id).await.unwrap().unwrap();
    assert!(receipt_record.invalidated_at.is_some());

    // 4. Attempting to approve the invalidated receipt fails
    let app_err = lifecycle
        .approve_update(
            &mut plugin,
            &install.id,
            &receipt_id,
            &v2_toml,
            &artifact_v2,
            "operator",
            &trust_store,
        )
        .await;
    assert!(app_err.is_err());

    // 5. Attempting to enable the revoked package fails
    let en_err = lifecycle
        .enable(&mut plugin, &install.id, "workspace")
        .await;
    assert!(en_err.is_err());
}

/// The database projection and the sandbox — the execution authority — must
/// agree after a disable, and must keep agreeing when someone tries to undo it.
///
/// The sandbox has no reversible disable: `InstalledPlugin::revoke` is its only
/// transition to an inert state and it is terminal. So a disable that wrote
/// `lifecycle = 'disabled'` would leave the row advertising a re-enable the
/// sandbox will never honour. The row must record the revocation that actually
/// happened, and the re-enable must fail in BOTH representations.
#[tokio::test]
async fn disable_leaves_the_database_and_the_sandbox_in_agreement() {
    let pool = setup_test_db().await;
    let store = MarketplaceStore::new(pool);
    let tmp = tempdir().unwrap();
    let cas = ContentAddressedStore::new(tmp.path()).unwrap();
    let verifier = PackageVerifier::new();
    let compatibility = CompatibilityChecker::new("0.9.0").unwrap();
    let lifecycle = MarketplaceLifecycleManager::new(store.clone(), cas, verifier, compatibility);

    let signing_key = SigningKey::from_bytes(&rand::random());
    let pub_bytes = signing_key.verifying_key().to_bytes();
    let pub_hex = hex::encode(pub_bytes);
    let pub_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pub_bytes);

    TrustManager::new(store.clone())
        .register_publisher("acme", "Acme", &pub_hex, PublisherTrustTier::Trusted, None)
        .await
        .unwrap();
    let mut trust_store = TrustedPublishers::default();
    trust_store.add("acme", &pub_b64).unwrap();

    let artifact = build_tar_gz(&[("main.wasm", b"\x00asm\x01\x00\x00\x00")]);
    let (toml, _) =
        build_signed_package("toggle-tool", "acme", "1.0.0", "", &artifact, &signing_key);

    let (mut plugin, install) = lifecycle
        .install(
            &toml,
            &artifact,
            "https://cdn.acme.com/toggle-1.0.0.tar.gz",
            1000,
            &trust_store,
            None,
            None,
        )
        .await
        .unwrap();
    lifecycle
        .smoke_test(&mut plugin, &install.id)
        .await
        .unwrap();
    lifecycle
        .enable(&mut plugin, &install.id, "workspace")
        .await
        .unwrap();

    lifecycle.disable(&mut plugin, &install.id).await.unwrap();

    // The sandbox is inert and terminal...
    assert_eq!(plugin.state(), codypendent_sandbox::LifecycleState::Revoked);
    assert!(!plugin.is_active());
    assert_eq!(plugin.enabled_scope(), None);

    // ...and the row says exactly that, scope cleared, with a recorded reason.
    let db_install = store.get_install_by_id(&install.id).await.unwrap().unwrap();
    assert_eq!(db_install.lifecycle, InstallLifecycleState::Revoked);
    assert_eq!(db_install.enabled_scope, None);
    assert!(
        db_install.revoked_at.is_some(),
        "a disable the sandbox cannot undo must be recorded as irreversible"
    );
    assert!(db_install.revoked_reason.is_some());

    // Re-enabling fails at the marketplace, at the sandbox, and leaves the row
    // untouched — no path makes one representation claim more than the other.
    let err = lifecycle
        .enable(&mut plugin, &install.id, "workspace")
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            codypendent_marketplace::MarketplaceError::RevokedPackage { .. }
        ),
        "expected a revoked-package refusal, got {err:?}"
    );
    assert!(
        plugin.enable("workspace").is_err(),
        "the sandbox refuses to re-enable a revoked plugin"
    );
    assert_eq!(plugin.state(), codypendent_sandbox::LifecycleState::Revoked);

    let after = store.get_install_by_id(&install.id).await.unwrap().unwrap();
    assert_eq!(after.lifecycle, InstallLifecycleState::Revoked);
    assert_eq!(after.enabled_scope, None);
    assert!(!plugin.is_active());
}

/// The sandbox — not a second, independent evaluator — decides whether an update
/// expands authority.
///
/// A capability withheld at install makes the granted set narrower than the
/// manifest's request, so a byte-identical capability list is still an expansion
/// relative to the GRANT. A manifest-to-manifest diff calls that "identical": on
/// the old code the update took the "safe" path, the sandbox blocked it and
/// parked the plugin in `UpdateBlocked` (inert, and unable to update or be
/// approved again), while the row still read `enabled` at the old version with
/// no receipt for anyone to decide.
#[tokio::test]
async fn an_update_the_sandbox_blocks_is_never_recorded_as_applied() {
    let pool = setup_test_db().await;
    let store = MarketplaceStore::new(pool);
    let tmp = tempdir().unwrap();
    let cas = ContentAddressedStore::new(tmp.path()).unwrap();
    let verifier = PackageVerifier::new();
    let compatibility = CompatibilityChecker::new("0.9.0").unwrap();
    let lifecycle = MarketplaceLifecycleManager::new(store.clone(), cas, verifier, compatibility);

    let signing_key = SigningKey::from_bytes(&rand::random());
    let pub_bytes = signing_key.verifying_key().to_bytes();
    let pub_hex = hex::encode(pub_bytes);
    let pub_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pub_bytes);

    TrustManager::new(store.clone())
        .register_publisher("acme", "Acme", &pub_hex, PublisherTrustTier::Trusted, None)
        .await
        .unwrap();
    let mut trust_store = TrustedPublishers::default();
    trust_store.add("acme", &pub_b64).unwrap();

    let artifact_v1 = build_tar_gz(&[("main.wasm", b"\x00asm\x01\x00\x00\x00")]);
    let (v1_toml, _) = build_signed_package(
        "withheld-tool",
        "acme",
        "1.0.0",
        "[capabilities]\nfilesystem_read = [\"/tmp\"]",
        &artifact_v1,
        &signing_key,
    );

    // Install with the filesystem read WITHHELD (an empty grant narrows the
    // manifest, which the sandbox allows).
    let (mut plugin, install) = lifecycle
        .install(
            &v1_toml,
            &artifact_v1,
            "https://cdn.acme.com/withheld-1.0.0.tar.gz",
            1000,
            &trust_store,
            Some(codypendent_sandbox::CapabilitySet::default()),
            None,
        )
        .await
        .unwrap();
    lifecycle
        .smoke_test(&mut plugin, &install.id)
        .await
        .unwrap();
    lifecycle
        .enable(&mut plugin, &install.id, "workspace")
        .await
        .unwrap();

    // Same capability list as v1.0.0 — identical manifest-to-manifest, but an
    // expansion against the grant the user actually approved.
    let artifact_v1_0_1 = build_tar_gz(&[("main.wasm", b"\x00asm\x01\x00\x00\x01")]);
    let (v1_0_1_toml, _) = build_signed_package(
        "withheld-tool",
        "acme",
        "1.0.1",
        "[capabilities]\nfilesystem_read = [\"/tmp\"]",
        &artifact_v1_0_1,
        &signing_key,
    );

    let err = lifecycle
        .update(
            &mut plugin,
            &install.id,
            &v1_0_1_toml,
            &artifact_v1_0_1,
            &trust_store,
        )
        .await
        .unwrap_err();

    let receipt_id = match err {
        codypendent_marketplace::MarketplaceError::UpdateExpandsPermissions { receipt, diff } => {
            assert!(diff.contains("filesystem_read"), "diff was: {diff}");
            receipt
        }
        other => panic!("expected UpdateExpandsPermissions, got {other:?}"),
    };

    // A receipt exists for a human to decide...
    let pending = store
        .get_pending_receipt(&install.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.id, receipt_id);
    assert!(pending.expands_permissions);

    // ...the sandbox was never pushed into `UpdateBlocked` behind the row's
    // back, and the row still describes the version that is actually installed.
    assert_eq!(plugin.state(), codypendent_sandbox::LifecycleState::Enabled);
    assert_eq!(plugin.manifest().version, "1.0.0");
    let db_install = store.get_install_by_id(&install.id).await.unwrap().unwrap();
    assert_eq!(db_install.lifecycle, InstallLifecycleState::Enabled);
    assert_eq!(db_install.version_id, install.version_id);

    // The reviewed expansion still approves cleanly through the sandbox.
    lifecycle
        .approve_update(
            &mut plugin,
            &install.id,
            &receipt_id,
            &v1_0_1_toml,
            &artifact_v1_0_1,
            "operator",
            &trust_store,
        )
        .await
        .unwrap();
    assert_eq!(plugin.manifest().version, "1.0.1");
    assert_eq!(plugin.state(), codypendent_sandbox::LifecycleState::Enabled);

    let db_install = store.get_install_by_id(&install.id).await.unwrap().unwrap();
    assert_eq!(db_install.lifecycle, InstallLifecycleState::Enabled);
    assert_eq!(db_install.enabled_scope.as_deref(), plugin.enabled_scope());
}

#[tokio::test]
async fn hidden_package_non_disclosure() {
    let pool = setup_test_db().await;
    let store = MarketplaceStore::new(pool);
    let catalog = MarketplaceCatalog::new(store.clone());

    let signing_key = SigningKey::from_bytes(&rand::random());
    let pub_bytes = signing_key.verifying_key().to_bytes();
    let pub_hex = hex::encode(pub_bytes);

    let trust_manager = TrustManager::new(store.clone());
    trust_manager
        .register_publisher("corp", "Corp", &pub_hex, PublisherTrustTier::Trusted, None)
        .await
        .unwrap();

    let now = chrono::Utc::now().to_rfc3339();

    // Create public package
    let public_pkg = codypendent_marketplace::MarketplacePackage {
        id: "public-tools".to_string(),
        publisher_id: "corp".to_string(),
        kind: "wasm-component".to_string(),
        display_name: "Public Tools".to_string(),
        summary: "Public tools for all".to_string(),
        hidden: false,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    store.create_package(&public_pkg).await.unwrap();

    // Create hidden package
    let hidden_pkg = codypendent_marketplace::MarketplacePackage {
        id: "confidential-sec-tools".to_string(),
        publisher_id: "corp".to_string(),
        kind: "wasm-component".to_string(),
        display_name: "Confidential Security Tools".to_string(),
        summary: "Internal only".to_string(),
        hidden: true,
        created_at: now.clone(),
        updated_at: now,
    };
    store.create_package(&hidden_pkg).await.unwrap();

    // 1. Standard discover only returns public packages
    let discovered = catalog.discover(None, false).await.unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].id, "public-tools");

    // 2. Discover with query for hidden package returns empty
    let search_res = catalog.discover(Some("confidential"), false).await.unwrap();
    assert!(search_res.is_empty());

    // 3. Inspect hidden package without include_hidden returns PackageNotFound,
    //    identical to inspecting a non-existent package
    let hidden_err = catalog
        .inspect("confidential-sec-tools", false)
        .await
        .unwrap_err();
    let nonexistent_err = catalog.inspect("does-not-exist", false).await.unwrap_err();

    assert_eq!(
        hidden_err.to_string(),
        "package `confidential-sec-tools` not found"
    );
    assert_eq!(
        nonexistent_err.to_string(),
        "package `does-not-exist` not found"
    );
    assert!(matches!(
        hidden_err,
        codypendent_marketplace::MarketplaceError::PackageNotFound(_)
    ));
    assert!(matches!(
        nonexistent_err,
        codypendent_marketplace::MarketplaceError::PackageNotFound(_)
    ));

    // 4. Authorized query (include_hidden = true) can inspect
    let (inspected, _) = catalog
        .inspect("confidential-sec-tools", true)
        .await
        .unwrap();
    assert_eq!(inspected.id, "confidential-sec-tools");
}
