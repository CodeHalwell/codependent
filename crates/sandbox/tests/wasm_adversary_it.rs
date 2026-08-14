//! Adversarial WASM guests, run for real against the declared ceilings.
//!
//! The unit tests in `wasm.rs` check each mechanism against the adversary its
//! author imagined. This suite checks the *property* the threat model claims —
//! **a declared ceiling terminates the guest** — against adversaries chosen to
//! walk around the mechanism instead of into it. The 2026-08-13 review found
//! exactly that gap: the wall clock was tested with a pure-compute loop (which
//! yields on `OutOfFuel`, where the check lived) and never with a guest that
//! spends its life in host calls (which does not yield at all). That guest ran
//! **166.8 seconds against a declared 1-second ceiling and the host returned
//! `Ok`**.
//!
//! Every test here prints its real measured duration, so a regression shows up
//! as a number and not only as a failed assertion.

use std::sync::Arc;
use std::time::{Duration, Instant};

use codypendent_sandbox::gate::{GateDenied, GateGrant, GateSeal};
use codypendent_sandbox::{
    DenyAllGate, HostRequest, RunPolicyGate, SandboxProfile, WasmError, WasmHost, WasmLimits,
    WasmOutcome,
};

/// A profile with no capabilities at all — the posture every one of these
/// adversaries runs under unless it says otherwise.
fn profile() -> SandboxProfile {
    SandboxProfile {
        plugin: "skill:adversary".into(),
        env_allowlist: vec!["PATH".into()],
        read_paths: Vec::new(),
        write_paths: Vec::new(),
        network_allowlist: Vec::new(),
        brokered_secrets: Vec::new(),
        allow_subprocess: false,
        memory_mb: 16,
        cpu_seconds: 30,
        wall_seconds: 1,
        maximum_output_mb: 1,
    }
}

/// A gate standing in for a permissive run policy, so the positive controls are
/// not vacuous.
struct AllowAllGate;
impl RunPolicyGate for AllowAllGate {
    fn authorize(&self, _r: &HostRequest, seal: &GateSeal) -> Result<GateGrant, GateDenied> {
        Ok(GateGrant::issue(seal, "test-allow-all"))
    }
}

fn run_with(
    wat: &str,
    limits: WasmLimits,
    profile: &SandboxProfile,
    gate: Arc<dyn RunPolicyGate>,
) -> (Result<WasmOutcome, WasmError>, Duration) {
    run_with_input(wat, b"", limits, profile, gate)
}

fn run_with_input(
    wat: &str,
    input: &[u8],
    limits: WasmLimits,
    profile: &SandboxProfile,
    gate: Arc<dyn RunPolicyGate>,
) -> (Result<WasmOutcome, WasmError>, Duration) {
    let wasm = wat::parse_str(wat).expect("fixture assembles");
    let host = WasmHost::new(limits).expect("limits are valid");
    let started = Instant::now();
    let result = host.run(&wasm, input, profile, gate, "skill:adversary");
    (result, started.elapsed())
}

