//! Runner identity, capability discovery, and cryptographic keys.

use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use rand::RngCore;
use std::collections::HashMap;
use uuid::Uuid;

use crate::types::RunnerCapabilities;

/// Runner identity and credentials.
#[derive(Clone)]
pub struct RunnerIdentity {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub name: String,
    pub kind: String,            // 'container' | 'kubernetes' | 'microvm' | 'macos'
    pub os: String,              // 'linux' | 'macos'
    pub arch: String,            // 'x86_64' | 'aarch64'
    pub sandbox_backend: String, // 'seatbelt' | 'bubblewrap' | 'none'
    pub capabilities: RunnerCapabilities,
    pub region: Option<String>,
    pub signing_key: SigningKey,
    pub attestation_pubkey: VerifyingKey,
}

impl RunnerIdentity {
    /// Generate a fresh runner identity with a new Ed25519 signing key.
    #[must_use]
    pub fn generate(
        organization_id: Uuid,
        name: impl Into<String>,
        kind: impl Into<String>,
        region: Option<String>,
    ) -> Self {
        // `SigningKey::generate` is gated behind ed25519-dalek's non-default `rand_core`
        // feature, which is not enabled for the workspace-pinned crypto dependency shared
        // with the trust store. Seeding from the OS CSPRNG directly is the same entropy
        // source that `generate` would use.
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let signing_key = SigningKey::from_bytes(&seed);
        let attestation_pubkey = signing_key.verifying_key();
        let os = std::env::consts::OS.to_string();
        let arch = std::env::consts::ARCH.to_string();
        let sandbox_backend = probe_sandbox_backend();
        let tools = probe_available_tools();

        let capabilities = RunnerCapabilities {
            tools,
            image_digest: None,
            region: region.clone(),
            policy_labels: vec![],
        };

        Self {
            id: Uuid::now_v7(),
            organization_id,
            name: name.into(),
            kind: kind.into(),
            os,
            arch,
            sandbox_backend,
            capabilities,
            region,
            signing_key,
            attestation_pubkey,
        }
    }

    /// Construct a runner identity with an explicit signing key.
    pub fn with_signing_key(
        id: Uuid,
        organization_id: Uuid,
        name: impl Into<String>,
        kind: impl Into<String>,
        signing_key: SigningKey,
        region: Option<String>,
    ) -> Self {
        let attestation_pubkey = signing_key.verifying_key();
        let os = std::env::consts::OS.to_string();
        let arch = std::env::consts::ARCH.to_string();
        let sandbox_backend = probe_sandbox_backend();
        let tools = probe_available_tools();

        let capabilities = RunnerCapabilities {
            tools,
            image_digest: None,
            region: region.clone(),
            policy_labels: vec![],
        };

        Self {
            id,
            organization_id,
            name: name.into(),
            kind: kind.into(),
            os,
            arch,
            sandbox_backend,
            capabilities,
            region,
            signing_key,
            attestation_pubkey,
        }
    }

    /// Returns public key as a 32-byte vector.
    #[must_use]
    pub fn pubkey_bytes(&self) -> Vec<u8> {
        self.attestation_pubkey.to_bytes().to_vec()
    }

    /// Sign bytes using runner's Ed25519 signing key.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        use ed25519_dalek::Signer;
        self.signing_key.sign(message).to_bytes().to_vec()
    }
}

/// Probe the local sandbox backend without running unconfined.
#[must_use]
pub fn probe_sandbox_backend() -> String {
    match codypendent_sandbox::enforcing_executor() {
        Ok(executor) => {
            let report = executor.capability_report();
            if report.available {
                report.backend.as_str().to_string()
            } else {
                "none".to_string()
            }
        }
        Err(_) => "none".to_string(),
    }
}

/// Probe standard development tools available on PATH.
#[must_use]
pub fn probe_available_tools() -> HashMap<String, String> {
    let mut tools = HashMap::new();
    for tool in &[
        "cargo", "rustc", "git", "docker", "podman", "node", "python3",
    ] {
        if which_tool(tool) {
            tools.insert((*tool).to_string(), "present".to_string());
        }
    }
    tools
}

fn which_tool(tool: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(tool);
            if candidate.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = candidate.metadata() {
                        if meta.permissions().mode() & 0o111 != 0 {
                            return true;
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    return true;
                }
            }
        }
    }
    false
}
