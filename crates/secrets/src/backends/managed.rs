//! Managed envelope-encrypted secret backend.
//!
//! # What this actually does
//!
//! Real envelope encryption with an authenticated cipher:
//!
//! 1. A fresh 32-byte **data encryption key** (DEK) is drawn from the OS CSPRNG
//!    for every single record — including two provisions of the same locator.
//! 2. The plaintext is sealed with `ChaCha20-Poly1305` under that DEK, with a
//!    fresh random 96-bit nonce and the locator bound in as associated data.
//! 3. The DEK is itself sealed with `ChaCha20-Poly1305` under the operator's
//!    **key encryption key** (KEK), again with a fresh random nonce and the same
//!    associated data, and the wrapped DEK is stored alongside the ciphertext.
//!
//! Consequences that the previous XOR-against-a-repeating-SHA-256-pad did not
//! have, and that [`tests`] assert:
//!
//! - Sealing the same plaintext twice produces different bytes (fresh DEK +
//!   fresh nonces), so equal values are not detectable by comparing records.
//! - Flipping any bit of a record makes it fail to open: Poly1305 authenticates
//!   the whole thing, so there is no malleability.
//! - Moving a record to a different locator makes it fail to open: the locator
//!   is the associated data on both layers.
//!
//! # What this does not do
//!
//! The KEK lives in this process's memory. That is what "managed" means here:
//! an in-process envelope store, not a KMS. There is no persistence and no
//! remote unwrap. A [`ManagedBackend`] built without a KEK
//! ([`ManagedBackend::unconfigured`], which is also its `Default`) **refuses
//! every operation** rather than storing anything under a guarantee it cannot
//! keep.

use std::collections::HashMap;
use std::fmt;
use std::sync::RwLock;

use async_trait::async_trait;
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use zeroize::{Zeroize, Zeroizing};

use crate::backend::{SecretBackend, SecretBackendKind};
use crate::lease::{LeaseContext, LeasedSecret};
use crate::{BackendErrorCode, SecretError};

/// Record magic + version. Bumping this rejects every older record rather than
/// misparsing one.
const MAGIC: &[u8] = b"cdp-menv1";
/// `ChaCha20-Poly1305` nonce width.
const NONCE_LEN: usize = 12;
/// Poly1305 tag width.
const TAG_LEN: usize = 16;
/// DEK width.
const DEK_LEN: usize = 32;
/// A wrapped DEK is the DEK plus its tag.
const WRAPPED_DEK_LEN: usize = DEK_LEN + TAG_LEN;
/// Fixed prefix before the payload ciphertext.
const HEADER_LEN: usize = MAGIC.len() + NONCE_LEN + WRAPPED_DEK_LEN + NONCE_LEN;

/// Domain separator so a record from this backend cannot be replayed into any
/// other `ChaCha20-Poly1305` use in the codebase.
const AAD_DOMAIN: &[u8] = b"codypendent-managed-envelope-v1\x00";

/// The operator's key encryption key.
///
/// Zeroized on drop; never `Clone`, never `Debug`-printable, never serialized.
struct Kek([u8; DEK_LEN]);

impl Drop for Kek {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Kek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Kek(\"<redacted>\")")
    }
}

struct ConfiguredVault {
    kek: Kek,
    /// Locator -> sealed record. Only ciphertext lives here.
    records: RwLock<HashMap<String, Vec<u8>>>,
}

/// Secret backend that seals managed credential values under an operator KEK.
///
/// Construct it with [`ManagedBackend::with_master_key`] to get a working
/// envelope store, or [`ManagedBackend::unconfigured`] to get a backend that
/// refuses. There is no third state and no default key: a backend that has not
/// been handed key material cannot be talked into storing a secret.
pub struct ManagedBackend {
    vault: Option<ConfiguredVault>,
}

impl fmt::Debug for ManagedBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManagedBackend")
            .field("configured", &self.vault.is_some())
            .finish()
    }
}

/// Fail closed: a `ManagedBackend` you did not deliberately key refuses.
impl Default for ManagedBackend {
    fn default() -> Self {
        Self::unconfigured()
    }
}

impl ManagedBackend {
    /// A managed backend with no key material. Every operation refuses with
    /// [`BackendErrorCode::NotConfigured`].
    #[must_use]
    pub fn unconfigured() -> Self {
        Self { vault: None }
    }