/// (a) A guest that loops on an **unprivileged** host call.
///
/// This is the reviewer's exploit verbatim: zero declared permissions, the
/// shipped `DenyAllGate` behind the host, and nothing but `codypendent::input`
/// in a loop. Fuel is not consumed inside a host function, so before the fix
/// the guest never reached the `OutOfFuel` slice boundary where the deadline
/// was checked, and the check never ran.
#[test]
fn a_host_call_loop_is_terminated_by_the_wall_clock_and_not_reported_ok() {
    let mut limits = WasmLimits::from_profile(&profile());
    limits.wall = Duration::from_millis(500);
    // Fuel must not be what stops it, or the test proves the wrong ceiling.
    limits.fuel = 200_000_000_000;

    // The shape matters, not just the loop. Each iteration must cost real time
    // in the HOST and almost no fuel in the GUEST, or the guest's own loop
    // reaches an `OutOfFuel` slice boundary and the old slice-boundary check
    // catches it — which is how a naive version of this test passes against the
    // unfixed code. So: 8 MiB copied per call (unavoidable host work: it is
    // `input`'s documented semantics), and only 2 000 iterations, whose guest
    // instructions total well under one 2 000 000-fuel slice. That is the
    // reviewer's reproduction: 800 198 fuel consumed across 166.8 seconds.
    let input = vec![b'A'; 8 * 1024 * 1024];
    let (result, elapsed) = run_with_input(
        r#"(module
             (import "codypendent" "input" (func $input (param i32 i32) (result i32)))
             (memory (export "memory") 128)
             (func (export "codypendent_run") (param i32) (result i32)
               (local $i i32)
               (local.set $i (i32.const 2000))
               (loop $l
                 (drop (call $input (i32.const 0) (i32.const 8388608)))
                 (local.set $i (i32.sub (local.get $i) (i32.const 1)))
                 (br_if $l (local.get $i)))
               i32.const 0))"#,
        &input,
        limits,
        &profile(),
        Arc::new(DenyAllGate),
    );

    println!(
        "(a) host-call loop: declared wall 500ms, real elapsed {elapsed:?}, result {result:?}"
    );
    match result {
        Err(WasmError::WallClockExceeded { wall }) => assert_eq!(wall, Duration::from_millis(500)),
        Ok(outcome) => panic!(
            "a guest that outlived its wall clock must be TERMINATED, not completed: {outcome:?}"
        ),
        Err(other) => panic!("expected WallClockExceeded, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(3),
        "overshoot must be bounded by one host call, not by guest behaviour: {elapsed:?}"
    );
}

/// (b) A guest that never calls out at all — the adversary the original test
/// imagined. Kept as the control: the slice-boundary check must still work.
#[test]
fn an_infinite_compute_loop_is_terminated_by_the_wall_clock() {
    let mut limits = WasmLimits::from_profile(&profile());
    limits.wall = Duration::from_millis(300);
    limits.fuel = 200_000_000_000;
    let (result, elapsed) = run_with(
        r#"(module
             (memory (export "memory") 1)
             (func (export "codypendent_run") (param i32) (result i32)
               (loop $l (br $l))
               i32.const 0))"#,
        limits,
        &profile(),
        Arc::new(DenyAllGate),
    );
    println!("(b) compute loop: declared wall 300ms, real elapsed {elapsed:?}, result {result:?}");
    assert!(
        matches!(result, Err(WasmError::WallClockExceeded { .. })),
        "{result:?}"
    );
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

/// (c) An unbounded allocator. Growth past the manifest's cap is a
/// spec-compliant `-1` to the guest, never a host allocation.
#[test]
fn an_unbounded_allocator_is_capped_at_the_declared_memory_ceiling() {
    let limits = WasmLimits::from_profile(&profile());
    let ceiling = limits.memory_bytes;
    let (result, elapsed) = run_with(
        // Grow 16 pages at a time until refused, then report the final size in
        // pages. A guest cannot exceed the cap; it can only learn where it is.
        r#"(module
             (memory (export "memory") 1)
             (func (export "codypendent_run") (param i32) (result i32)
               (loop $l
                 (br_if $l (i32.ne (memory.grow (i32.const 16)) (i32.const -1))))
               (memory.size)))"#,
        limits,
        &profile(),
        Arc::new(DenyAllGate),
    );
    let outcome = result.expect("a refused growth is a guest-visible -1, not a host error");
    let final_bytes = (outcome.status as usize) * 64 * 1024;
    println!(
        "(c) allocator: ceiling {ceiling} bytes, guest ended with {} pages = {final_bytes} bytes, elapsed {elapsed:?}",
        outcome.status
    );
    assert!(
        final_bytes <= ceiling,
        "the guest grew past its declared ceiling: {final_bytes} > {ceiling}"
    );
    assert!(
        final_bytes > ceiling / 2,
        "sanity: the guest really did grow, so the cap is what stopped it"
    );
}

