//! The WASM guest runtime (STEP 6.3) — enforced resource ceilings and a host
//! surface that cannot act without the run policy.
//!
//! A skill may ship a `.wasm` module. This module runs it under `wasmi` with
//! every ceiling **enforced, not advisory**: a deterministic instruction budget
//! (fuel), a linear-memory cap, a wall-clock deadline that actually terminates
//! the guest, a captured-output cap, and a host-I/O byte budget. The full
//! reasoning, including what is deliberately not defended against, is in
//! `.impl/threat-models/12-executable-skills.md`.
//!
//! # Denied by default
//!
//! A guest gets **no imports at all** beyond the `codypendent` module defined
//! below. WASI preview 1 and 2 are not linked and `wasmi_wasi` is not a
//! dependency, so a module importing `wasi_snapshot_preview1.fd_write` is
//! refused before instantiation with a named import rather than a generic
//! linker failure. There is no host function for writing files, spawning
//! processes, or opening sockets — those are refused *structurally*, by the
//! absence of a callable, which is stronger than refusing them at a gate.
//! There is no clock, no randomness, no environment, and no argv.
//!
//! A module carrying a `start` section is refused: start code would run outside
//! the fuel-sliced loop below and so outside the wall-clock ceiling.
//!
//! # Why `wasmi` and not `wasmtime`
//!
//! A skill module runs for milliseconds, so an interpreter is fast enough — and
//! it keeps a JIT, which compiles attacker-supplied bytecode into native code,
//! out of the trust boundary. `wasmi` also costs 30 crates against wasmtime's
//! 97 (measured), which matters in a workspace three other crates depend on.
//! `wasmi` has no epoch interruption; [`WasmHost::run`] replaces it with
//! fuel-sliced resumption, which bounds work deterministically rather than by
//! host scheduling.
//!
//! # The guest ABI
//!
//! The guest exports:
//!
//! | export | signature | required |
//! |---|---|---|
//! | `memory` | linear memory | yes |
//! | `codypendent_run` | `() -> i32` | yes |
//!
//! and may import, from module `codypendent`:
//!
//! | import | signature | privileged |
//! |---|---|---|
//! | `input` | `(ptr: i32, cap: i32) -> i32` | no |
//! | `log` | `(ptr: i32, len: i32)` | no |
//! | `read_file` | `(path_ptr, path_len, out_ptr, out_cap) -> i32` | **yes** |
//!
//! `input` copies up to `cap` bytes of the invocation's input into guest memory
//! and returns the input's true length, so a guest calls it once with `cap = 0`
//! to size its buffer. The host never writes into guest memory unbidden and the
//! guest needs no allocator contract.
//!
//! `read_file` is the only privileged call. It goes through
//! [`CapabilityBroker`], so it is refused unless the package's manifest
//! declared the path **and** the run policy allows it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmi::{
    Caller, Config, Engine, Extern, Linker, Module, ResumableCall, Store, StoreLimits,
    StoreLimitsBuilder, Val,
};

use crate::gate::{CapabilityBroker, HostRequest, RunPolicyGate};
use crate::profile::SandboxProfile;
use crate::sanitize::{sanitize_untrusted, Sanitized};

/// Fuel granted per resumption slice. Small enough that a slice is a few
/// milliseconds on a slow host, so the wall-clock check between slices bounds
/// overshoot tightly; large enough that the resume overhead is negligible.
const FUEL_SLICE: u64 = 2_000_000;

/// Fuel granted per second of the profile's `cpu_seconds`. This is the
/// conversion that makes a manifest's CPU cap mean something for a guest that
/// never enters the OS scheduler's view.
const FUEL_PER_CPU_SECOND: u64 = 200_000_000;

/// Hard ceilings a manifest can never exceed, however it is written. A package
/// declares what it needs *within* these; it cannot raise them.
const MAX_MEMORY_BYTES: usize = 512 * 1024 * 1024;
const MAX_WALL: Duration = Duration::from_secs(600);
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_HOST_READ_BYTES: usize = 64 * 1024 * 1024;
const MAX_FUEL: u64 = 200_000_000_000;

/// Guest-visible result codes. A guest learns only that a call failed, never
/// why: "denied by policy" and "no such file" return the identical code so the
/// host surface is not an enumeration oracle for the filesystem or for the
/// operator's policy.
const GUEST_REFUSED: i32 = -1;
const GUEST_BAD_ARGUMENT: i32 = -2;

