//! Backend contract integration tests.
//!
//! The contract every backend in this crate is held to: it either does the
//! thing it is named after, or it refuses with a typed dotted code. No backend
//! is allowed to simulate a lookup, fall back to a different source, or report
//! "not found" for a store it never consulted.

use codypendent_secrets::backends::{
    EnvironmentBackend, KeychainBackend, ManagedBackend, VaultBackend, WorkloadIdentityBackend,
};
use codypendent_secrets::{LeaseContext, SecretBackend, SecretBackendKind, SecretError};

/// A KEK / seed that is not a repeated constant, as the constructors require.
fn varied_key(salt: u8) -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31).wrapping_add(salt);
    }
    k
}

#[tokio::test]
async fn environment_contract() {
    let backend = EnvironmentBackend::new();
    assert_eq!(backend.kind(), SecretBackendKind::Environment);

    std::env::set_var("TEST_ENV_BACKEND_VAR", "env_secret_value_123");
    let ctx = LeaseContext::new(1000, "job-env", "env.cap");

    let res = backend
        .resolve("env:TEST_ENV_BACKEND_VAR", &ctx)
        .await
        .expect("resolve env:");
    assert_eq!(res.expose_str().unwrap(), "env_secret_value_123");

    let res2 = backend
        .resolve("TEST_ENV_BACKEND_VAR", &ctx)
        .await
        .expect("resolve raw name");
    assert_eq!(res2.expose_str().unwrap(), "env_secret_value_123");

    let err = backend
        .resolve("NONEXISTENT_ENV_VAR_XYZ_999", &ctx)
        .await
        .expect_err("missing env var must fail");
    assert_eq!(err.outcome_code(), "secrets.env-missing");
}

/// The environment backend's error text is the only one that could plausibly
/// have interpolated a value. Prove it interpolates nothing at all.
#[tokio::test]
async fn environment_error_text_contains_neither_value_nor_locator() {
    let backend = EnvironmentBackend::new();
    let ctx = LeaseContext::new(1000, "job-env", "env.cap");
    std::env::set_var("TEST_ENV_BLANK_VAR", "   ");

    let err = backend
        .resolve("TEST_ENV_BLANK_VAR", &ctx)
        .await
        .expect_err("blank env var must fail");
    let rendered = err.to_string();
    assert!(!rendered.contains("TEST_ENV_BLANK_VAR"));
    assert_eq!(err.outcome_code(), "secrets.env-missing");
}

#[tokio::test]
async fn keychain_contract() {
    let backend = KeychainBackend::for_current_platform();
    assert_eq!(backend.kind(), SecretBackendKind::Keychain);
    let ctx = LeaseContext::new(1000, "job-kc", "kc.cap");

    // There is no keychain client in this build. Whatever the platform, the
    // answer is a refusal — never "the item is not in the keychain", which
    // would be a claim about a store nothing consulted.
    let err = backend
        .resolve("codypendent/github", &ctx)
        .await
        .expect_err("keychain must return a typed refusal");
    let code = err.outcome_code();
    assert!(
        code == "secrets.backend-not-configured" || code == "secrets.keychain-unsupported",
        "unexpected keychain code {code}"
    );
    assert_ne!(code, "secrets.backend-secret-not-found");
}

#[tokio::test]
async fn managed_contract() {
    let backend = ManagedBackend::with_master_key(varied_key(7)).expect("keyed backend");
    assert_eq!(backend.kind(), SecretBackendKind::Managed);
    assert!(backend.is_configured());
    let ctx = LeaseContext::new(1000, "job-mng", "mng.cap");

    backend
        .provision("db_password", b"super_secure_db_pass")
        .expect("seal");

    let res = backend
        .resolve("db_password", &ctx)
        .await
        .expect("resolve managed");
    assert_eq!(res.expose(), b"super_secure_db_pass");

    let err = backend
        .resolve("unprovisioned_key", &ctx)
        .await
        .expect_err("missing managed key");
    assert!(matches!(err, SecretError::BackendError { .. }));
    assert_eq!(err.outcome_code(), "secrets.backend-secret-not-found");
}

