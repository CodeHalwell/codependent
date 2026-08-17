//! Hostile input archive materialization tests.
//!
//! Verifies Acceptance Criterion 10:
//! "Malicious archives are refused. Absolute path, ../ parent escape, symlink escape,
//! hardlink escape, duplicate entry conflict, expansion-ratio bomb, size overflow,
//! undeclared entry, wrong hash — nine cases, each refused before any byte lands on disk."

use flate2::write::GzEncoder;
use flate2::Compression;
use tempfile::TempDir;

use codypendent_runner::{
    InputManifest, InputManifestEntry, MaterializeError, MaterializeLimits, Materializer,
};

fn create_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut enc);
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            // Write the entry name straight into the header bytes rather than
            // via `set_path`/`append_data`. Those validate the path, so the tar
            // crate refuses to BUILD an absolute or `../` entry — which is
            // exactly the hostile input these tests must hand to the
            // materializer. Checksum last: it covers the name field.
            let name = path.as_bytes();
            assert!(
                name.len() < 100,
                "fixture path must fit the ustar name field"
            );
            header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name);
            header.set_cksum();
            tar.append(&header, *content).unwrap();
        }
        tar.finish().unwrap();
    }
    enc.finish().unwrap()
}

#[test]
fn materialize_refuses_hostile_archive_absolute_path() {
    let archive = create_tar_gz(&[("/etc/passwd", b"root:x:0:0::/root:/bin/bash")]);
    let temp_dir = TempDir::new().unwrap();
    let materializer = Materializer::new(MaterializeLimits::default());

    let err = materializer
        .materialize_bytes(&archive, temp_dir.path(), None)
        .unwrap_err();

    assert!(matches!(err, MaterializeError::AbsolutePath(_)));
}

#[test]
fn materialize_refuses_hostile_archive_parent_escape() {
    let archive = create_tar_gz(&[("subdir/../../escape.txt", b"escaped content")]);
    let temp_dir = TempDir::new().unwrap();
    let materializer = Materializer::new(MaterializeLimits::default());

    let err = materializer
        .materialize_bytes(&archive, temp_dir.path(), None)
        .unwrap_err();

    assert!(matches!(err, MaterializeError::ParentTraversal(_)));
}

#[test]
fn materialize_refuses_hostile_archive_symlink_escape() {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut enc);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        tar.append_link(&mut header, "evil_symlink", "../../etc/passwd")
            .unwrap();
        tar.finish().unwrap();
    }
    let archive = enc.finish().unwrap();

    let temp_dir = TempDir::new().unwrap();
    let materializer = Materializer::new(MaterializeLimits::default());

    let err = materializer
        .materialize_bytes(&archive, temp_dir.path(), None)
        .unwrap_err();

    assert!(matches!(err, MaterializeError::SymlinkEscape { .. }));
}

#[test]
fn materialize_refuses_hostile_archive_hardlink_escape() {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut enc);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        tar.append_link(&mut header, "evil_hardlink", "/etc/shadow")
            .unwrap();
        tar.finish().unwrap();
    }
    let archive = enc.finish().unwrap();

    let temp_dir = TempDir::new().unwrap();
    let materializer = Materializer::new(MaterializeLimits::default());

    let err = materializer
        .materialize_bytes(&archive, temp_dir.path(), None)
        .unwrap_err();

    assert!(matches!(err, MaterializeError::HardlinkEscape { .. }));
}

#[test]
fn materialize_refuses_hostile_archive_duplicate_entry_conflict() {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut enc);
        for content in &[b"version 1".as_slice(), b"version 2".as_slice()] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "file.txt", *content).unwrap();
        }
        tar.finish().unwrap();
    }
    let archive = enc.finish().unwrap();

    let temp_dir = TempDir::new().unwrap();
    let materializer = Materializer::new(MaterializeLimits::default());

    let err = materializer
        .materialize_bytes(&archive, temp_dir.path(), None)
        .unwrap_err();

    assert!(matches!(err, MaterializeError::DuplicateEntry(_)));
}

#[test]
fn materialize_refuses_hostile_archive_expansion_ratio_bomb() {
    // 5 MiB of zeros compressed into a tiny gzip archive
    let large_data = vec![0u8; 5 * 1024 * 1024];
    let archive = create_tar_gz(&[("zeros.dat", &large_data)]);

    let temp_dir = TempDir::new().unwrap();
    let limits = MaterializeLimits {
        max_file_size: 50 * 1024 * 1024,
        max_total_size: 100 * 1024 * 1024,
        max_files_count: 1000,
        max_expansion_ratio: 10, // Low ratio ceiling for testing bomb detection
        min_compressed_for_ratio: 100 * 1024, // Check above 100 KiB uncompressed
    };

    let materializer = Materializer::new(limits);
    let err = materializer
        .materialize_bytes(&archive, temp_dir.path(), None)
        .unwrap_err();

    assert!(matches!(err, MaterializeError::ExpansionBomb { .. }));
}

