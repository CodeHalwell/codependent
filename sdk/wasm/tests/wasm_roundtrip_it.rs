//! The gold-standard check: build a plugin with this SDK, exactly the way the
//! SDK tells an author to build one, and load it through the **real** host in
//! `crates/sandbox`.
//!
//! The unit tests in `src/lib.rs` assert the generated entry point's name and
//! signature at compile time, which is what catches the defect this suite was
//! written for (the macro emitted `run() -> i32`; the host requires
//! `codypendent_run(i32) -> i32` and refused every SDK-built plugin). They
//! cannot check the parts that only exist after linking — that a `cdylib`
//! actually exports `memory`, that no forbidden import sneaks in, that the
//! module carries no `start` section. Only a real build can, so here it is.
//!
//! # Why every test here is `#[ignore]`d
//!
//! It needs the `wasm32-unknown-unknown` target and a nested `cargo build`.
//! Making that a hard requirement would put a wasm toolchain on the critical
//! path of every CI run and every `cargo test --workspace`, for a check that
//! only changes when the ABI does. Run it deliberately:
//!
//! ```text
//! rustup target add wasm32-unknown-unknown
//! cargo test -p codypendent-wasm-sdk --test wasm_roundtrip_it -- --ignored --nocapture
//! ```
//!
//! If the target is not installed the test **skips loudly** rather than
//! passing quietly; anything else that fails is a real failure.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use codypendent_sandbox::gate::{GateDenied, GateGrant, GateSeal};
use codypendent_sandbox::{
    HostRequest, RunPolicyGate, SandboxProfile, WasmHost, WasmLimits, WasmOutcome,
};

const TARGET: &str = "wasm32-unknown-unknown";

/// A gate standing in for a permissive run policy. The guest here makes no
/// privileged call, so this is never consulted; it exists because the host
/// refuses to be built without one.
struct AllowAllGate;
impl RunPolicyGate for AllowAllGate {
    fn authorize(&self, _r: &HostRequest, seal: &GateSeal) -> Result<GateGrant, GateDenied> {
        Ok(GateGrant::issue(seal, "test-allow-all"))
    }
}

fn profile() -> SandboxProfile {
    SandboxProfile {
        plugin: "skill:sdk-round-trip".into(),
        env_allowlist: Vec::new(),
        read_paths: Vec::new(),
        write_paths: Vec::new(),
        network_allowlist: Vec::new(),
        brokered_secrets: Vec::new(),
        allow_subprocess: false,
        memory_mb: 32,
        cpu_seconds: 10,
        wall_seconds: 20,
        maximum_output_mb: 1,
    }
}

/// Whether the wasm target is installed. Without `rustup` we cannot tell, so we
/// assume not and skip — a false skip is loud, a false failure is noise.
fn wasm_target_installed() -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|l| l.trim() == TARGET)
        })
        .unwrap_or(false)
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo-guest")
}

/// Build the fixture guest and return the module bytes.
///
/// The fixture carries its own empty `[workspace]` table and is built into its
/// own target directory, so this nested `cargo` neither joins the repository's
/// workspace nor contends with the outer build's lock.
fn build_guest() -> Vec<u8> {
    let manifest = fixture_dir().join("Cargo.toml");
    let target_dir = std::env::temp_dir().join("codypendent-sdk-roundtrip-target");
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--release", "--target", TARGET])
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target_dir)
        .status()
        .expect("failed to invoke cargo for the fixture guest");
    assert!(status.success(), "fixture guest failed to build");

    let wasm = target_dir
        .join(TARGET)
        .join("release")
        .join("codypendent_echo_guest.wasm");
    std::fs::read(&wasm).unwrap_or_else(|e| panic!("no module at {}: {e}", wasm.display()))
}

fn run(wasm: &[u8], input: &[u8]) -> WasmOutcome {
    let host = WasmHost::new(WasmLimits::from_profile(&profile())).expect("host");
    host.run(
        wasm,
        input,
        &profile(),
        Arc::new(AllowAllGate),
        "test:sdk-round-trip",
    )
    .expect("the host must accept and run an SDK-built plugin")
}

/// The defect, end to end: with the old macro this fails at load with
/// `MissingExport("codypendent_run")`.
#[test]
#[ignore = "needs the wasm32-unknown-unknown target; run with --ignored"]
fn sdk_built_plugin_loads_and_runs_under_the_real_host() {
    if !wasm_target_installed() {
        eprintln!("SKIPPED: {TARGET} is not installed (`rustup target add {TARGET}`)");
        return;
    }
    let wasm = build_guest();

    let outcome = run(&wasm, b"round trip");
    assert!(outcome.success(), "status was {}", outcome.status);
    // Proves the two-call sizing protocol worked against the real host: the
    // guest could only echo the whole input if it sized its buffer from the
    // `cap = 0` call.
    assert!(
        outcome.output.text.contains("echo:round trip"),
        "output was {:?}",
        outcome.output.text
    );
    assert!(outcome.denials.is_empty());
}

/// The guest's `Err(code)` must arrive at the host as the outcome code.
#[test]
#[ignore = "needs the wasm32-unknown-unknown target; run with --ignored"]
fn guest_outcome_code_reaches_the_host() {
    if !wasm_target_installed() {
        eprintln!("SKIPPED: {TARGET} is not installed (`rustup target add {TARGET}`)");
        return;
    }
    let wasm = build_guest();

    let outcome = run(&wasm, b"fail");
    assert_eq!(outcome.status, 3);
    assert!(!outcome.success());
}

/// An empty input must not trap: it is the one-call path through the sizing
/// protocol, and a null pointer reaches the host on it.
#[test]
#[ignore = "needs the wasm32-unknown-unknown target; run with --ignored"]
fn empty_input_is_not_a_trap() {
    if !wasm_target_installed() {
        eprintln!("SKIPPED: {TARGET} is not installed (`rustup target add {TARGET}`)");
        return;
    }
    let wasm = build_guest();

    let outcome = run(&wasm, b"");
    assert!(outcome.success(), "status was {}", outcome.status);
    assert!(outcome.output.text.contains("echo:"));
}