/// (d) A file the manifest never declared, under a run policy that would allow
/// anything. The declaration is a ceiling the run policy cannot widen.
#[test]
fn an_ungranted_file_open_is_refused_by_the_declaration_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, b"top secret").unwrap();
    let canonical = std::fs::canonicalize(&secret).unwrap();

    let (result, elapsed) = run_with(
        &read_fixture(&canonical.to_string_lossy()),
        WasmLimits::from_profile(&profile()),
        &profile(), // declares nothing
        Arc::new(AllowAllGate),
    );
    let outcome = result.expect("a refusal is a guest-visible code, not a host error");
    println!(
        "(d) ungranted read: status={} host_bytes_read={} denials={:?} elapsed {elapsed:?}",
        outcome.status, outcome.host_bytes_read, outcome.denials
    );
    assert_eq!(outcome.status, -1, "the guest is told only that it failed");
    assert_eq!(outcome.host_bytes_read, 0, "nothing came off the disk");
    assert!(outcome.denials[0].contains("sandbox.undeclared-capability"));

    // The positive control: declared AND allowed, the same read succeeds. Without
    // this the refusal above could be an artifact of a broken fixture.
    let mut declared = profile();
    declared.read_paths = vec![canonical.to_string_lossy().into_owned()];
    let (allowed, _) = run_with(
        &read_fixture(&canonical.to_string_lossy()),
        WasmLimits::from_profile(&profile()),
        &declared,
        Arc::new(AllowAllGate),
    );
    let allowed = allowed.unwrap();
    assert_eq!(allowed.status, 10, "positive control: ten bytes were read");
    assert_eq!(allowed.host_bytes_read, 10);
}

/// (e) A network call. There is no network host function at all, so the refusal
/// is structural — the module cannot even instantiate.
#[test]
fn an_ungranted_network_call_is_refused_before_instantiation() {
    for (module, name, signature) in [
        (
            "wasi_snapshot_preview1",
            "sock_connect",
            "(param i32 i32 i32) (result i32)",
        ),
        ("codypendent", "connect", "(param i32 i32) (result i32)"),
        ("env", "system", "(param i32) (result i32)"),
    ] {
        let wat = format!(
            r#"(module
                 (import "{module}" "{name}" (func $c {signature}))
                 (memory (export "memory") 1)
                 (func (export "codypendent_run") (param i32) (result i32) i32.const 0))"#
        );
        let (result, _) = run_with(
            &wat,
            WasmLimits::from_profile(&profile()),
            &profile(),
            Arc::new(AllowAllGate),
        );
        println!("(e) import {module}::{name} -> {result:?}");
        match result {
            Err(WasmError::ForbiddenImport { module: m, name: n }) => {
                assert_eq!((m.as_str(), n.as_str()), (module, name));
            }
            other => panic!("an ungranted network import must be refused by name: {other:?}"),
        }
    }
}

/// (f) The host-read budget, against a guest that hands the host an
/// out-of-bounds destination.
///
/// The accounting used to happen *after* the copy into guest memory and was
/// skipped by an early return, so 500 reads of a granted 20 MB file pulled
/// 9.3 GiB off disk metered as `host_bytes_read = 0`.
#[test]
fn a_bad_destination_pointer_does_not_buy_unmetered_reads() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.bin");
    std::fs::write(&big, vec![7u8; 1_000_000]).unwrap();
    let canonical = std::fs::canonicalize(&big).unwrap();

    let mut declared = profile();
    declared.read_paths = vec![canonical.to_string_lossy().into_owned()];

    let mut limits = WasmLimits::from_profile(&declared);
    limits.host_read_bytes = 4_000_000; // four whole files' worth
    limits.wall = Duration::from_secs(30); // the wall must not be what stops it
    let ceiling = limits.host_read_bytes;

    // `out_ptr` is 4 MiB into a ONE PAGE (64 KiB) memory, so every copy fails.
    // 64 iterations against a 4-file budget: without the accounting the guest
    // reads 64 MB and is never refused.
    let path = canonical.to_string_lossy().into_owned();
    let wat = format!(
        r#"(module
             (import "codypendent" "read_file"
               (func $read (param i32 i32 i32 i32) (result i32)))
             (memory (export "memory") 1)
             (data (i32.const 0) "{path}")
             (func (export "codypendent_run") (param i32) (result i32)
               (local $i i32) (local $last i32)
               (local.set $i (i32.const 64))
               (loop $l
                 (local.set $last
                   (call $read (i32.const 0) (i32.const {len})
                               (i32.const 4194304) (i32.const 1000000)))
                 (local.set $i (i32.sub (local.get $i) (i32.const 1)))
                 (br_if $l (local.get $i)))
               (local.get $last)))"#,
        len = path.len()
    );

    let (result, elapsed) = run_with(&wat, limits, &declared, Arc::new(AllowAllGate));
    let outcome = result.expect("the guest completes; the budget is what stops the reads");
    println!(
        "(f) bad out_ptr: budget {ceiling} bytes, metered host_bytes_read = {}, denials = {}, elapsed {elapsed:?}",
        outcome.host_bytes_read,
        outcome.denials.len()
    );
    assert!(
        outcome.host_bytes_read > 0,
        "the bytes came off the disk, so they must be metered — not zero"
    );
    assert!(
        outcome.host_bytes_read <= ceiling,
        "metered {} bytes against a {ceiling}-byte ceiling",
        outcome.host_bytes_read
    );
    assert!(
        outcome
            .denials
            .iter()
            .any(|d| d.contains("host-read budget exhausted")),
        "the budget must actually engage and refuse: {:?}",
        outcome.denials
    );
}