/// The enforced ceilings for one guest invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmLimits {
    /// Total instruction budget. Exhaustion terminates the guest.
    pub fuel: u64,
    /// Linear-memory ceiling in bytes. `memory.grow` past this returns −1 to
    /// the guest (a spec-compliant allocation failure), it does not abort.
    pub memory_bytes: usize,
    /// Wall-clock ceiling. Checked between fuel slices; overshoot is bounded by
    /// one slice, not by guest behaviour.
    pub wall: Duration,
    /// Captured-output ceiling. Output past this is dropped and flagged.
    pub output_bytes: usize,
    /// Total bytes the guest may pull through privileged host calls. Bounds
    /// host-side I/O independently of fuel, which does not meter host work.
    pub host_read_bytes: usize,
}

impl WasmLimits {
    /// Derive limits from a package's [`SandboxProfile`], clamped to the hard
    /// ceilings above. The profile's `cpu_seconds` becomes a fuel budget and
    /// its `wall_seconds` becomes the deadline, so a skill's `[limits]` table
    /// reaches the guest instead of stopping at the parser.
    #[must_use]
    pub fn from_profile(profile: &SandboxProfile) -> Self {
        Self {
            fuel: profile
                .cpu_seconds
                .saturating_mul(FUEL_PER_CPU_SECOND)
                .min(MAX_FUEL),
            memory_bytes: (profile.memory_mb as usize)
                .saturating_mul(1024 * 1024)
                .min(MAX_MEMORY_BYTES),
            wall: Duration::from_secs(profile.wall_seconds).min(MAX_WALL),
            output_bytes: (profile.maximum_output_mb as usize)
                .saturating_mul(1024 * 1024)
                .min(MAX_OUTPUT_BYTES),
            host_read_bytes: MAX_HOST_READ_BYTES,
        }
    }

    /// Reject a degenerate budget. A zero ceiling is refused rather than
    /// treated as "unlimited" — the same rule
    /// [`crate::executor`]'s `validate_enforceable_profile` applies to the
    /// process path, so the two executors cannot disagree about what a zero
    /// means.
    fn validate(&self) -> Result<(), WasmError> {
        for (field, zero) in [
            ("fuel", self.fuel == 0),
            ("memory_bytes", self.memory_bytes == 0),
            ("wall", self.wall.is_zero()),
            ("output_bytes", self.output_bytes == 0),
            ("host_read_bytes", self.host_read_bytes == 0),
        ] {
            if zero {
                return Err(WasmError::InvalidLimits(format!(
                    "resource ceiling `{field}` must be greater than zero"
                )));
            }
        }
        Ok(())
    }
}

/// Why a guest could not be run, or was terminated. Every variant is a refusal:
/// the host never continues a guest past a breached ceiling.
#[derive(Debug, thiserror::Error)]
pub enum WasmError {
    /// The module is not valid WebAssembly, or uses a disabled proposal.
    #[error("invalid wasm module: {0}")]
    Compile(String),
    /// The module imports something outside the `codypendent` host module.
    #[error("wasm module imports `{module}::{name}`, which the host does not provide; a guest gets no ambient capabilities (WASI is not linked)")]
    ForbiddenImport {
        /// The import's module namespace.
        module: String,
        /// The import's field name.
        name: String,
    },
    /// The module carries a `start` section.
    #[error(
        "wasm module declares a start function; start code would run outside the metered call"
    )]
    StartFunction,
    /// A required export is missing or has the wrong type.
    #[error("wasm module does not export `{0}` with the expected signature")]
    MissingExport(&'static str),
    /// The instruction budget was exhausted.
    #[error("wasm guest exhausted its instruction budget ({fuel} fuel)")]
    FuelExhausted {
        /// The budget that was exhausted.
        fuel: u64,
    },
    /// The wall-clock deadline passed.
    #[error("wasm guest exceeded its wall-clock ceiling ({wall:?})")]
    WallClockExceeded {
        /// The ceiling that was breached.
        wall: Duration,
    },
    /// The guest trapped (unreachable, out-of-bounds, division by zero, …).
    #[error("wasm guest trapped: {0}")]
    Trap(String),
    /// The declared ceilings are unusable.
    #[error("invalid wasm resource ceilings: {0}")]
    InvalidLimits(String),
    /// The runtime itself failed to set up.
    #[error("wasm runtime error: {0}")]
    Runtime(String),
}

