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
//! | `codypendent_run` | `(i32) -> i32` | yes |
//!
//! The entry point's single `i32` parameter is **reserved**: the host passes `0`
//! today and a guest must accept and ignore it. It is in the signature so a
//! future per-invocation handle can be threaded through without every shipped
//! module becoming unloadable. The arity is checked exactly
//! ([`WasmError::MissingExport`]), so this table is the ABI — a module built to
//! a nullary `codypendent_run` is refused. There is no WASM SDK yet, which is
//! why the table is normative rather than illustrative.
//!
//! A guest may import, from module `codypendent`:
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
//!
//! Every host call is metered before it does anything: it is charged fuel and
//! the wall-clock deadline is checked, and a call made past the deadline
//! terminates the guest (see [`WasmHost::run`]). A host call is otherwise a
//! blind spot in both ceilings — fuel does not advance inside one, and the
//! `OutOfFuel` yield a pure-compute guest hits need never happen for a guest
//! that spends its life in host calls.

use std::path::Path;
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

/// Fuel charged for entering a host function, and per eight bytes it copies.
///
/// The engine does not meter host work at all: a guest that loops on
/// `codypendent::input` consumed 800k fuel in 167 seconds (measured), so
/// neither the fuel budget nor the slice boundary that carries the wall-clock
/// check was ever reached. These charges make a host call cost something, so a
/// host-call loop burns its instruction budget and yields on `OutOfFuel` like
/// any other loop. They are a floor, not a measurement of host time — the
/// deadline check in [`enter_host_call`] is what actually bounds it.
const FUEL_PER_HOST_CALL: u64 = 1_000;
const HOST_BYTES_PER_FUEL: usize = 8;

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
    /// Wall-clock ceiling. Checked between fuel slices **and** on entry to
    /// every host call, and re-checked once the guest returns, so overshoot is
    /// bounded by one fuel slice or one host call — whichever the guest is
    /// inside — rather than by guest behaviour.
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
    /// When this invocation must be over. Carried in the store so a host
    /// function can see it; the guest has no way to read or move it.
    deadline: Instant,
    /// Set by [`enter_host_call`] when it refuses. The trap it returns is
    /// indistinguishable from any other host trap at the `wasmi` boundary, so
    /// the reason is recorded here and read back by [`WasmHost::run`].
    deadline_exceeded: bool,
}

impl HostState {
    /// Record a refusal for the audit trail. The guest never sees this text.
    fn deny(&mut self, request: &HostRequest, reason: &str) -> i32 {
        self.denials
            .push(format!("{} refused: {reason}", request.describe()));
        GUEST_REFUSED
    }
}

/// Charge `fuel` against the store's remaining budget, saturating at zero.
///
/// Zero is not a failure here: the guest traps `OutOfFuel` on its next
/// instruction, which is a resumable yield the run loop already handles (and
/// where it decides whether the total budget is exhausted).
fn charge_fuel(caller: &mut Caller<'_, HostState>, fuel: u64) {
    let remaining = caller.get_fuel().unwrap_or(0);
    let _ = caller.set_fuel(remaining.saturating_sub(fuel));
}