#[test]
fn materialize_refuses_hostile_archive_size_overflow() {
    let large_data = vec![b'A'; 2000];
    let archive = create_tar_gz(&[("large.txt", &large_data)]);

    let temp_dir = TempDir::new().unwrap();
    let limits = MaterializeLimits {
        max_file_size: 1000, // Limit smaller than file
        max_total_size: 5000,
        max_files_count: 10,
        max_expansion_ratio: 100,
        min_compressed_for_ratio: 1024 * 1024,
    };

    let materializer = Materializer::new(limits);
    let err = materializer
        .materialize_bytes(&archive, temp_dir.path(), None)
        .unwrap_err();

    assert!(matches!(err, MaterializeError::SizeOverflow { .. }));
}

#[test]
fn materialize_refuses_hostile_archive_undeclared_entry() {
    let archive = create_tar_gz(&[
        ("declared.txt", b"valid declared file"),
        ("surprise.sh", b"#!/bin/sh\nrm -rf /"),
    ]);

    let manifest = InputManifest {
        entries: vec![InputManifestEntry {
            path: "declared.txt".to_string(),
            // The real SHA-256 of `valid declared file`. It was previously the
            // hash of the EMPTY string, so this entry failed the hash check and
            // the test never reached the undeclared `surprise.sh` it exists for.
            content_hash: "sha256:a843ae79659a42754bdd0a98344c75e554cbd6e1237ae93059b71cf9fa8ddbb6"
                .to_string(),
            byte_length: 19,
            mode: 0o644,
            executable: false,
        }],
    };

    let temp_dir = TempDir::new().unwrap();
    let materializer = Materializer::new(MaterializeLimits::default());

    let err = materializer
        .materialize_bytes(&archive, temp_dir.path(), Some(&manifest))
        .unwrap_err();

    assert!(matches!(err, MaterializeError::UndeclaredEntry(path) if path == "surprise.sh"));
}

#[test]
fn materialize_refuses_hostile_archive_wrong_hash() {
    let content = b"tampered file content";
    let archive = create_tar_gz(&[("src/lib.rs", content)]);

    let manifest = InputManifest {
        entries: vec![InputManifestEntry {
            path: "src/lib.rs".to_string(),
            content_hash: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(), // Wrong expected hash
            byte_length: content.len() as u64,
            mode: 0o644,
            executable: false,
        }],
    };

    let temp_dir = TempDir::new().unwrap();
    let materializer = Materializer::new(MaterializeLimits::default());

    let err = materializer
        .materialize_bytes(&archive, temp_dir.path(), Some(&manifest))
        .unwrap_err();

    assert!(matches!(err, MaterializeError::ChecksumMismatch { .. }));
}

#[test]
fn materialize_extracts_clean_archive_successfully() {
    use sha2::{Digest, Sha256};

    let file1_content = b"fn main() { println!(\"Hello world\"); }";
    let file2_content = b"[package]\nname = \"test-app\"\nversion = \"0.1.0\"\n";

    let hash1 = format!("sha256:{}", hex::encode(Sha256::digest(file1_content)));
    let hash2 = format!("sha256:{}", hex::encode(Sha256::digest(file2_content)));

    let archive = create_tar_gz(&[
        ("src/main.rs", file1_content),
        ("Cargo.toml", file2_content),
    ]);

    let manifest = InputManifest {
        entries: vec![
            InputManifestEntry {
                path: "src/main.rs".to_string(),
                content_hash: hash1,
                byte_length: file1_content.len() as u64,
                mode: 0o644,
                executable: false,
            },
            InputManifestEntry {
                path: "Cargo.toml".to_string(),
                content_hash: hash2,
                byte_length: file2_content.len() as u64,
                mode: 0o644,
                executable: false,
            },
        ],
    };

    let temp_dir = TempDir::new().unwrap();
    let materializer = Materializer::new(MaterializeLimits::default());

    let report = materializer
        .materialize_bytes(&archive, temp_dir.path(), Some(&manifest))
        .unwrap();

    assert_eq!(report.files.len(), 2);
    assert!(temp_dir.path().join("src/main.rs").exists());
    assert!(temp_dir.path().join("Cargo.toml").exists());
}