/// The captured, sanitized result of a guest invocation — the audit record,
/// shaped like [`crate::executor::SandboxOutcome`] so both executors report the
/// same way.
#[derive(Debug, Clone)]
pub struct WasmOutcome {
    /// The `i32` the guest's `codypendent_run` returned. Zero is success by
    /// convention; the host does not interpret it further.
    pub status: i32,
    /// Everything the guest wrote via `log`, sanitized and origin-labeled.
    pub output: Sanitized,
    /// Whether output hit the cap and was truncated.
    pub output_truncated: bool,
    /// Fuel actually consumed.
    pub fuel_consumed: u64,
    /// Wall-clock duration.
    pub duration: Duration,
    /// Every privileged request that was refused, in order — so a denial is
    /// visible to the operator even though the guest only saw an error code.
    pub denials: Vec<String>,
    /// Bytes pulled through privileged host calls.
    pub host_bytes_read: usize,
}

impl WasmOutcome {
    /// Whether the guest completed and reported success.
    #[must_use]
    pub fn success(&self) -> bool {
        self.status == 0
    }

    /// A one-line audit summary (safe to log — guest output is not included).
    #[must_use]
    pub fn audit_summary(&self) -> String {
        format!(
            "wasm origin={} status={} duration_ms={} fuel={} out_bytes={} host_read={} denials={} stripped={} truncated={}",
            self.output.origin,
            self.status,
            self.duration.as_millis(),
            self.fuel_consumed,
            self.output.text.len(),
            self.host_bytes_read,
            self.denials.len(),
            self.output.stripped_controls,
            self.output_truncated,
        )
    }
}

/// Per-invocation host state. Owned (not borrowed) so the store carries no
/// lifetime and the host functions stay plain `fn`s.
struct HostState {
    profile: SandboxProfile,
    gate: Arc<dyn RunPolicyGate>,
    input: Vec<u8>,
    output: Vec<u8>,
    output_cap: usize,
    output_truncated: bool,
    host_read_remaining: usize,
    host_bytes_read: usize,
    denials: Vec<String>,
    limits: StoreLimits,
}

impl HostState {
    /// Record a refusal for the audit trail. The guest never sees this text.
    fn deny(&mut self, request: &HostRequest, reason: &str) -> i32 {
        self.denials
            .push(format!("{} refused: {reason}", request.describe()));
        GUEST_REFUSED
    }
}

/// Copy `len` bytes out of guest memory, refusing an out-of-bounds or
/// implausibly large range before allocating anything host-side.
fn read_guest_bytes(
    caller: &Caller<'_, HostState>,
    memory: &wasmi::Memory,
    ptr: i32,
    len: usize,
    ceiling: usize,
) -> Option<Vec<u8>> {
    let ptr = usize::try_from(ptr).ok()?;
    if len > ceiling {
        return None;
    }
    let mut buffer = vec![0u8; len];
    memory.read(caller, ptr, &mut buffer).ok()?;
    Some(buffer)
}

/// The guest's own memory export. A guest without one cannot exchange bytes.
fn guest_memory(caller: &mut Caller<'_, HostState>) -> Option<wasmi::Memory> {
    caller.get_export("memory").and_then(Extern::into_memory)
}

/// `codypendent::input` — copy the invocation input into guest memory.
/// Unprivileged: the input is what the *host* chose to pass.
fn host_input(mut caller: Caller<'_, HostState>, ptr: i32, cap: i32) -> i32 {
    let Some(memory) = guest_memory(&mut caller) else {
        return GUEST_BAD_ARGUMENT;
    };
    let Ok(cap) = usize::try_from(cap) else {
        return GUEST_BAD_ARGUMENT;
    };
    let Ok(ptr) = usize::try_from(ptr) else {
        return GUEST_BAD_ARGUMENT;
    };
    let input = caller.data().input.clone();
    let full = input.len();
    let n = cap.min(full);
    // A zero-capacity call is the documented "how big is my input?" query, so
    // it must not fail on a null pointer.
    if n > 0 && memory.write(&mut caller, ptr, &input[..n]).is_err() {
        return GUEST_BAD_ARGUMENT;
    }
    i32::try_from(full).unwrap_or(i32::MAX)
}

/// `codypendent::log` — append to the captured output. Unprivileged, but capped:
/// a guest cannot exhaust host memory by logging.
fn host_log(mut caller: Caller<'_, HostState>, ptr: i32, len: i32) {
    let Some(memory) = guest_memory(&mut caller) else {
        return;
    };
    let Ok(len) = usize::try_from(len) else {
        return;
    };
    let room = {
        let state = caller.data();
        state.output_cap.saturating_sub(state.output.len())
    };
    // Clamp rather than refuse: an over-long write is truncated and flagged, so
    // a guest cannot suppress the truncation signal by writing one huge buffer
    // instead of many small ones. `room` bounds the host-side allocation.
    let wanted = len.min(room);
    let Some(bytes) = read_guest_bytes(&caller, &memory, ptr, wanted, room) else {
        return;
    };
    let state = caller.data_mut();
    state.output.extend_from_slice(&bytes);
    if len > wanted {
        state.output_truncated = true;
    }
}