/// (g) The documented guest ABI is the enforced guest ABI.
///
/// There is no WASM SDK and no example guest, so `wasm.rs`'s module doc is the
/// only spec a skill author has. It documented `codypendent_run: () -> i32`
/// while the host required one parameter, so a module built to the doc was
/// refused.
#[test]
fn a_module_built_to_the_documented_abi_loads() {
    let (documented, _) = run_with(
        r#"(module
             (memory (export "memory") 1)
             (func (export "codypendent_run") (param i32) (result i32) (local.get 0)))"#,
        WasmLimits::from_profile(&profile()),
        &profile(),
        Arc::new(DenyAllGate),
    );
    let outcome = documented.expect("the documented `(i32) -> i32` signature must load");
    assert_eq!(
        outcome.status, 0,
        "the reserved parameter the host passes is 0"
    );

    // And the arity really is checked, so the doc is normative rather than
    // decorative.
    let (nullary, _) = run_with(
        r#"(module
             (memory (export "memory") 1)
             (func (export "codypendent_run") (result i32) i32.const 0))"#,
        WasmLimits::from_profile(&profile()),
        &profile(),
        Arc::new(DenyAllGate),
    );
    assert!(
        matches!(nullary, Err(WasmError::MissingExport("codypendent_run"))),
        "{nullary:?}"
    );
}

/// A guest that finishes normally but overran its ceiling is still a refusal.
/// Otherwise one long host call turns an overshoot into a reported success.
#[test]
fn a_guest_that_returns_past_its_deadline_is_refused_rather_than_reported_ok() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.bin");
    std::fs::write(&file, vec![0u8; 1_000_000]).unwrap();
    let canonical = std::fs::canonicalize(&file).unwrap();
    let mut declared = profile();
    declared.read_paths = vec![canonical.to_string_lossy().into_owned()];

    let mut limits = WasmLimits::from_profile(&declared);
    // A ceiling no real run can respect: the very first host call is already
    // past it, so this pins "past the deadline ⇒ Err", not "slow ⇒ Err".
    limits.wall = Duration::from_nanos(1);

    let (result, elapsed) = run_with(
        &read_fixture(&canonical.to_string_lossy()),
        limits,
        &declared,
        Arc::new(AllowAllGate),
    );
    println!("(h) deadline already passed: elapsed {elapsed:?}, result {result:?}");
    assert!(
        matches!(result, Err(WasmError::WallClockExceeded { .. })),
        "{result:?}"
    );
}

/// Read `path` through `codypendent::read_file` and return the host's code.
fn read_fixture(path: &str) -> String {
    format!(
        r#"(module
             (import "codypendent" "read_file"
               (func $read (param i32 i32 i32 i32) (result i32)))
             (memory (export "memory") 1)
             (data (i32.const 0) "{path}")
             (func (export "codypendent_run") (param i32) (result i32)
               (call $read (i32.const 0) (i32.const {len}) (i32.const 4096) (i32.const 4096))))"#,
        len = path.len()
    )
}
