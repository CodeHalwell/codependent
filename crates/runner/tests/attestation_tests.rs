//! Attestation digest binding, signing, and verification tests.
//!
//! Verifies Acceptance Criteria 2 and 3:
//! - Acceptance Criterion 2: "Attestation bytes bind every required field. Mutating any one of
//!   job_spec_hash, attempt_id, lease_id, image_digest, input_manifest_hash, any output hash,
//!   or the result changes the digest. Test: attestation_digest_binds_every_field"
//! - Acceptance Criterion 3: "An older-scheme signature does not verify.
//!   Test: attestation_rejects_foreign_scheme_tag"

use uuid::Uuid;

use codypendent_runner::{
    compute_statement_digest, sign_attestation, verify_attestation, AttestationError,
    AttestationOutput, AttestationStatement, RunnerIdentity, ATTESTATION_SCHEME_V1,
};

fn sample_statement() -> AttestationStatement {
    AttestationStatement {
        job_id: Uuid::parse_str("018f6c42-8c4d-7a31-9b1b-9f931d8e1234").unwrap(),
        job_spec_hash: "sha256:1111111111111111111111111111111111111111111111111111111111111111"
            .to_string(),
        attempt_id: Uuid::parse_str("018f6c42-8c4d-7a31-9b1b-9f931d8e5678").unwrap(),
        attempt_number: 1,
        lease_id: Uuid::parse_str("018f6c42-8c4d-7a31-9b1b-9f931d8e9abc").unwrap(),
        lease_generation: 1,
        runner_id: Uuid::parse_str("018f6c42-8c4d-7a31-9b1b-9f931d8edef0").unwrap(),
        image_digest: "sha256:2222222222222222222222222222222222222222222222222222222222222222"
            .to_string(),
        input_manifest_hash:
            "sha256:3333333333333333333333333333333333333333333333333333333333333333".to_string(),
        outputs: vec![
            AttestationOutput {
                name: "bin".to_string(),
                content_hash:
                    "sha256:4444444444444444444444444444444444444444444444444444444444444444"
                        .to_string(),
                byte_length: 1024,
            },
            AttestationOutput {
                name: "report.json".to_string(),
                content_hash:
                    "sha256:5555555555555555555555555555555555555555555555555555555555555555"
                        .to_string(),
                byte_length: 256,
            },
        ],
        started_at: "2026-08-17T10:00:00Z".to_string(),
        ended_at: "2026-08-17T10:01:00Z".to_string(),
        exit_code: 0,
        result: "succeeded".to_string(),
    }
}

#[test]
fn attestation_digest_binds_every_field() {
    let baseline = sample_statement();
    let (baseline_digest, _) = compute_statement_digest(&baseline, ATTESTATION_SCHEME_V1);

    // Table-driven mutations: each mutation MUST change the resulting digest!
    let mutations: Vec<(&'static str, Box<dyn Fn(&mut AttestationStatement)>)> = vec![
        ("job_id", Box::new(|s| s.job_id = Uuid::now_v7())),
        (
            "job_spec_hash",
            Box::new(|s| {
                s.job_spec_hash =
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .to_string()
            }),
        ),
        ("attempt_id", Box::new(|s| s.attempt_id = Uuid::now_v7())),
        ("attempt_number", Box::new(|s| s.attempt_number = 2)),
        ("lease_id", Box::new(|s| s.lease_id = Uuid::now_v7())),
        ("lease_generation", Box::new(|s| s.lease_generation = 2)),
        ("runner_id", Box::new(|s| s.runner_id = Uuid::now_v7())),
        (
            "image_digest",
            Box::new(|s| {
                s.image_digest =
                    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                        .to_string()
            }),
        ),
        (
            "input_manifest_hash",
            Box::new(|s| {
                s.input_manifest_hash =
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                        .to_string()
            }),
        ),
        (
            "output_hash",
            Box::new(|s| {
                s.outputs[0].content_hash =
                    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
                        .to_string()
            }),
        ),
        (
            "output_length",
            Box::new(|s| s.outputs[0].byte_length = 2048),
        ),
        (
            "output_name",
            Box::new(|s| s.outputs[0].name = "renamed_bin".to_string()),
        ),
        (
            "started_at",
            Box::new(|s| s.started_at = "2026-08-17T11:00:00Z".to_string()),
        ),
        (
            "ended_at",
            Box::new(|s| s.ended_at = "2026-08-17T11:01:00Z".to_string()),
        ),
        ("exit_code", Box::new(|s| s.exit_code = 1)),
        ("result", Box::new(|s| s.result = "failed".to_string())),
    ];

    for (field_name, mutator) in mutations {
        let mut modified = baseline.clone();
        mutator(&mut modified);
        let (modified_digest, _) = compute_statement_digest(&modified, ATTESTATION_SCHEME_V1);

        assert_ne!(
            baseline_digest, modified_digest,
            "Mutating field `{field_name}` must change the attestation digest"
        );
    }
}

#[test]
fn attestation_rejects_foreign_scheme_tag() {
    let identity = RunnerIdentity::generate(Uuid::now_v7(), "test-runner", "container", None);
    let statement = sample_statement();
    let mut attestation = sign_attestation(statement, &identity);

    // Foreign / older scheme tag
    attestation.scheme = "codypendent-runner-attestation-v0".to_string();

    let err = verify_attestation(&attestation, &identity.pubkey_bytes()).unwrap_err();
    assert!(matches!(err, AttestationError::ForeignSchemeTag { .. }));
}

#[test]
fn attestation_verifies_valid_signature_successfully() {
    let identity = RunnerIdentity::generate(Uuid::now_v7(), "test-runner", "container", None);
    let statement = sample_statement();
    let attestation = sign_attestation(statement.clone(), &identity);

    let verified = verify_attestation(&attestation, &identity.pubkey_bytes()).unwrap();
    assert_eq!(verified.job_id, statement.job_id);
    assert_eq!(verified.result, "succeeded");
}

#[test]
fn attestation_rejects_tampered_statement_or_digest() {
    let identity = RunnerIdentity::generate(Uuid::now_v7(), "test-runner", "container", None);
    let statement = sample_statement();
    let mut attestation = sign_attestation(statement, &identity);

    // Tamper statement bytes
    attestation.statement[10] ^= 0xff;

    let err = verify_attestation(&attestation, &identity.pubkey_bytes()).unwrap_err();
    assert_eq!(err, AttestationError::DigestMismatch);
}

#[test]
fn attestation_rejects_wrong_public_key() {
    let identity1 = RunnerIdentity::generate(Uuid::now_v7(), "runner-1", "container", None);
    let identity2 = RunnerIdentity::generate(Uuid::now_v7(), "runner-2", "container", None);

    let statement = sample_statement();
    let attestation = sign_attestation(statement, &identity1);

    // Verify with identity2's key
    let err = verify_attestation(&attestation, &identity2.pubkey_bytes()).unwrap_err();
    assert_eq!(err, AttestationError::SignatureMismatch);
}
