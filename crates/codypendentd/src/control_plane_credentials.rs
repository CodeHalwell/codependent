//! Secure persistence and startup rehydration for control-plane workload tokens.
//!
//! The pairing tables store only a reference and verification hash. Secret
//! material is kept in the existing owner-only, atomically written `AuthStore`.
//! `keychain:` references remain fail-closed until the repository's keychain
//! backend gains a real platform client; they are never treated as an AuthStore
//! key or silently downgraded to an environment variable.

use std::path::Path;

use chrono::Utc;
use codypendent_daemon::control_plane_sync::{get_credential, list_active_pairings, SyncEngine};
use codypendent_runtime::auth::{AuthStore, Save};
use sha2::{Digest, Sha256};

const AUTH_STORE_SCHEME: &str = "auth-store:";
const AUTH_STORE_KEY_PREFIX: &str = "control-plane/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedControlPlaneCredential {
    pub credential_ref: String,
    pub credential_hash: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CredentialRehydration {
    pub loaded: usize,
    pub unavailable: usize,
}

fn auth_store_key(pairing_id: &str) -> String {
    format!("{AUTH_STORE_KEY_PREFIX}{pairing_id}")
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Persist an opaque bearer/workload token and return only non-secret metadata
/// suitable for `control_plane_credentials`.
pub fn persist_control_plane_token(
    data_dir: &Path,
    pairing_id: &str,
    token: &str,
) -> std::io::Result<PersistedControlPlaneCredential> {
    if token.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to persist an empty control-plane credential",
        ));
    }
    let key = auth_store_key(pairing_id);
    // Under the shared hold: every writer of `auth.json` renames a whole map
    // back, so an unserialized pair loses one side's entry.
    codypendent_runtime::auth::update(data_dir, |auth| -> std::io::Result<_> {
        auth.set(&key, token);
        Ok((Save::Yes, ()))
    })?;
    Ok(PersistedControlPlaneCredential {
        credential_ref: format!("{AUTH_STORE_SCHEME}{key}"),
        credential_hash: token_hash(token),
    })
}

/// Remove a persisted token after local revocation or a failed pairing commit.
pub fn remove_control_plane_token(data_dir: &Path, pairing_id: &str) -> std::io::Result<bool> {
    codypendent_runtime::auth::update(data_dir, |auth| -> std::io::Result<_> {
        let removed = auth.remove(&auth_store_key(pairing_id));
        Ok((if removed { Save::Yes } else { Save::No }, removed))
    })
}

/// Load active pairing credentials into the process-local engine cache.
/// Missing, expired, purpose/audience-mismatched, keychain-only, or hash-
/// mismatched credentials are counted but never handed to the HTTP client.
pub async fn rehydrate_control_plane_credentials(
    pool: &sqlx::SqlitePool,
    data_dir: &Path,
    engine: &SyncEngine,
) -> anyhow::Result<CredentialRehydration> {
    let auth = AuthStore::load(data_dir)?;
    let pairings = list_active_pairings(pool).await?;
    let mut report = CredentialRehydration::default();

    for pairing in pairings {
        let Some(credential) = get_credential(pool, &pairing.id).await? else {
            report.unavailable += 1;
            continue;
        };
        let valid_metadata = credential.expires_at > Utc::now()
            && credential.purpose == "sync"
            && (credential.audience == "control-plane" || credential.audience == pairing.endpoint);
        if !valid_metadata {
            report.unavailable += 1;
            continue;
        }
        let Some(key) = credential.credential_ref.strip_prefix(AUTH_STORE_SCHEME) else {
            // In particular, never reinterpret an unresolved `keychain:`
            // locator as a plaintext AuthStore key.
            report.unavailable += 1;
            continue;
        };
        let Some(token) = auth.get(key) else {
            report.unavailable += 1;
            continue;
        };
        if token_hash(token) != credential.credential_hash {
            report.unavailable += 1;
            continue;
        }
        engine.set_pairing_token(&pairing.id, token).await;
        report.loaded += 1;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_tokens_round_trip_without_entering_metadata_or_debug() {
        let dir = tempfile::tempdir().expect("tempdir");
        let secret = "cp_daemon_secret-value";
        let metadata = persist_control_plane_token(dir.path(), "pairing-a", secret)
            .expect("persist control-plane token");

        assert!(!metadata.credential_ref.contains(secret));
        assert!(!metadata.credential_hash.contains(secret));
        assert!(!format!("{metadata:?}").contains(secret));
        let auth = AuthStore::load(dir.path()).expect("reload auth store");
        assert_eq!(
            auth.get("control-plane/pairing-a"),
            Some(secret),
            "the owner-only secret store retains the opaque token"
        );
        assert!(remove_control_plane_token(dir.path(), "pairing-a").expect("remove token"));
        assert_eq!(
            AuthStore::load(dir.path())
                .expect("reload after remove")
                .get("control-plane/pairing-a"),
            None
        );
    }
}