#[tokio::test]
async fn managed_refuses_without_a_key_rather_than_faking_encryption() {
    let backend = ManagedBackend::unconfigured();
    let ctx = LeaseContext::new(1000, "job-mng", "mng.cap");

    let store_err = backend
        .provision("db_password", b"super_secure_db_pass")
        .expect_err("storing under no key must be refused");
    assert_eq!(store_err.outcome_code(), "secrets.backend-not-configured");

    let read_err = backend
        .resolve("db_password", &ctx)
        .await
        .expect_err("reading from an unconfigured backend must be refused");
    assert_eq!(read_err.outcome_code(), "secrets.backend-not-configured");

    // And a placeholder key cannot be used to get around it.
    let key_err = ManagedBackend::with_master_key([0x42; 32])
        .expect_err("a repeated-constant KEK must be refused");
    assert_eq!(key_err.outcome_code(), "secrets.invalid-key-material");
}

#[tokio::test]
async fn vault_contract() {
    let backend = VaultBackend::new();
    assert_eq!(backend.kind(), SecretBackendKind::Vault);
    assert!(!backend.is_configured());
    let ctx = LeaseContext::new(1000, "job-vault", "vault.cap");

    // No Vault client exists. Refuse; do not claim a miss and do not claim a
    // successful revocation.
    let err = backend
        .resolve("secret/data/codypendent/api_key", &ctx)
        .await
        .expect_err("vault must refuse");
    assert_eq!(err.outcome_code(), "secrets.backend-not-configured");

    let revoke_err = backend
        .revoke("vault-lease-id-123")
        .await
        .expect_err("revocation against no client must not report success");
    assert_eq!(revoke_err.outcome_code(), "secrets.backend-not-configured");
}

#[tokio::test]
async fn vault_outage_denies_rather_than_falling_back() {
    let backend = VaultBackend::new();
    let ctx = LeaseContext::new(1000, "job-vault-outage", "vault.cap");

    // "Outage" is now permanent rather than simulated by a boolean: there is no
    // client, so Vault is always unreachable. Either way the assertion is the
    // one that matters — an unreachable Vault must never resolve to material
    // from some other source.
    let err = backend
        .resolve("secret/data/codypendent/api_key", &ctx)
        .await
        .expect_err("must fail closed");
    assert!(matches!(err, SecretError::BackendError { .. }));
    assert_eq!(err.outcome_code(), "secrets.backend-not-configured");
}

#[tokio::test]
async fn workload_identity_contract() {
    let backend = WorkloadIdentityBackend::with_signing_seed(varied_key(3)).expect("seeded");
    assert_eq!(backend.kind(), SecretBackendKind::WorkloadIdentity);
    let ctx = LeaseContext::new(1000, "job-wit", "wit.cap");

    let res = backend
        .resolve("api://codypendent-service", &ctx)
        .await
        .expect("resolve workload token");
    let token = res.expose_str().unwrap();
    assert!(token.starts_with("wit_"));

    // Unseeded refuses rather than minting from a default seed.
    let unconfigured = WorkloadIdentityBackend::unconfigured();
    let err = unconfigured
        .resolve("api://codypendent-service", &ctx)
        .await
        .expect_err("an unseeded backend must refuse");
    assert_eq!(err.outcome_code(), "secrets.backend-not-configured");
}

/// No backend's `Debug` may render key material, and no backend's error may
/// render a value.
#[tokio::test]
async fn no_backend_debug_or_error_leaks_material() {
    let sentinel = "SENTINEL_BACKEND_MATERIAL_NEVER_RENDERED_777";
    let ctx = LeaseContext::new(1000, "job-leak", "leak.cap");

    let managed = ManagedBackend::with_master_key(varied_key(9)).expect("keyed");
    managed.provision("k", sentinel.as_bytes()).expect("seal");
    let leased = managed.resolve("k", &ctx).await.expect("open");

    let renders = [
        format!("{managed:?}"),
        format!("{:?}", VaultBackend::new()),
        format!("{:?}", KeychainBackend::for_current_platform()),
        format!("{:?}", EnvironmentBackend::new()),
        format!(
            "{:?}",
            WorkloadIdentityBackend::with_signing_seed(varied_key(4)).expect("seeded")
        ),
        format!("{leased:?}"),
    ];
    for rendered in renders {
        assert!(
            !rendered.contains(sentinel),
            "material leaked into Debug: {rendered}"
        );
    }
    assert_eq!(leased.expose_str().unwrap(), sentinel);
}