/// The prologue **every** host function runs before doing anything.
///
/// Fuel is not consumed while execution is inside a host function, and a guest
/// that spends its life in host calls need never reach the `OutOfFuel` slice
/// boundary where the wall clock used to be checked — measured, a guest with
/// zero declared permissions ran 166.8 s against a declared 1 s ceiling and the
/// host reported `Ok`. Checking the deadline here closes that: the check is
/// outside the guest, the guest cannot skip a host call it is making, and
/// exceeding the ceiling **terminates** the invocation with an error rather
/// than letting it complete.
fn enter_host_call(caller: &mut Caller<'_, HostState>) -> Result<(), wasmi::Error> {
    charge_fuel(caller, FUEL_PER_HOST_CALL);
    let state = caller.data_mut();
    if Instant::now() >= state.deadline {
        state.deadline_exceeded = true;
        return Err(wasmi::Error::new(
            "wasm guest exceeded its wall-clock ceiling during a host call",
        ));
    }
    Ok(())
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
fn host_input(mut caller: Caller<'_, HostState>, ptr: i32, cap: i32) -> Result<i32, wasmi::Error> {
    enter_host_call(&mut caller)?;
    let Some(memory) = guest_memory(&mut caller) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    let Ok(cap) = usize::try_from(cap) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    let Ok(ptr) = usize::try_from(ptr) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    let input = caller.data().input.clone();
    let full = input.len();
    let n = cap.min(full);
    charge_fuel(&mut caller, (n / HOST_BYTES_PER_FUEL) as u64);
    // A zero-capacity call is the documented "how big is my input?" query, so
    // it must not fail on a null pointer.
    if n > 0 && memory.write(&mut caller, ptr, &input[..n]).is_err() {
        return Ok(GUEST_BAD_ARGUMENT);
    }
    Ok(i32::try_from(full).unwrap_or(i32::MAX))
}

/// `codypendent::log` — append to the captured output. Unprivileged, but capped:
/// a guest cannot exhaust host memory by logging.
fn host_log(mut caller: Caller<'_, HostState>, ptr: i32, len: i32) -> Result<(), wasmi::Error> {
    enter_host_call(&mut caller)?;
    let Some(memory) = guest_memory(&mut caller) else {
        return Ok(());
    };
    let Ok(len) = usize::try_from(len) else {
        return Ok(());
    };
    let room = {
        let state = caller.data();
        state.output_cap.saturating_sub(state.output.len())
    };
    // Clamp rather than refuse: an over-long write is truncated and flagged, so
    // a guest cannot suppress the truncation signal by writing one huge buffer
    // instead of many small ones. `room` bounds the host-side allocation.
    let wanted = len.min(room);
    charge_fuel(&mut caller, (wanted / HOST_BYTES_PER_FUEL) as u64);
    let Some(bytes) = read_guest_bytes(&caller, &memory, ptr, wanted, room) else {
        return Ok(());
    };
    let state = caller.data_mut();
    state.output.extend_from_slice(&bytes);
    if len > wanted {
        state.output_truncated = true;
    }
    Ok(())
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
) -> Result<i32, wasmi::Error> {
    enter_host_call(&mut caller)?;
    let Some(memory) = guest_memory(&mut caller) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    // A path longer than this is not a path.
    let Ok(path_len) = usize::try_from(path_len) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    let Some(raw) = read_guest_bytes(&caller, &memory, path_ptr, path_len, 4096) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    let Ok(requested) = String::from_utf8(raw) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    let (Ok(out_ptr), Ok(out_cap)) = (usize::try_from(out_ptr), usize::try_from(out_cap)) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };

    // Resolve first, then decide, then open the resolved path. A missing file
    // and a denied file return the SAME code (`GUEST_REFUSED`) so the guest
    // cannot use this call to probe for the existence of paths it may not read.
    let Ok(resolved) = std::fs::canonicalize(&requested) else {
        let request = HostRequest::ReadFile { path: requested };
        return Ok(caller.data_mut().deny(&request, "unresolvable path"));
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
            return Ok(caller.data_mut().deny(&request, &reason));
        }
    };
    debug_assert!(
        grant.answers(&request),
        "broker checks this before returning"
    );

    let budget = caller.data().host_read_remaining;
    if budget == 0 {
        return Ok(caller
            .data_mut()
            .deny(&request, "host-read budget exhausted"));
    }
    let bytes = match read_capped(&resolved, budget.min(out_cap)) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(caller.data_mut().deny(&request, "read failed")),
    };
    // Account BEFORE the copy into guest memory, not after. The bytes have
    // already come off the disk; whether the guest's destination pointer was
    // valid is its problem, not the budget's. Returning early on a bad
    // `out_ptr` skipped this and bought unmetered reads — measured, 500 calls
    // against a granted 20 MB file pulled 9.3 GiB with `host_bytes_read = 0`
    // and a 64 MiB ceiling that never engaged.
    charge_fuel(&mut caller, (bytes.len() / HOST_BYTES_PER_FUEL) as u64);
    let state = caller.data_mut();
    state.host_read_remaining = state.host_read_remaining.saturating_sub(bytes.len());
    state.host_bytes_read += bytes.len();
    if memory.write(&mut caller, out_ptr, &bytes).is_err() {
        return Ok(GUEST_BAD_ARGUMENT);
    }
    Ok(i32::try_from(bytes.len()).unwrap_or(i32::MAX))
}