/// `codypendent::read_file` — the only privileged host call.
///
/// The path is canonicalized **before** the request is formed, and the file
/// that is opened is that same canonical path, so the bytes checked and the
/// bytes read are the same object: a `..` segment or a symlinked parent cannot
/// smuggle the read out of the granted roots between check and open.
fn host_read_file(
    mut caller: Caller<'_, HostState>,
    path_ptr: i32,
    path_len: i32,
    out_ptr: i32,
    out_cap: i32,
) -> i32 {
    let Some(memory) = guest_memory(&mut caller) else {
        return GUEST_BAD_ARGUMENT;
    };
    // A path longer than this is not a path.
    let Ok(path_len) = usize::try_from(path_len) else {
        return GUEST_BAD_ARGUMENT;
    };
    let Some(raw) = read_guest_bytes(&caller, &memory, path_ptr, path_len, 4096) else {
        return GUEST_BAD_ARGUMENT;
    };
    let Ok(requested) = String::from_utf8(raw) else {
        return GUEST_BAD_ARGUMENT;
    };
    let (Ok(out_ptr), Ok(out_cap)) = (usize::try_from(out_ptr), usize::try_from(out_cap)) else {
        return GUEST_BAD_ARGUMENT;
    };

    // Resolve first, then decide, then open the resolved path. A missing file
    // and a denied file return the SAME code (`GUEST_REFUSED`) so the guest
    // cannot use this call to probe for the existence of paths it may not read.
    let Ok(resolved) = std::fs::canonicalize(&requested) else {
        let request = HostRequest::ReadFile { path: requested };
        return caller.data_mut().deny(&request, "unresolvable path");
    };
    let request = HostRequest::ReadFile {
        path: resolved.to_string_lossy().into_owned(),
    };

    let verdict = {
        let state = caller.data();
        let broker = CapabilityBroker::new(&state.profile, state.gate.as_ref());
        broker.request(&request)
    };
    let grant = match verdict {
        Ok(grant) => grant,
        Err(denied) => {
            let reason = denied.code.clone();
            return caller.data_mut().deny(&request, &reason);
        }
    };
    debug_assert!(
        grant.answers(&request),
        "broker checks this before returning"
    );

    let budget = caller.data().host_read_remaining;
    if budget == 0 {
        return caller
            .data_mut()
            .deny(&request, "host-read budget exhausted");
    }
    let bytes = match read_capped(&resolved, budget.min(out_cap)) {
        Ok(bytes) => bytes,
        Err(_) => return caller.data_mut().deny(&request, "read failed"),
    };
    if memory.write(&mut caller, out_ptr, &bytes).is_err() {
        return GUEST_BAD_ARGUMENT;
    }
    let state = caller.data_mut();
    state.host_read_remaining = state.host_read_remaining.saturating_sub(bytes.len());
    state.host_bytes_read += bytes.len();
    i32::try_from(bytes.len()).unwrap_or(i32::MAX)
}

/// Read at most `cap` bytes. Never allocates on the file's own claimed size, so
/// a hostile symlink to `/dev/zero` cannot exhaust host memory.
fn read_capped(path: &PathBuf, cap: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)?;
    let mut buffer = Vec::new();
    file.take(cap as u64).read_to_end(&mut buffer)?;
    Ok(buffer)
}

/// The WASM guest runtime. One host is reusable across invocations; the engine
/// configuration (fuel metering, disabled proposals) is fixed at construction
/// so it cannot be relaxed per call.
#[derive(Debug)]
pub struct WasmHost {
    engine: Engine,
    limits: WasmLimits,
}

impl WasmHost {
    /// Build a host enforcing `limits`.
    pub fn new(limits: WasmLimits) -> Result<Self, WasmError> {
        limits.validate()?;
        let mut config = Config::default();
        config.consume_fuel(true);
        // Proposals a skill has no need for, switched off so their
        // implementations are not reachable from guest bytes at all.
        // SIMD and relaxed-SIMD are not compiled in at all (wasmi's `simd`
        // feature is off), which is stronger than switching them off here.
        config
            .wasm_memory64(false)
            .wasm_multi_memory(false)
            .wasm_custom_page_sizes(false)
            .wasm_wide_arithmetic(false)
            .wasm_tail_call(false);
        Ok(Self {
            engine: Engine::new(&config),
            limits,
        })
    }

