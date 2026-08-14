//! codypendent-sandbox — the plugin **security boundary** (Phase 6).
//!
//! This crate exists because it draws a trust boundary (the manual's crate rule):
//! everything a plugin declares is *untrusted input* until this crate has parsed,
//! verified, and gated it. It carries no daemon or agent-framework code, so the
//! security decisions are exercised in isolation.
//!
//! The pieces, in lifecycle order:
//!
//! * [`manifest`] — parse `plugin.toml` (the [`docs/specs/plugin.toml`] shape):
//!   identity, runtime, capabilities, resources, security record, update policy.
//! * [`verify`] — checksum (sha256) + publisher signature (ed25519) verification,
//!   with the default-**deny** unsigned policy.
//! * [`permission`] — the [`CapabilitySet`](permission::CapabilitySet) and the
//!   **permission diff** that blocks a capability-expanding update until it is
//!   re-approved (exit criterion 2).
//! * [`profile`] — lowering a granted capability set into a **closed**
//!   [`SandboxProfile`](profile::SandboxProfile): env allowlist, pre-opened paths,
//!   network allowlist, resource caps. An executor that honours it cannot grant
//!   an undeclared path or host (exit criterion 1).
//! * [`lifecycle`] — the discover → verify → install-disabled → smoke-test →
//!   enable → update → revoke state machine, carrying each plugin's trust record.
//! * [`sanitize`] — neutralize untrusted plugin/MCP output (label by origin,
//!   size-cap, strip control sequences) before it enters context.
//!
//! What this crate contains as of STEP 6.2: the [`executor`] — the OS-level
//! enforcement seam (`sandbox-exec` on macOS, `bwrap` arg-generation on Linux, a
//! fail-closed refusal elsewhere) that *consumes* a [`SandboxProfile`](profile::SandboxProfile)
//! and actually confines a process — and the [`trust_store`], the data-only
//! trusted-publisher key store that gives [`verify_artifact`](verify::verify_artifact)
//! real keys to verify against.
//!
//! STEP 6.3 adds the WASM half and the seam that keeps it honest:
//!
//! * [`wasm`] — a `wasmi` guest runtime with **enforced** ceilings (fuel,
//!   linear memory, a wall clock that actually terminates, output, host I/O)
//!   and no ambient capabilities at all: WASI is not linked, and a module that
//!   imports it is refused by name.
//! * [`gate`] — the [`RunPolicyGate`](gate::RunPolicyGate) seam. A package's
//!   own manifest is a **ceiling**, never a grant: every privileged act a guest
//!   attempts is lowered into a [`HostRequest`](gate::HostRequest) that must
//!   also be authorized by the daemon's deny-first **run** policy. This crate
//!   cannot depend on `codypendent-daemon` (the daemon depends on *it*), so the
//!   dependency is inverted rather than a second capability model being minted
//!   here. See `.impl/threat-models/12-executable-skills.md` §0.
//!
//! * [`hook`] — the hook engine's decision core: a `hook.toml` parser with the
//!   same `deny_unknown_fields` discipline as `plugin.toml`, the verdict lattice
//!   in which `Deny` is absorbing, and the [`Unapproved`](hook::Unapproved) type
//!   wall that makes "a hook cannot escalate privilege" a compile-time property
//!   — a rewritten tool call has no accessor and can only be unwrapped by
//!   re-entering the policy engine. See `.impl/threat-models/13-hooks.md`.
//!
//! What it still defers (named, not faked): the brokered-secrets daemon, the
//! network broker that `network_allowlist` waits on, and the runtime dispatch
//! site that emits hook events (owned by another crate; see the hook module docs).
//!
//! [`docs/specs/plugin.toml`]: ../../docs/specs/plugin.toml

pub mod executor;
pub mod gate;
pub mod hook;
pub mod lifecycle;
pub mod manifest;
pub mod permission;
pub mod profile;
pub mod sanitize;
pub mod trust_store;
pub mod verify;
pub mod wasm;

pub use executor::{
    bwrap_argv, enforcing_executor, seatbelt_profile, CapabilityReport, RefusingSandbox,
    SandboxBackend, SandboxCommand, SandboxError, SandboxExecutor, SandboxOutcome,
    SandboxProcessSpec,
};
pub use gate::{
    CapabilityBroker, DenyAllGate, GateDenied, GateGrant, GateSeal, HostRequest, RunPolicyGate,
};
pub use hook::{
    combine, parse_hook, validate_event_set, Authorized, FailurePolicy, HookDenied, HookError,
    HookEvent, HookKind, HookNetwork, HookOutcome, HookOutput, HookPolicy, HookRuntime, HookScope,
    HookSpec, HookVerdict, PolicyReentry, ReentryContext, ToolCall, Unapproved, HOOK_PLACEHOLDERS,
    MAX_HOOKS_PER_EVENT, SUPPORTED_HOOK_SCHEMA_VERSION,
};
pub use lifecycle::{
    InstalledPlugin, LifecycleError, LifecycleState, PendingUpdateApproval, TrustTier,
};
pub use manifest::{
    parse_manifest, CapabilitiesSpec, ManifestError, PluginKind, PluginManifest, ResourcesSpec,
    RuntimeSpec, SecuritySpec, UiCapability, UiCompatibilitySpec, UiContributionPoint,
    UiContributionSpec, UiEntrypointsSpec, UiSpec, UiTarget, UpdateSpec,
    SUPPORTED_PLUGIN_SCHEMA_VERSION, SUPPORTED_UI_PROTOCOL_VERSION, SUPPORTED_UI_SCHEMA_VERSION,
    SUPPORTED_UI_SDK_VERSION,
};
pub use permission::{
    diff_manifests, diff_resources, Capability, CapabilitySet, PermissionDiff, ResourceChange,
    UiPermission,
};
pub use profile::{SandboxProfile, ENV_ALLOWLIST};
pub use sanitize::{sanitize_untrusted, Sanitized};
pub use trust_store::{TrustStoreError, TrustedPublishers};
pub use verify::{
    checksum_of, signing_digest, verify_artifact, UnsignedPolicy, Verified, VerifyError,
};
pub use wasm::{WasmError, WasmHost, WasmLimits, WasmOutcome};