/// Read a brokered secret reference.
///
/// The guest supplies the secret name. The request is authorized against the
/// declaration ceiling and run policy gate via `CapabilityBroker`. On grant,
/// the secret name reference is returned into the guest's buffer.
fn host_read_secret(
    mut caller: Caller<'_, HostState>,
    name_ptr: i32,
    name_len: i32,
    out_ptr: i32,
    out_cap: i32,
) -> Result<i32, wasmi::Error> {
    enter_host_call(&mut caller)?;
    let Some(memory) = guest_memory(&mut caller) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    let Ok(name_len) = usize::try_from(name_len) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    let Some(raw) = read_guest_bytes(&caller, &memory, name_ptr, name_len, 256) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    let Ok(requested) = String::from_utf8(raw) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };
    let (Ok(out_ptr), Ok(out_cap)) = (usize::try_from(out_ptr), usize::try_from(out_cap)) else {
        return Ok(GUEST_BAD_ARGUMENT);
    };

    let request = HostRequest::ReadSecret {
        name: requested.clone(),
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
            return Ok(caller.data_mut().deny(&request, &reason));
        }
    };
    debug_assert!(
        grant.answers(&request),
        "broker checks this before returning"
    );

    let bytes = requested.as_bytes();
    let n = bytes.len().min(out_cap);
    charge_fuel(&mut caller, (n / HOST_BYTES_PER_FUEL) as u64);
    if memory.write(&mut caller, out_ptr, &bytes[..n]).is_err() {
        return Ok(GUEST_BAD_ARGUMENT);
    }
    Ok(i32::try_from(n).unwrap_or(i32::MAX))
}

/// Read at most `cap` bytes. Never allocates on the file's own claimed size, so
/// a hostile symlink to `/dev/zero` cannot exhaust host memory.
fn read_capped(path: &Path, cap: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;

    // Open FIRST, then ask every question of the OPEN DESCRIPTOR.
    //
    // The previous shape was stat-then-open-by-pathname, and the caller had
    // already canonicalized-then-passed-the-string. Both are check-then-use on a
    // name, and a name is not a file: between the broker authorizing
    // `/granted/dir/f` and this function opening it, another process can replace
    // `/granted/dir` with a symlink to `/etc`, and the bytes returned come from
    // outside every granted root the manifest declares. Canonicalizing does not
    // close that window — it only decides which name gets raced.
    //
    // `open_resolved_path` therefore walks the (already canonical) path one
    // component at a time with `O_NOFOLLOW` on every component, so a symlink
    // substituted at ANY position after canonicalization makes the open fail
    // instead of silently redirecting it. What is read is the handle that walk
    // produced, never a re-resolution of the string.
    let file = open_resolved_path(path)?;

    // REGULAR FILES ONLY. Fuel is not consumed while execution is inside a host
    // function, and the wall deadline is only observed when the guest yields on
    // `OutOfFuel`, so a host read that blocks is bounded by nothing at all: a
    // FIFO with no writer, a character device, or a stalled network mount would
    // hang the invocation forever.
    //
    // The check now runs on the descriptor (`fstat`), not on the path, so it
    // describes the exact object about to be read rather than whatever the name
    // pointed at a moment ago. The open above is non-blocking precisely so this
    // ordering is possible — `open(fifo, O_RDONLY)` without `O_NONBLOCK` sleeps
    // in the kernel until a writer arrives, which is why the original code had
    // to check before opening.
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }

    let mut buffer = Vec::new();
    file.take(cap as u64).read_to_end(&mut buffer)?;
    Ok(buffer)
}