    /// The ceilings this host enforces.
    #[must_use]
    pub fn limits(&self) -> &WasmLimits {
        &self.limits
    }

    /// Run `wasm` with `input`, under `profile`'s declaration ceiling and
    /// `gate`'s run policy.
    ///
    /// There is no overload without a gate: a host that could be built without
    /// one would be a second policy path, so the absence is deliberate.
    pub fn run(
        &self,
        wasm: &[u8],
        input: &[u8],
        profile: &SandboxProfile,
        gate: Arc<dyn RunPolicyGate>,
        origin: &str,
    ) -> Result<WasmOutcome, WasmError> {
        let started = Instant::now();
        let module =
            Module::new(&self.engine, wasm).map_err(|e| WasmError::Compile(e.to_string()))?;

        // Name the offending import rather than letting the linker fail
        // generically: an operator debugging a refused skill needs to see that
        // it asked for WASI.
        for import in module.imports() {
            let (m, n) = (import.module(), import.name());
            let known = m == "codypendent" && matches!(n, "input" | "log" | "read_file");
            if !known {
                return Err(WasmError::ForbiddenImport {
                    module: m.to_string(),
                    name: n.to_string(),
                });
            }
        }

        let state = HostState {
            profile: profile.clone(),
            gate,
            input: input.to_vec(),
            output: Vec::new(),
            output_cap: self.limits.output_bytes,
            output_truncated: false,
            host_read_remaining: self.limits.host_read_bytes,
            host_bytes_read: 0,
            denials: Vec::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(self.limits.memory_bytes)
                // One memory, one table, one instance: a guest cannot multiply
                // its memory ceiling by allocating more memories.
                .memories(1)
                .tables(1)
                .instances(1)
                .build(),
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);

        let mut linker = Linker::new(&self.engine);
        let link = |linker: &mut Linker<HostState>| -> Result<(), wasmi::errors::LinkerError> {
            linker.func_wrap("codypendent", "input", host_input)?;
            linker.func_wrap("codypendent", "log", host_log)?;
            linker.func_wrap("codypendent", "read_file", host_read_file)?;
            Ok(())
        };
        link(&mut linker).map_err(|e| WasmError::Runtime(e.to_string()))?;

        // `Linker::instantiate` is deprecated in favour of
        // `instantiate_and_start`, whose suggested way to suppress a `start`
        // section is to pre-set fuel to zero and let it trap. That is weaker
        // than what is wanted here: a module carrying start code is *refused*,
        // legibly, rather than run-until-it-runs-out. `ensure_no_start` is only
        // reachable through the deprecated call, so the deprecation is taken
        // deliberately.
        #[allow(deprecated)]
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| WasmError::Compile(e.to_string()))?
            .ensure_no_start(&mut store)
            .map_err(|_| WasmError::StartFunction)?;

        let entry = instance
            .get_func(&store, "codypendent_run")
            .ok_or(WasmError::MissingExport("codypendent_run"))?;
        let ty = entry.ty(&store);
        if ty.params().len() != 1 || ty.results().len() != 1 {
            return Err(WasmError::MissingExport("codypendent_run"));
        }
        let _ = instance
            .get_memory(&store, "memory")
            .ok_or(WasmError::MissingExport("memory"))?;

        let deadline = started + self.limits.wall;
        let mut outputs = [Val::I32(0)];
        let mut remaining = self.limits.fuel;
        let mut granted = remaining.min(FUEL_SLICE);
        remaining -= granted;
        store
            .set_fuel(granted)
            .map_err(|e| WasmError::Runtime(e.to_string()))?;

        let mut call = entry
            .call_resumable(&mut store, &[Val::I32(0)], &mut outputs)
            .map_err(|e| self.classify_trap(e))?;
        let mut consumed: u64 = 0;
        loop {
            match call {
                ResumableCall::Finished => {
                    consumed += granted.saturating_sub(store.get_fuel().unwrap_or(0));
                    break;
                }
                ResumableCall::HostTrap(_) => {
                    // No host function returns an error, so reaching this means
                    // the runtime's contract changed under us. Refuse.
                    return Err(WasmError::Trap("host function trapped".into()));
                }
                ResumableCall::OutOfFuel(next) => {
                    consumed += granted;
                    // The wall clock is enforced *here*, outside the guest: a
                    // guest cannot extend a slice, refuel itself, or skip this
                    // check, so overshoot is bounded by one slice.
                    if Instant::now() >= deadline {
                        return Err(WasmError::WallClockExceeded {
                            wall: self.limits.wall,
                        });
                    }
                    let need = next.required_fuel().max(1);
                    if remaining < need {
                        return Err(WasmError::FuelExhausted {
                            fuel: self.limits.fuel,
                        });
                    }
                    granted = remaining.min(FUEL_SLICE.max(need));
                    remaining -= granted;
                    store
                        .set_fuel(granted)
                        .map_err(|e| WasmError::Runtime(e.to_string()))?;
                    call = next
                        .resume(&mut store, &mut outputs)
                        .map_err(|e| self.classify_trap(e))?;
                }
            }
        }

