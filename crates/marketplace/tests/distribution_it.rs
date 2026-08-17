//! Distribution, hostile archive defense, and verification integration tests (Milestone 5, Tasks 5.3 & 5.4).

use codypendent_marketplace::{
    checksum_of, signing_digest, ContentAddressedStore, DownloadAllowlist, MarketplaceError,
    PackageVerifier, TrustedPublishers, UnsignedPolicy, MAX_COMPRESSION_RATIO,
};
use codypendent_sandbox::{parse_manifest, PluginManifest, VerifyError};
use ed25519_dalek::{Signer, SigningKey};
use flate2::write::GzEncoder;
use flate2::Compression;
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
            let path_bytes = path.as_bytes();
            let name_slice = &mut header.as_mut_bytes()[..100];
            name_slice.fill(0);
            let len = path_bytes.len().min(100);
            name_slice[..len].copy_from_slice(&path_bytes[..len]);
            header.set_cksum();
            builder.append(&header, *content).unwrap();
        }
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn build_symlink_tar_gz(link_name: &str, target: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        let mut header = Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_link_name(target).unwrap();
        header.set_cksum();
        builder
            .append_data(&mut header, link_name, &[][..])
            .unwrap();
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn build_signed_package(
    id: &str,
    publisher: &str,
    version: &str,
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

#[test]
fn package_verification_success_and_failures() {
    let signing_key = SigningKey::from_bytes(&rand::random());
    let pub_bytes = signing_key.verifying_key().to_bytes();
    let pub_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pub_bytes);

    let mut trust_store = TrustedPublishers::default();
    trust_store.add("alice", &pub_b64).unwrap();

    let artifact = build_tar_gz(&[("main.wasm", b"\x00asm\x01\x00\x00\x00")]);
    let (manifest_toml, _) =
        build_signed_package("good-pkg", "alice", "1.0.0", &artifact, &signing_key);

    let verifier = PackageVerifier::new();

    // 1. Success with valid signature and trusted publisher under default-deny policy
    let (parsed, verified) = verifier
        .verify(&manifest_toml, &artifact, &trust_store)
        .unwrap();
    assert_eq!(parsed.id, "good-pkg");
    assert!(verified.signed);

    // 2. Checksum mismatch
    let corrupt_artifact = build_tar_gz(&[("main.wasm", b"different bytes")]);
    let err = verifier
        .verify(&manifest_toml, &corrupt_artifact, &trust_store)
        .unwrap_err();
    assert!(matches!(
        err,
        MarketplaceError::Verify(VerifyError::ChecksumMismatch { .. })
    ));

    // 3. Untrusted publisher key (bob is not in trust store)
    let bob_signing_key = SigningKey::from_bytes(&rand::random());
    let (bob_toml, _) =
        build_signed_package("bob-pkg", "bob", "1.0.0", &artifact, &bob_signing_key);
    let err = verifier
        .verify(&bob_toml, &artifact, &trust_store)
        .unwrap_err();
    assert!(matches!(
        err,
        MarketplaceError::Verify(VerifyError::InvalidPublisherKey(_))
    ));

    // 4. Unsigned package under default Deny policy
    let checksum = checksum_of(&artifact);
    let unsigned_toml = format!(
        r#"
schema_version = 1
id = "unsigned-pkg"
name = "Unsigned"
version = "1.0.0"
kind = "wasm-component"
publisher = "alice"
scopes = ["workspace"]
[runtime]
command = "main.wasm"
[security]
checksum = "{checksum}"
signature = ""
"#
    );
    let err = verifier
        .verify(&unsigned_toml, &artifact, &trust_store)
        .unwrap_err();
    assert!(matches!(
        err,
        MarketplaceError::Verify(VerifyError::UnsignedDenied)
    ));

    // 5. Unsigned package allowed under explicit Allow policy
    let dev_verifier = PackageVerifier::with_unsigned_policy(UnsignedPolicy::Allow);
    let (dev_parsed, dev_verified) = dev_verifier
        .verify(&unsigned_toml, &artifact, &trust_store)
        .unwrap();
    assert_eq!(dev_parsed.id, "unsigned-pkg");
    assert!(!dev_verified.signed);

    // 6. Tampered signed manifest (e.g. altered name) fails signature verification
    let tampered_toml =
        manifest_toml.replace("Test Package good-pkg", "Malicious Package good-pkg");
    let err = verifier
        .verify(&tampered_toml, &artifact, &trust_store)
        .unwrap_err();
    assert!(matches!(
        err,
        MarketplaceError::Verify(VerifyError::SignatureMismatch)
    ));
}

#[test]
fn hostile_archive_matrix_is_refused() {
    let tmp = tempdir().unwrap();
    let cas = ContentAddressedStore::new(tmp.path()).unwrap();

    // 1. Absolute path refusal
    let abs_artifact = build_tar_gz(&[("/etc/passwd", b"root:x:0:0:::")]);
    let err = cas
        .install_artifact("sha256:abs", &abs_artifact)
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::Package(_)));

    // 2. Parent traversal refusal
    let parent_artifact = build_tar_gz(&[("../../escape.txt", b"escaped")]);
    let err = cas
        .install_artifact("sha256:parent", &parent_artifact)
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::Package(_)));

    // 3. Symlink entry refusal
    let symlink_artifact = build_symlink_tar_gz("evil_link", "/etc/passwd");
    let err = cas
        .install_artifact("sha256:symlink", &symlink_artifact)
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::Package(_)));

    // 4. Duplicate normalized path refusal
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        for path in &["dir/file.txt", "dir/./file.txt"] {
            let mut header = Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, *path, &b"test"[..])
                .unwrap();
        }
        builder.finish().unwrap();
    }
    let dup_artifact = encoder.finish().unwrap();
    let err = cas
        .install_artifact("sha256:dup", &dup_artifact)
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::Package(_)));

    // 5. Empty archive refusal (0 files)
    let empty_artifact = build_tar_gz(&[]);
    let err = cas
        .install_artifact("sha256:empty", &empty_artifact)
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::Package(_)));

    // 6. Excessive compression ratio refusal (decompression bomb defense)
    // 500 KB of zeroes compresses into ~500 bytes (ratio ~1000:1 > MAX_COMPRESSION_RATIO)
    let bomb_data = vec![0u8; 500 * 1024];
    let bomb_artifact = build_tar_gz(&[("bomb.dat", &bomb_data)]);
    let ratio = (bomb_data.len() as u64) / (bomb_artifact.len() as u64);
    assert!(ratio > MAX_COMPRESSION_RATIO);
    let err = cas
        .install_artifact("sha256:bomb", &bomb_artifact)
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::Package(_)));
}