    /// A managed backend keyed with an operator-supplied 32-byte KEK.
    ///
    /// # Errors
    ///
    /// Refuses key material that is a single repeated byte. That is not a
    /// statistical test — it is a footgun guard, because placeholder keys in
    /// this codebase's history looked exactly like `[0x42; 32]`, and a
    /// hardcoded KEK makes the envelope decorative.
    pub fn with_master_key(master_key: [u8; DEK_LEN]) -> Result<Self, SecretError> {
        let first = master_key[0];
        if master_key.iter().all(|b| *b == first) {
            // Do not leak the rejected bytes anywhere, including here.
            let mut rejected = master_key;
            rejected.zeroize();
            return Err(SecretError::backend(
                BackendErrorCode::InvalidKeyMaterial,
                "managed KEK must be 32 random bytes, not a repeated constant",
            ));
        }
        Ok(Self {
            vault: Some(ConfiguredVault {
                kek: Kek(master_key),
                records: RwLock::new(HashMap::new()),
            }),
        })
    }

    /// Whether this backend has key material and can store or open records.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.vault.is_some()
    }

    /// Seal a value into the managed store.
    ///
    /// # Errors
    ///
    /// Refuses when the backend is unconfigured, so a caller cannot hand a
    /// secret to a backend that would not encrypt it.
    pub fn provision(
        &self,
        locator: impl Into<String>,
        plaintext: &[u8],
    ) -> Result<(), SecretError> {
        let vault = self.vault()?;
        let locator = locator.into();
        let record = seal(&vault.kek, &locator, plaintext)?;
        let mut store = vault.records.write().map_err(|_| poisoned())?;
        store.insert(locator, record);
        Ok(())
    }

    /// Seal already-leased material without it ever becoming a loose `Vec`.
    ///
    /// # Errors
    ///
    /// As [`ManagedBackend::provision`].
    pub fn provision_leased(
        &self,
        locator: impl Into<String>,
        secret: &LeasedSecret,
    ) -> Result<(), SecretError> {
        self.provision(locator, secret.expose())
    }

    fn vault(&self) -> Result<&ConfiguredVault, SecretError> {
        self.vault.as_ref().ok_or_else(|| {
            SecretError::backend(
                BackendErrorCode::NotConfigured,
                "the managed backend has no key encryption key; it refuses to store or return material",
            )
        })
    }
}

fn poisoned() -> SecretError {
    SecretError::backend(BackendErrorCode::Internal, "managed keystore lock poisoned")
}

fn aad(locator: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_DOMAIN.len() + locator.len());
    out.extend_from_slice(AAD_DOMAIN);
    out.extend_from_slice(locator.as_bytes());
    out
}

/// Envelope-seal `plaintext` under `kek`, binding `locator` as associated data.
fn seal(kek: &Kek, locator: &str, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
    let associated = aad(locator);

    // Fresh DEK per record: two seals of the same plaintext never collide.
    let mut dek = Zeroizing::new([0u8; DEK_LEN]);
    OsRng.fill_bytes(&mut dek[..]);
    let data_cipher = ChaCha20Poly1305::new(Key::from_slice(&dek[..]));
    let data_nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = data_cipher
        .encrypt(
            &data_nonce,
            Payload {
                msg: plaintext,
                aad: &associated,
            },
        )
        .map_err(|_| SecretError::backend(BackendErrorCode::Internal, "managed seal failed"))?;

    let kek_cipher = ChaCha20Poly1305::new(Key::from_slice(&kek.0));
    let wrap_nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let wrapped_dek = kek_cipher
        .encrypt(
            &wrap_nonce,
            Payload {
                msg: &dek[..],
                aad: &associated,
            },
        )
        .map_err(|_| SecretError::backend(BackendErrorCode::Internal, "managed key wrap failed"))?;

    debug_assert_eq!(wrapped_dek.len(), WRAPPED_DEK_LEN);

    let mut record = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    record.extend_from_slice(MAGIC);
    record.extend_from_slice(&wrap_nonce);
    record.extend_from_slice(&wrapped_dek);
    record.extend_from_slice(&data_nonce);
    record.extend_from_slice(&ciphertext);
    Ok(record)
}

