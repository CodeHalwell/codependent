//! Signed execution attestation and cryptographic verification (§3.5).
//!
//! Follows the canonical signing digest contract:
//! ```text
//! digest = SHA256( b"codypendent-runner-attestation-v1"
//!                || be_u64(len(canonical))
//!                || canonical )
//! ```

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::identity::RunnerIdentity;
use crate::types::{Attestation, AttestationStatement};

/// The canonical attestation scheme tag.
pub const ATTESTATION_SCHEME_V1: &str = "codypendent-runner-attestation-v1";

/// Attestation verification errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AttestationError {
    #[error(
        "foreign or unsupported attestation scheme tag: expected `{expected}`, got `{actual}`"
    )]
    ForeignSchemeTag { expected: String, actual: String },

    #[error("statement digest mismatch")]
    DigestMismatch,

    #[error("attestation signer public key does not match the trusted runner key")]
    SignerPublicKeyMismatch,

    #[error("attestation envelope field `{field}` does not match the signed statement")]
    EnvelopeMismatch { field: &'static str },

    #[error("attestation statement is not in canonical wire form")]
    NonCanonicalStatement,

    #[error("invalid signer public key: {0}")]
    InvalidPublicKey(String),

    #[error("invalid signature encoding: {0}")]
    InvalidSignature(String),

    #[error("signature verification failed: bad signature")]
    SignatureMismatch,

    #[error("failed to deserialize canonical statement: {0}")]
    Deserialization(String),
}

/// Compute the canonical statement digest and canonical JSON bytes according to §3.5.
#[must_use]
pub fn compute_statement_digest(
    statement: &AttestationStatement,
    scheme: &str,
) -> ([u8; 32], Vec<u8>) {
    let mut canonical_statement = statement.clone();
    // Deterministic ordering: outputs must be sorted by name ASC
    canonical_statement
        .outputs
        .sort_by(|a, b| a.name.cmp(&b.name));

    let canonical_bytes =
        serde_json::to_vec(&canonical_statement).expect("AttestationStatement serializes to JSON");

    let mut hasher = Sha256::new();
    hasher.update(scheme.as_bytes());
    hasher.update((canonical_bytes.len() as u64).to_be_bytes());
    hasher.update(&canonical_bytes);

    let digest: [u8; 32] = hasher.finalize().into();
    (digest, canonical_bytes)
}

/// Sign an attestation statement using the runner's identity key.
#[must_use]
pub fn sign_attestation(statement: AttestationStatement, identity: &RunnerIdentity) -> Attestation {
    let mut statement = statement;
    // The signing identity is authoritative for this duplicated field. Keeping
    // a caller-supplied runner id would create an envelope/statement ambiguity.
    statement.runner_id = identity.id;
    let (statement_digest, statement_bytes) =
        compute_statement_digest(&statement, ATTESTATION_SCHEME_V1);

    let signature = identity.sign(&statement_digest);

    Attestation {
        id: Uuid::now_v7(),
        attempt_id: statement.attempt_id,
        job_id: statement.job_id,
        lease_id: statement.lease_id,
        runner_id: identity.id,
        scheme: ATTESTATION_SCHEME_V1.to_string(),
        statement: statement_bytes,
        statement_digest,
        signature,
        signer_pubkey: identity.pubkey_bytes(),
    }
}

/// Verify an attestation and its signature against a trusted runner public key.
pub fn verify_attestation(
    attestation: &Attestation,
    trusted_pubkey: &[u8],
) -> Result<AttestationStatement, AttestationError> {
    // 1. Check scheme tag (reject foreign or older scheme tags outright)
    if attestation.scheme != ATTESTATION_SCHEME_V1 {
        return Err(AttestationError::ForeignSchemeTag {
            expected: ATTESTATION_SCHEME_V1.to_string(),
            actual: attestation.scheme.clone(),
        });
    }

    // 2. Parse and verify signer public key
    let key_bytes: [u8; 32] = trusted_pubkey.try_into().map_err(|_| {
        AttestationError::InvalidPublicKey(format!(
            "expected 32 bytes public key, got {}",
            trusted_pubkey.len()
        ))
    })?;

    let verifying_key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|e| AttestationError::InvalidPublicKey(e.to_string()))?;
    if attestation.signer_pubkey.as_slice() != trusted_pubkey {
        return Err(AttestationError::SignerPublicKeyMismatch);
    }

    // 3. Verify statement digest: recalculate SHA256(scheme || len_be64 || statement)
    let mut hasher = Sha256::new();
    hasher.update(attestation.scheme.as_bytes());
    hasher.update((attestation.statement.len() as u64).to_be_bytes());
    hasher.update(&attestation.statement);
    let expected_digest: [u8; 32] = hasher.finalize().into();

    if expected_digest != attestation.statement_digest {
        return Err(AttestationError::DigestMismatch);
    }

    // 4. Verify Ed25519 signature over the statement digest
    let sig_bytes: [u8; 64] = attestation.signature.as_slice().try_into().map_err(|_| {
        AttestationError::InvalidSignature(format!(
            "expected 64 bytes signature, got {}",
            attestation.signature.len()
        ))
    })?;

    let signature = Signature::from_bytes(&sig_bytes);
    verifying_key
        .verify_strict(&attestation.statement_digest, &signature)
        .map_err(|_| AttestationError::SignatureMismatch)?;

    // 5. Deserialize the canonical statement
    let statement: AttestationStatement = serde_json::from_slice(&attestation.statement)
        .map_err(|e| AttestationError::Deserialization(e.to_string()))?;

    // The envelope is routing metadata and is not itself signed. Bind every
    // duplicated identity back to the signed statement before a caller uses an
    // envelope field to select a lease or persistence key.
    for (field, matches) in [
        ("job_id", attestation.job_id == statement.job_id),
        ("attempt_id", attestation.attempt_id == statement.attempt_id),
        ("lease_id", attestation.lease_id == statement.lease_id),
        ("runner_id", attestation.runner_id == statement.runner_id),
    ] {
        if !matches {
            return Err(AttestationError::EnvelopeMismatch { field });
        }
    }

    // A valid signer must use the same deterministic representation produced
    // by `sign_attestation`. This rejects alternate JSON spellings and duplicate
    // fields rather than giving them protocol-level meaning.
    let (_, canonical) = compute_statement_digest(&statement, ATTESTATION_SCHEME_V1);
    if canonical != attestation.statement {
        return Err(AttestationError::NonCanonicalStatement);
    }

    Ok(statement)
}