#[test]
fn download_security_controls() {
    let mut allowlist = DownloadAllowlist::new();
    allowlist.allow_domain("marketplace.codypendent.io");
    allowlist.allow_domain("cdn.trusted-plugins.com");

    // 1. Allowed HTTPS domains
    assert!(allowlist
        .check_url("https://marketplace.codypendent.io/v1/pkg.tar.gz")
        .is_ok());
    assert!(allowlist
        .check_url("https://sub.marketplace.codypendent.io/pkg.tar.gz")
        .is_ok());
    assert!(allowlist
        .check_url("https://cdn.trusted-plugins.com/plugin.tar.gz")
        .is_ok());

    // 2. Disallowed HTTP scheme (cleartext)
    let err = allowlist
        .check_url("http://marketplace.codypendent.io/pkg.tar.gz")
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::DownloadDisallowed(_)));

    // 3. Disallowed domains
    let err = allowlist
        .check_url("https://evil.com/pkg.tar.gz")
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::DownloadDisallowed(_)));

    // 4. SSRF private IP attempts
    let err = allowlist
        .check_url("https://10.0.0.1/pkg.tar.gz")
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::DownloadDisallowed(_)));

    let err = allowlist
        .check_url("https://192.168.1.100/pkg.tar.gz")
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::DownloadDisallowed(_)));

    let err = allowlist
        .check_url("https://127.0.0.1/pkg.tar.gz")
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::DownloadDisallowed(_)));

    // 5. Localhost opt-in for testing
    allowlist.set_allow_localhost(true);
    assert!(allowlist
        .check_url("http://localhost:8080/pkg.tar.gz")
        .is_ok());
    assert!(allowlist
        .check_url("http://127.0.0.1:8080/pkg.tar.gz")
        .is_ok());
}