/// Open a sealed record. Every failure mode collapses to one code so the error
/// cannot be used as an oracle.
fn open(kek: &Kek, locator: &str, record: &[u8]) -> Result<Vec<u8>, SecretError> {
    let corrupt = || {
        SecretError::backend(
            BackendErrorCode::DecryptFailed,
            "managed record failed authentication",
        )
    };

    if record.len() < HEADER_LEN + TAG_LEN || !record.starts_with(MAGIC) {
        return Err(corrupt());
    }
    let associated = aad(locator);

    let mut cursor = MAGIC.len();
    let wrap_nonce = &record[cursor..cursor + NONCE_LEN];
    cursor += NONCE_LEN;
    let wrapped_dek = &record[cursor..cursor + WRAPPED_DEK_LEN];
    cursor += WRAPPED_DEK_LEN;
    let data_nonce = &record[cursor..cursor + NONCE_LEN];
    cursor += NONCE_LEN;
    let ciphertext = &record[cursor..];

    let kek_cipher = ChaCha20Poly1305::new(Key::from_slice(&kek.0));
    let dek_bytes = Zeroizing::new(
        kek_cipher
            .decrypt(
                Nonce::from_slice(wrap_nonce),
                Payload {
                    msg: wrapped_dek,
                    aad: &associated,
                },
            )
            .map_err(|_| corrupt())?,
    );
    if dek_bytes.len() != DEK_LEN {
        return Err(corrupt());
    }

    let data_cipher = ChaCha20Poly1305::new(Key::from_slice(dek_bytes.as_slice()));
    data_cipher
        .decrypt(
            Nonce::from_slice(data_nonce),
            Payload {
                msg: ciphertext,
                aad: &associated,
            },
        )
        .map_err(|_| corrupt())
}

#[async_trait]
impl SecretBackend for ManagedBackend {
    fn kind(&self) -> SecretBackendKind {
        SecretBackendKind::Managed
    }