        let status = match outputs[0] {
            Val::I32(v) => v,
            _ => return Err(WasmError::MissingExport("codypendent_run")),
        };
        let state = store.into_data();
        let output = sanitize_untrusted(
            origin,
            &String::from_utf8_lossy(&state.output),
            self.limits.output_bytes,
        );
        Ok(WasmOutcome {
            status,
            output_truncated: state.output_truncated || output.truncated,
            output,
            fuel_consumed: consumed,
            duration: started.elapsed(),
            denials: state.denials,
            host_bytes_read: state.host_bytes_read,
        })
    }

    /// Map a `wasmi` error onto a refusal. Fuel exhaustion inside a slice can
    /// surface as a plain error rather than a resumable state when the trap
    /// escapes a host boundary, so it is recognised here too.
    fn classify_trap(&self, error: wasmi::Error) -> WasmError {
        if matches!(error.as_trap_code(), Some(wasmi::core::TrapCode::OutOfFuel)) {
            return WasmError::FuelExhausted {
                fuel: self.limits.fuel,
            };
        }
        WasmError::Trap(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{DenyAllGate, GateDenied, GateGrant, GateSeal};

    fn profile() -> SandboxProfile {
        SandboxProfile {
            plugin: "skill:test".into(),
            env_allowlist: vec!["PATH".into()],
            read_paths: Vec::new(),
            write_paths: Vec::new(),
            network_allowlist: Vec::new(),
            brokered_secrets: Vec::new(),
            allow_subprocess: false,
            memory_mb: 16,
            cpu_seconds: 2,
            wall_seconds: 5,
            maximum_output_mb: 1,
        }
    }

    fn host() -> WasmHost {
        WasmHost::new(WasmLimits::from_profile(&profile())).expect("limits are valid")
    }

    fn run(wat: &str, input: &[u8], profile: &SandboxProfile) -> Result<WasmOutcome, WasmError> {
        let wasm = wat::parse_str(wat).expect("fixture assembles");
        host().run(&wasm, input, profile, Arc::new(DenyAllGate), "skill:test")
    }

    /// A gate standing in for a permissive run policy.
    struct AllowAllGate;
    impl RunPolicyGate for AllowAllGate {
        fn authorize(&self, _r: &HostRequest, seal: &GateSeal) -> Result<GateGrant, GateDenied> {
            Ok(GateGrant::issue(seal, "test-allow-all"))
        }
    }

    #[test]
    fn a_module_importing_wasi_is_refused_by_name() {
        let err = run(
            r#"(module
                 (import "wasi_snapshot_preview1" "fd_write"
                   (func (param i32 i32 i32 i32) (result i32)))
                 (memory (export "memory") 1)
                 (func (export "codypendent_run") (param i32) (result i32) i32.const 0))"#,
            b"",
            &profile(),
        )
        .unwrap_err();
        match err {
            WasmError::ForbiddenImport { module, name } => {
                assert_eq!(module, "wasi_snapshot_preview1");
                assert_eq!(name, "fd_write");
            }
            other => panic!("expected a named forbidden import, got {other:?}"),
        }
    }

    #[test]
    fn a_module_with_a_start_section_is_refused() {
        let err = run(
            r#"(module
                 (memory (export "memory") 1)
                 (func $s)
                 (start $s)
                 (func (export "codypendent_run") (param i32) (result i32) i32.const 0))"#,
            b"",
            &profile(),
        )
        .unwrap_err();
        assert!(matches!(err, WasmError::StartFunction));
    }

    #[test]
    fn an_infinite_loop_is_terminated_by_the_wall_clock() {
        // The property epoch interruption would give: a guest that calls no
        // host function and never returns is still stopped.
        let mut limits = WasmLimits::from_profile(&profile());
        limits.wall = Duration::from_millis(150);
        limits.fuel = MAX_FUEL; // fuel must not be what stops it
        let host = WasmHost::new(limits).unwrap();
        let wasm = wat::parse_str(
            r#"(module
                 (memory (export "memory") 1)
                 (func (export "codypendent_run") (param i32) (result i32)
                   (loop $l (br $l))
                   i32.const 0))"#,
        )
        .unwrap();
        let started = Instant::now();
        let err = host
            .run(&wasm, b"", &profile(), Arc::new(DenyAllGate), "skill:test")
            .unwrap_err();
        assert!(
            matches!(err, WasmError::WallClockExceeded { .. }),
            "{err:?}"
        );
        // Overshoot is bounded by one fuel slice, not unbounded.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "terminated promptly"
        );
    }

    #[test]
    fn a_small_fuel_budget_terminates_a_long_computation() {
        let mut limits = WasmLimits::from_profile(&profile());
        limits.fuel = 1_000;
        limits.wall = Duration::from_secs(60);
        let host = WasmHost::new(limits).unwrap();
        let wasm = wat::parse_str(
            r#"(module
                 (memory (export "memory") 1)
                 (func (export "codypendent_run") (param i32) (result i32)
                   (loop $l (br $l))
                   i32.const 0))"#,
        )
        .unwrap();
        let err = host
            .run(&wasm, b"", &profile(), Arc::new(DenyAllGate), "skill:test")
            .unwrap_err();
        assert!(matches!(err, WasmError::FuelExhausted { .. }), "{err:?}");
    }

    #[test]
    fn memory_growth_is_refused_past_the_cap() {
        // 16 MB cap; the guest asks for 64 MB (1024 pages) and must be told no
        // (−1), not granted it and not aborted.
        let out = run(
            r#"(module
                 (memory (export "memory") 1)
                 (func (export "codypendent_run") (param i32) (result i32)
                   (memory.grow (i32.const 1024))))"#,
            b"",
            &profile(),
        )
        .expect("growth failure is a guest-visible -1, not a host error");
        assert_eq!(out.status, -1, "memory.grow past the cap returns -1");
    }

    #[test]
    fn output_is_captured_sanitized_and_capped() {
        let mut limits = WasmLimits::from_profile(&profile());
        limits.output_bytes = 8;
        let host = WasmHost::new(limits).unwrap();
        // Writes "AAAAAAAAAAAA" (12 bytes) into a cap of 8.
        let wasm = wat::parse_str(
            r#"(module
                 (import "codypendent" "log" (func $log (param i32 i32)))
                 (memory (export "memory") 1)
                 (data (i32.const 0) "AAAAAAAAAAAA")
                 (func (export "codypendent_run") (param i32) (result i32)
                   (call $log (i32.const 0) (i32.const 12))
                   i32.const 0))"#,
        )
        .unwrap();
        let out = host
            .run(&wasm, b"", &profile(), Arc::new(DenyAllGate), "skill:test")
            .unwrap();
        assert!(out.output_truncated);
        assert_eq!(out.output.origin, "skill:test");
        assert!(out.output.text.len() <= 8);
    }

    #[test]
    fn input_is_pulled_by_the_guest_and_its_true_length_reported() {
        let out = run(
            r#"(module
                 (import "codypendent" "input" (func $input (param i32 i32) (result i32)))
                 (memory (export "memory") 1)
                 (func (export "codypendent_run") (param i32) (result i32)
                   (call $input (i32.const 0) (i32.const 0))))"#,
            b"hello world",
            &profile(),
        )
        .unwrap();
        assert_eq!(
            out.status, 11,
            "a zero-capacity call reports the true length"
        );
    }

    /// The fixture used by both halves of the gate test: read a path out of
    /// memory and return the host's result code verbatim.
    const READ_FIXTURE: &str = r#"(module
         (import "codypendent" "read_file"
           (func $read (param i32 i32 i32 i32) (result i32)))
         (memory (export "memory") 1)
         (func (export "codypendent_run") (param i32) (result i32)
           (call $read (i32.const 0) (i32.const PATHLEN) (i32.const 256) (i32.const 256))))"#;

    fn read_fixture(path: &str) -> Vec<u8> {
        let wat = format!(
            "{}\n",
            READ_FIXTURE.replace("PATHLEN", &path.len().to_string())
        );
        // Place the path bytes at offset 0 via a data segment.
        let wat = wat.replace(
            "(memory (export \"memory\") 1)",
            &format!("(memory (export \"memory\") 1)\n(data (i32.const 0) \"{path}\")"),
        );
        wat::parse_str(&wat).expect("fixture assembles")
    }

    #[test]
    fn a_privileged_read_is_refused_without_a_run_policy() {
        // The default posture: the manifest may say anything; with no run
        // policy behind the host, nothing privileged happens.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        std::fs::write(&file, b"top secret").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();
        let mut p = profile();
        p.read_paths = vec![canonical.to_string_lossy().into_owned()];

        let wasm = read_fixture(&canonical.to_string_lossy());
        let out = host()
            .run(&wasm, b"", &p, Arc::new(DenyAllGate), "skill:test")
            .unwrap();
        assert_eq!(out.status, GUEST_REFUSED);
        assert_eq!(out.host_bytes_read, 0);
        assert_eq!(out.denials.len(), 1);
        assert!(out.denials[0].contains("sandbox.no-run-policy"));
    }

    #[test]
    fn a_read_the_manifest_never_declared_is_refused_even_by_a_permissive_policy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        std::fs::write(&file, b"top secret").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();
        // Manifest declares nothing; the run policy would allow anything.
        let wasm = read_fixture(&canonical.to_string_lossy());
        let out = host()
            .run(&wasm, b"", &profile(), Arc::new(AllowAllGate), "skill:test")
            .unwrap();
        assert_eq!(out.status, GUEST_REFUSED);
        assert_eq!(out.host_bytes_read, 0);
        assert!(out.denials[0].contains("sandbox.undeclared-capability"));
    }

    #[test]
    fn a_declared_and_allowed_read_succeeds_and_is_metered() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.txt");
        std::fs::write(&file, b"0123456789").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();
        let mut p = profile();
        p.read_paths = vec![canonical.to_string_lossy().into_owned()];

        let wasm = read_fixture(&canonical.to_string_lossy());
        let out = host()
            .run(&wasm, b"", &p, Arc::new(AllowAllGate), "skill:test")
            .unwrap();
        assert_eq!(out.status, 10, "ten bytes were read");
        assert_eq!(out.host_bytes_read, 10);
        assert!(out.denials.is_empty());
    }

    #[test]
    fn a_missing_file_and_a_denied_file_are_indistinguishable_to_the_guest() {
        // BRIEF rule 2: no enumeration oracle. Both return GUEST_REFUSED.
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("present.txt");
        std::fs::write(&present, b"x").unwrap();
        let canonical = std::fs::canonicalize(&present).unwrap();

        // Present but undeclared.
        let denied = host()
            .run(
                &read_fixture(&canonical.to_string_lossy()),
                b"",
                &profile(),
                Arc::new(AllowAllGate),
                "skill:test",
            )
            .unwrap();
        // Absent entirely.
        let absent_path = dir.path().join("absent.txt");
        let absent = host()
            .run(
                &read_fixture(&absent_path.to_string_lossy()),
                b"",
                &profile(),
                Arc::new(AllowAllGate),
                "skill:test",
            )
            .unwrap();
        assert_eq!(denied.status, absent.status);
        assert_eq!(denied.status, GUEST_REFUSED);
    }

    #[test]
    fn a_traversal_path_cannot_escape_the_granted_root() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        std::fs::create_dir(&allowed).unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, b"secret").unwrap();
        let mut p = profile();
        p.read_paths = vec![std::fs::canonicalize(&allowed)
            .unwrap()
            .to_string_lossy()
            .into_owned()];

        let escape = format!("{}/../outside.txt", allowed.display());
        let out = host()
            .run(
                &read_fixture(&escape),
                b"",
                &p,
                Arc::new(AllowAllGate),
                "skill:test",
            )
            .unwrap();
        assert_eq!(out.status, GUEST_REFUSED);
        assert_eq!(out.host_bytes_read, 0);
    }

    #[test]
    fn zero_ceilings_are_refused_rather_than_meaning_unlimited() {
        let mut limits = WasmLimits::from_profile(&profile());
        limits.fuel = 0;
        assert!(matches!(
            WasmHost::new(limits).unwrap_err(),
            WasmError::InvalidLimits(_)
        ));
        let mut limits = WasmLimits::from_profile(&profile());
        limits.wall = Duration::ZERO;
        assert!(matches!(
            WasmHost::new(limits).unwrap_err(),
            WasmError::InvalidLimits(_)
        ));
    }

    #[test]
    fn limits_are_clamped_to_the_hard_ceilings() {
        let mut p = profile();
        p.memory_mb = u64::MAX;
        p.wall_seconds = u64::MAX;
        p.maximum_output_mb = u64::MAX;
        p.cpu_seconds = u64::MAX;
        let limits = WasmLimits::from_profile(&p);
        assert_eq!(limits.memory_bytes, MAX_MEMORY_BYTES);
        assert_eq!(limits.wall, MAX_WALL);
        assert_eq!(limits.output_bytes, MAX_OUTPUT_BYTES);
        assert_eq!(limits.fuel, MAX_FUEL);
    }
}