/// Open an **already canonical, absolute** path without following a symlink at
/// any component, and without blocking on a FIFO or device.
///
/// `openat` from the root down with `O_NOFOLLOW` at every step. `path` comes
/// from `std::fs::canonicalize`, so it contains no `.`, no `..` and no symlink
/// *at the moment it was resolved*; any symlink this walk meets is therefore one
/// that appeared afterwards — exactly the substitution being refused. An
/// `ELOOP` here is a race lost, not a misconfiguration, and it fails closed.
#[cfg(unix)]
fn open_resolved_path(path: &Path) -> std::io::Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags};
    use std::path::Component;

    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "host read path is not absolute",
        ));
    }
    let names: Vec<&std::ffi::OsStr> = components
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            // `canonicalize` never emits these. One appearing means the path was
            // not canonical, so the broker's decision was made about a different
            // string than the one being opened.
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "host read path is not canonical",
            )),
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let Some((last, parents)) = names.split_last() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "host read path names no file",
        ));
    };

    let mut dir = rustix::fs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    for name in parents {
        dir = rustix::fs::openat(
            &dir,
            *name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
    }
    let file = rustix::fs::openat(
        &dir,
        *last,
        // `NONBLOCK` so a FIFO fixture cannot park this thread in `open`; the
        // regular-file check on the returned descriptor rejects it immediately
        // afterwards.
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    Ok(std::fs::File::from(file))
}

/// Non-Unix fallback. There is no `openat`/`O_NOFOLLOW` equivalent reachable
/// without a platform dependency this crate does not carry, so the symlink-swap
/// window described above is NOT closed here. Stated rather than papered over:
/// the WASM host is only built and exercised on Unix in this workspace.
#[cfg(not(unix))]
fn open_resolved_path(path: &PathBuf) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
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
            let known =
                m == "codypendent" && matches!(n, "input" | "log" | "read_file" | "read_secret");
            if !known {
                return Err(WasmError::ForbiddenImport {
                    module: m.to_string(),
                    name: n.to_string(),
                });
            }
        }

        let deadline = started + self.limits.wall;
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
            deadline,
            deadline_exceeded: false,
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
            linker.func_wrap("codypendent", "read_secret", host_read_secret)?;
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

        let mut outputs = [Val::I32(0)];
        let mut remaining = self.limits.fuel;
        let mut granted = remaining.min(FUEL_SLICE);
        remaining -= granted;
        store
            .set_fuel(granted)
            .map_err(|e| WasmError::Runtime(e.to_string()))?;

        let mut call = entry
            .call_resumable(&mut store, &[Val::I32(0)], &mut outputs)
            .map_err(|e| self.classify_trap(&store, e))?;
        let mut consumed: u64 = 0;
        loop {
            match call {
                ResumableCall::Finished => {
                    consumed += granted.saturating_sub(store.get_fuel().unwrap_or(0));
                    break;
                }
                ResumableCall::HostTrap(trap) => {
                    // The only host function that returns an error is one whose
                    // prologue refused, so this is where a guest that outlived
                    // its wall clock inside a host call is terminated.
                    if store.data().deadline_exceeded {
                        return Err(WasmError::WallClockExceeded {
                            wall: self.limits.wall,
                        });
                    }
                    return Err(WasmError::Trap(format!(
                        "host function trapped: {}",
                        trap.host_error()
                    )));
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
                        .map_err(|e| self.classify_trap(&store, e))?;
                }
            }
        }

        // A run that overran its declared ceiling is a refusal even when the
        // guest returned normally: a single long host call could otherwise
        // finish past the deadline and be reported as a successful completion,
        // which is what made a 167x overshoot read as `Ok`.
        if started.elapsed() >= self.limits.wall {
            return Err(WasmError::WallClockExceeded {
                wall: self.limits.wall,
            });
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
    /// escapes a host boundary, so it is recognised here too — as is a host
    /// call refused by [`enter_host_call`], whose reason lives in the store
    /// because `wasmi` gives every host error the same shape.
    fn classify_trap(&self, store: &Store<HostState>, error: wasmi::Error) -> WasmError {
        if store.data().deadline_exceeded {
            return WasmError::WallClockExceeded {
                wall: self.limits.wall,
            };
        }
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

    /// A FIFO with no writer blocks `read` forever in the kernel. Fuel is not
    /// consumed inside a host call and the wall deadline is only observed when
    /// the guest yields on `OutOfFuel`, so nothing would ever terminate that
    /// invocation. `read_capped` refuses anything that is not a regular file,
    /// which is why this test can complete at all.
    #[test]
    #[cfg(unix)]
    fn a_fifo_is_refused_rather_than_blocking_forever() {
        use std::os::unix::fs::FileTypeExt as _;

        // `read_capped` takes an already-canonical path (its caller passes the
        // string the broker authorized) and refuses to traverse a symlink at any
        // component. On macOS `/var` is itself a symlink to `/private/var`, so
        // the fixture root has to be canonical for the walk to be about the
        // substitution under test rather than about the temp dir.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical tempdir");
        let fifo = root.join("pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed");
        assert!(
            std::fs::metadata(&fifo)
                .expect("stat fifo")
                .file_type()
                .is_fifo(),
            "precondition: the fixture really is a FIFO"
        );

        // Opening a read-only FIFO does not block; reading it does. Without the
        // regular-file check this call never returns.
        let refused = super::read_capped(&fifo, 1024);
        assert!(
            refused.is_err(),
            "a FIFO must be refused, not read — an unbounded host read is not \
             covered by the fuel or wall-clock limits"
        );

        let ordinary = root.join("input.txt");
        std::fs::write(&ordinary, b"hello").expect("write");
        assert_eq!(
            super::read_capped(&ordinary, 1024).expect("regular file reads"),
            b"hello"
        );
    }
    /// A parent directory swapped for a symlink AFTER the path was resolved must
    /// not redirect the read outside the granted roots.
    ///
    /// This is the shape the host call had: canonicalize, decide, then open by
    /// pathname. Here the decision is simulated by canonicalizing while
    /// `granted/inner` is a real directory; the swap then happens before the
    /// read, exactly as a racing process would do it. `read_capped` must fail
    /// rather than return `outside/secret`'s bytes.
    #[test]
    #[cfg(unix)]
    fn a_parent_swapped_for_a_symlink_after_resolution_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical tempdir");

        let granted = root.join("granted");
        let inner = granted.join("inner");
        std::fs::create_dir_all(&inner).expect("create granted/inner");
        std::fs::write(inner.join("f"), b"authorized").expect("write authorized file");

        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::fs::write(outside.join("f"), b"SECRET").expect("write secret");

        // What the broker would have authorized.
        let resolved = inner.join("f").canonicalize().expect("canonicalize");
        assert_eq!(
            super::read_capped(&resolved, 1024).expect("reads before the swap"),
            b"authorized"
        );

        // The race: `granted/inner` becomes a symlink to `outside`.
        std::fs::remove_file(inner.join("f")).expect("remove");
        std::fs::remove_dir(&inner).expect("remove inner");
        std::os::unix::fs::symlink(&outside, &inner).expect("plant symlink");

        let raced = super::read_capped(&resolved, 1024);
        assert!(
            raced.is_err(),
            "a symlink substituted at a parent component after resolution must \
             refuse the open, not read {:?}",
            raced
                .as_ref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        );
    }

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