    async fn resolve(
        &self,
        locator: &str,
        _context: &LeaseContext,
    ) -> Result<LeasedSecret, SecretError> {
        let vault = self.vault()?;
        let record = {
            let store = vault.records.read().map_err(|_| poisoned())?;
            match store.get(locator) {
                Some(record) => record.clone(),
                None => {
                    return Err(SecretError::backend(
                        BackendErrorCode::SecretNotFound,
                        "no managed record under this locator",
                    ))
                }
            }
        };
        let mut plaintext = open(&vault.kek, locator, &record)?;
        let leased = LeasedSecret::from_bytes(&plaintext);
        plaintext.zeroize();
        Ok(leased)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; DEK_LEN] {
        let mut k = [0u8; DEK_LEN];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(19);
        }
        k
    }

    fn backend() -> ManagedBackend {
        ManagedBackend::with_master_key(key()).expect("varied key accepted")
    }

    fn record_for(backend: &ManagedBackend, locator: &str) -> Vec<u8> {
        backend
            .vault
            .as_ref()
            .expect("configured")
            .records
            .read()
            .expect("lock")
            .get(locator)
            .cloned()
            .expect("record present")
    }

    #[test]
    fn a_repeated_constant_kek_is_refused() {
        let err = ManagedBackend::with_master_key([0x42; DEK_LEN])
            .expect_err("placeholder key must be refused");
        assert_eq!(err.outcome_code(), "secrets.invalid-key-material");
        assert!(ManagedBackend::with_master_key([0x00; DEK_LEN]).is_err());
    }

    #[test]
    fn default_is_unconfigured_and_refuses_to_store() {
        let backend = ManagedBackend::default();
        assert!(!backend.is_configured());
        let err = backend
            .provision("db_password", b"hunter2")
            .expect_err("an unconfigured backend must refuse to store");
        assert_eq!(err.outcome_code(), "secrets.backend-not-configured");
    }

    #[tokio::test]
    async fn unconfigured_refuses_to_resolve() {
        let backend = ManagedBackend::unconfigured();
        let ctx = LeaseContext::new(1000, "job", "cap");
        let err = backend
            .resolve("db_password", &ctx)
            .await
            .expect_err("an unconfigured backend must refuse to resolve");
        assert_eq!(err.outcome_code(), "secrets.backend-not-configured");
    }

    #[tokio::test]
    async fn round_trips_a_value() {
        let backend = backend();
        let ctx = LeaseContext::new(1000, "job", "cap");
        backend
            .provision("db_password", b"super_secure_db_pass")
            .expect("seal");
        let out = backend.resolve("db_password", &ctx).await.expect("open");
        assert_eq!(out.expose(), b"super_secure_db_pass");
    }

    #[test]
    fn the_stored_record_never_contains_the_plaintext() {
        let backend = backend();
        let sentinel = b"SENTINEL_SECRET_MATERIAL_NEVER_STORED";
        backend.provision("k", sentinel).expect("seal");
        let record = record_for(&backend, "k");
        assert!(
            !record.windows(sentinel.len()).any(|w| w == sentinel),
            "plaintext appeared verbatim in the sealed record"
        );
    }

    #[test]
    fn sealing_the_same_plaintext_twice_produces_different_bytes() {
        let backend = backend();
        backend.provision("a", b"identical-value").expect("seal a");
        backend.provision("b", b"identical-value").expect("seal b");
        let a = record_for(&backend, "a");
        let b = record_for(&backend, "b");
        assert_ne!(a, b, "deterministic ciphertext leaks plaintext equality");

        // And re-sealing the same locator is not stable either.
        backend
            .provision("a", b"identical-value")
            .expect("reseal a");
        let a2 = record_for(&backend, "a");
        assert_ne!(a, a2, "re-sealing must not reproduce the previous record");
    }

    /// The XOR construction this replaced had the property that
    /// `c1 ^ c2 == p1 ^ p2` for records under the same locator. Prove that is
    /// gone: XORing the two ciphertext bodies must not reveal the plaintext XOR.
    #[test]
    fn xoring_two_records_does_not_reveal_the_plaintext_xor() {
        let backend = backend();
        let p1 = b"aaaaaaaaaaaaaaaa";
        let p2 = b"bbbbbbbbbbbbbbbb";
        backend.provision("x", p1).expect("seal");
        let c1 = record_for(&backend, "x");
        backend.provision("x", p2).expect("seal");
        let c2 = record_for(&backend, "x");

        let expected: Vec<u8> = p1.iter().zip(p2.iter()).map(|(a, b)| a ^ b).collect();
        let body1 = &c1[HEADER_LEN..];
        let body2 = &c2[HEADER_LEN..];
        let actual: Vec<u8> = body1
            .iter()
            .zip(body2.iter())
            .map(|(a, b)| a ^ b)
            .take(expected.len())
            .collect();
        assert_ne!(actual, expected);
    }

    #[tokio::test]
    async fn a_tampered_record_fails_to_open() {
        let backend = backend();
        let ctx = LeaseContext::new(1000, "job", "cap");
        backend.provision("k", b"authentic-value").expect("seal");

        for flip_at in [0usize, MAGIC.len(), HEADER_LEN] {
            let mut record = record_for(&backend, "k");
            record[flip_at] ^= 0x01;
            let err = open(&backend.vault.as_ref().unwrap().kek, "k", &record)
                .expect_err("a flipped bit must fail authentication");
            assert_eq!(err.outcome_code(), "secrets.decrypt-failed");
        }

        // Truncation is equally fatal.
        let record = record_for(&backend, "k");
        let err = open(
            &backend.vault.as_ref().unwrap().kek,
            "k",
            &record[..record.len() - 1],
        )
        .expect_err("truncation must fail authentication");
        assert_eq!(err.outcome_code(), "secrets.decrypt-failed");

        // The untampered record still opens, so the test is not vacuous.
        assert_eq!(
            backend.resolve("k", &ctx).await.expect("open").expose(),
            b"authentic-value"
        );
    }

    #[test]
    fn a_record_moved_to_another_locator_fails_to_open() {
        let backend = backend();
        backend.provision("locator-one", b"value").expect("seal");
        let record = record_for(&backend, "locator-one");
        let kek = &backend.vault.as_ref().unwrap().kek;
        assert!(open(kek, "locator-one", &record).is_ok());
        let err = open(kek, "locator-two", &record)
            .expect_err("associated data must bind the record to its locator");
        assert_eq!(err.outcome_code(), "secrets.decrypt-failed");
    }

    #[test]
    fn a_record_sealed_under_another_kek_fails_to_open() {
        let backend = backend();
        backend.provision("k", b"value").expect("seal");
        let record = record_for(&backend, "k");

        let mut other = key();
        other[0] ^= 0xFF;
        let other = ManagedBackend::with_master_key(other).expect("key");
        let err = open(&other.vault.as_ref().unwrap().kek, "k", &record)
            .expect_err("a foreign KEK must not open the record");
        assert_eq!(err.outcome_code(), "secrets.decrypt-failed");
    }

    #[test]
    fn debug_output_never_shows_key_material() {
        let backend = backend();
        assert_eq!(
            format!("{backend:?}"),
            "ManagedBackend { configured: true }"
        );
        assert_eq!(
            format!("{:?}", ManagedBackend::unconfigured()),
            "ManagedBackend { configured: false }"
        );
        let kek = Kek(key());
        assert_eq!(format!("{kek:?}"), "Kek(\"<redacted>\")");
    }
}
