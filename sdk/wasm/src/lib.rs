//! Codypendent WASM Plugin SDK.
//!
//! Idiomatic, safe Rust bindings for the Codypendent WASM guest ABI. The ABI
//! itself is normative in `crates/sandbox/src/wasm.rs`; this crate is a
//! *binding* to it, so every signature below is copied from that module's table
//! and must not drift from it.
//!
//! # The contract this crate implements
//!
//! A guest module must export:
//!
//! | export | signature | required |
//! |---|---|---|
//! | `memory` | linear memory | yes |
//! | `codypendent_run` | `(i32) -> i32` | yes |
//!
//! [`export_plugin!`] generates `codypendent_run`. The single `i32` parameter is
//! **reserved**: the host passes `0` today and a guest must accept and ignore
//! it, so that a future per-invocation handle can be threaded through without
//! every shipped module becoming unloadable. The host checks the arity
//! *exactly* — a nullary `codypendent_run`, or one named `run`, is refused at
//! load with `MissingExport`. The returned `i32` is the outcome code: `0` is
//! success by convention, anything else is a failure the host records but does
//! not interpret.
//!
//! The `memory` export is not something this crate can emit for you: it comes
//! from building a `cdylib` for `wasm32-unknown-unknown`. A plugin crate
//! therefore **must** declare
//!
//! ```toml
//! [lib]
//! crate-type = ["cdylib"]
//! ```
//!
//! and build with `cargo build --target wasm32-unknown-unknown --release`. An
//! `rlib`-only build exports nothing and is refused with
//! `MissingExport("memory")` — the failure looks like a host bug and is not, so
//! it is called out here rather than left to be discovered.
//!
//! A guest may import, from module `codypendent`:
//!
//! | import | signature | privileged |
//! |---|---|---|
//! | `input` | `(ptr: i32, cap: i32) -> i32` | no |
//! | `log` | `(ptr: i32, len: i32)` | no |
//! | `read_file` | `(path_ptr, path_len, out_ptr, out_cap) -> i32` | **yes** |
//!
//! Nothing else links. There is no WASI, no clock, no randomness, no allocator
//! contract with the host: the host never writes into guest memory unbidden.
//!
//! `input` returns the input's **true length**, not the number of bytes it
//! copied, so the documented way to read it is two calls: once with `cap = 0`
//! to size the buffer, once with a buffer that size. [`input`] does exactly
//! that.
//!
//! `read_file` is the only privileged call, and the only one that can be
//! refused: it goes through the capability broker, so it fails unless the
//! package manifest declared the path **and** the run policy allows it. A
//! denial and a missing file return the identical code ([`REFUSED`]) on
//! purpose — the host surface is not an existence oracle for paths the guest
//! may not read.
//!
//! # Example
//!
//! ```ignore
//! // `ignore`, not a doctest: these bindings only resolve on
//! // wasm32-unknown-unknown, where the imports are provided by the host.
//! use codypendent_wasm_sdk::{export_plugin, input_string, log};
//!
//! fn main_plugin() -> Result<(), i32> {
//!     let input = input_string().map_err(|_| 1)?;
//!     log(&format!("Processing input: {input}"));
//!     Ok(())
//! }
//!
//! export_plugin!(main_plugin);
//! ```

/// A privileged call was refused, or its target does not exist. The host
/// returns one code for both so the guest cannot probe for paths it may not
/// read.
pub const REFUSED: i32 = -1;

/// The host could not make sense of the arguments — an out-of-range pointer, a
/// length that is not a length, or a path that is not UTF-8.
pub const BAD_ARGUMENT: i32 = -2;

/// Starting buffer size for [`read_file`], and the growth step below it.
const READ_FILE_INITIAL_CAP: usize = 64 * 1024;

/// Ceiling for [`read_file`]'s growth loop. Matches the host's own
/// `MAX_HOST_READ_BYTES`, so the loop stops where the host would have stopped
/// serving it anyway.
const READ_FILE_MAX_CAP: usize = 64 * 1024 * 1024;

/// The raw host imports, declared exactly as the ABI table above spells them.
///
/// On `wasm32` a `*mut u8` and a `usize` both lower to `i32`, so these
/// declarations *are* the table; they are written with Rust pointer types
/// because the safe wrappers below are the only callers.
#[cfg(target_arch = "wasm32")]
mod ffi {
    #[link(wasm_import_module = "codypendent")]
    extern "C" {
        pub fn input(ptr: *mut u8, cap: usize) -> i32;
        pub fn log(ptr: *const u8, len: usize);
        pub fn read_file(
            path_ptr: *const u8,
            path_len: usize,
            out_ptr: *mut u8,
            out_cap: usize,
        ) -> i32;
    }
}

/// Off-target stand-ins for the host imports.
///
/// The crate is a workspace member, so it is compiled for the host on every
/// `cargo test`/`cargo clippy` run, where no host provides `codypendent::*`.
/// Under `cfg(test)` these route to [`test_host`], a faithful re-implementation
/// of the host functions in `crates/sandbox/src/wasm.rs` — which is what lets
/// the two-call sizing protocol be tested at all without a wasm toolchain.
/// Outside tests they panic: a plugin built for anything but
/// `wasm32-unknown-unknown` is not a plugin.
#[cfg(not(target_arch = "wasm32"))]
mod ffi {
    #[cfg(not(test))]
    fn off_target(name: &str) -> ! {
        panic!("codypendent host import `{name}` is only available on wasm32-unknown-unknown");
    }

    pub unsafe fn input(ptr: *mut u8, cap: usize) -> i32 {
        #[cfg(test)]
        {
            crate::test_host::input(ptr, cap)
        }
        #[cfg(not(test))]
        {
            let _ = (ptr, cap);
            off_target("input")
        }
    }

    pub unsafe fn log(ptr: *const u8, len: usize) {
        #[cfg(test)]
        {
            crate::test_host::log(ptr, len);
        }
        #[cfg(not(test))]
        {
            let _ = (ptr, len);
            off_target("log")
        }
    }

    pub unsafe fn read_file(
        path_ptr: *const u8,
        path_len: usize,
        out_ptr: *mut u8,
        out_cap: usize,
    ) -> i32 {
        #[cfg(test)]
        {
            crate::test_host::read_file(path_ptr, path_len, out_ptr, out_cap)
        }
        #[cfg(not(test))]
        {
            let _ = (path_ptr, path_len, out_ptr, out_cap);
            off_target("read_file")
        }
    }
}

/// Retrieve the raw input bytes passed to the plugin invocation.
///
/// Implements the host's two-call sizing protocol: `input(_, 0)` returns the
/// input's true length without writing anything, then a second call with a
/// buffer of that size fills it. A host that reports a length larger than the
/// buffer we handed it is clamped rather than trusted, so a length/copy
/// disagreement can never produce a `Vec` whose claimed length exceeds its
/// initialised bytes.
///
/// Returns an empty vector when there is no input, or when the host refuses
/// (a negative code, which for this call means the module exported no
/// `memory`).
#[must_use]
pub fn input() -> Vec<u8> {
    // Step 1: size the buffer. A zero capacity is the documented query, and the
    // host does not write for it, so the null pointer is safe.
    let len = unsafe { ffi::input(std::ptr::null_mut(), 0) };
    let Ok(len) = usize::try_from(len) else {
        return Vec::new();
    };
    if len == 0 {
        return Vec::new();
    }

    // Step 2: fill it. The return is the true length again, not a copied count;
    // clamp to what we actually provided.
    let mut buf = vec![0u8; len];
    let reported = unsafe { ffi::input(buf.as_mut_ptr(), buf.len()) };
    let Ok(reported) = usize::try_from(reported) else {
        return Vec::new();
    };
    buf.truncate(reported.min(len));
    buf
}

/// Retrieve the input parsed as a UTF-8 string.
///
/// # Errors
///
/// Returns [`std::string::FromUtf8Error`] when the invocation's input is not
/// valid UTF-8. Input is untrusted, so this is a real case, not a formality.
pub fn input_string() -> Result<String, std::string::FromUtf8Error> {
    String::from_utf8(input())
}

/// Append a message to the invocation's captured log output.
///
/// The host caps total captured output and flags the truncation; a guest cannot
/// evade that cap and cannot observe it.
pub fn log(message: &str) {
    unsafe {
        ffi::log(message.as_ptr(), message.len());
    }
}

/// Read a file within the sandbox's permitted roots, into a caller-sized
/// buffer.
///
/// This is the exact ABI call: one host call, at most `capacity` bytes. Unlike
/// `input`, `read_file` returns the number of bytes **copied**, not the file's
/// true size, so a result of exactly `capacity` is ambiguous — the file may
/// have been longer and silently truncated. Use this when you know the size you
/// want; use [`read_file`] when you do not.
///
/// # Errors
///
/// [`REFUSED`] when the path was denied by policy, does not exist, is not a
/// regular file, or the invocation's host-read budget is exhausted — the host
/// deliberately returns one code for all of these. [`BAD_ARGUMENT`] when the
/// path or the output buffer could not be interpreted.
pub fn read_file_with_capacity(path: &str, capacity: usize) -> Result<Vec<u8>, i32> {
    let mut buf = vec![0u8; capacity];
    let res = unsafe { ffi::read_file(path.as_ptr(), path.len(), buf.as_mut_ptr(), buf.len()) };
    let Ok(copied) = usize::try_from(res) else {
        return Err(res);
    };
    buf.truncate(copied.min(capacity));
    Ok(buf)
}

/// Read a file within the sandbox's permitted roots.
///
/// `read_file` has no sizing query — it reports bytes copied, not the file's
/// length — so a fixed buffer truncates a larger file without saying so. This
/// grows instead: it starts at 64 KiB and doubles while the host fills the
/// buffer exactly, up to the host's own 64 MiB host-read ceiling.
///
/// Each retry re-reads the file and is charged again against the invocation's
/// host-read budget, so pass a known size to [`read_file_with_capacity`] when
/// you have one.
///
/// # Errors
///
/// As [`read_file_with_capacity`].
pub fn read_file(path: &str) -> Result<Vec<u8>, i32> {
    let mut capacity = READ_FILE_INITIAL_CAP;
    loop {
        let bytes = read_file_with_capacity(path, capacity)?;
        // A short read is unambiguous: the file ended, or the host's budget did.
        if bytes.len() < capacity || capacity >= READ_FILE_MAX_CAP {
            return Ok(bytes);
        }
        capacity = capacity.saturating_mul(2).min(READ_FILE_MAX_CAP);
    }
}

/// Convenience macro for logging formatted text.
#[macro_export]
macro_rules! wasm_log {
    ($($arg:tt)*) => {
        $crate::log(&format!($($arg)*))
    };
}

/// Export the guest entry point the host looks for.
///
/// Generates:
///
/// ```ignore
/// #[no_mangle]
/// pub extern "C" fn codypendent_run(_reserved: i32) -> i32
/// ```
///
/// The name and the arity are both load-bearing: the host resolves the export
/// by the exact name `codypendent_run` and checks it takes exactly one `i32`
/// and returns exactly one `i32`, refusing the module otherwise. The parameter
/// is reserved by the host ABI — it is always `0` today and must be ignored.
///
/// `$entry_fn` is anything callable as `() -> Result<_, E>` where `E: Into<i32>`
/// via `as`: `Ok` becomes the outcome code `0`, `Err(code)` becomes `code`.
///
/// Remember that the crate must be built as a `cdylib` for
/// `wasm32-unknown-unknown`, or the required `memory` export is absent and the
/// host refuses the module even though this entry point is present.
#[macro_export]
macro_rules! export_plugin {
    ($entry_fn:expr) => {
        /// The host ABI entry point. `_reserved` is passed as `0` by the host
        /// today and must be ignored; it exists so a future per-invocation
        /// handle does not require an ABI break.
        #[no_mangle]
        pub extern "C" fn codypendent_run(_reserved: i32) -> i32 {
            match $entry_fn() {
                Ok(_) => 0,
                Err(code) => code as i32,
            }
        }
    };
}

/// A faithful stand-in for the host functions in `crates/sandbox/src/wasm.rs`,
/// used only by this crate's own tests.
///
/// It reproduces the semantics the wrappers depend on — in particular that
/// `input` returns the input's *true length* regardless of `cap`, while
/// `read_file` returns *bytes copied* — so a wrapper that confuses the two
/// fails here rather than in a shipped plugin.
#[cfg(test)]
mod test_host {
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct Host {
        pub input: Vec<u8>,
        pub output: Vec<u8>,
        pub files: Vec<(String, Vec<u8>)>,
        /// Every `cap` the guest asked `input` for, in order.
        pub input_caps: Vec<usize>,
        /// Every `out_cap` the guest asked `read_file` for, in order.
        pub read_caps: Vec<usize>,
        /// Bytes the host will serve per `read_file` call before its budget
        /// clamps the copy. `usize::MAX` means "unbudgeted".
        pub read_budget: usize,
    }

    thread_local! {
        static HOST: RefCell<Host> = RefCell::new(Host {
            read_budget: usize::MAX,
            ..Host::default()
        });
    }

    /// Reset the host and install `f`'s configuration for one test.
    pub fn with_host<T>(setup: impl FnOnce(&mut Host), body: impl FnOnce() -> T) -> (T, Host) {
        HOST.with(|h| {
            let mut h = h.borrow_mut();
            *h = Host {
                read_budget: usize::MAX,
                ..Host::default()
            };
            setup(&mut h);
        });
        let out = body();
        let host = HOST.with(|h| std::mem::take(&mut *h.borrow_mut()));
        (out, host)
    }

    /// Mirrors `host_input`: copies `min(cap, len)` bytes and returns the input's
    /// TRUE length. Never writes for `cap == 0`.
    pub unsafe fn input(ptr: *mut u8, cap: usize) -> i32 {
        HOST.with(|h| {
            let mut h = h.borrow_mut();
            h.input_caps.push(cap);
            let full = h.input.len();
            let n = cap.min(full);
            if n > 0 {
                std::ptr::copy_nonoverlapping(h.input.as_ptr(), ptr, n);
            }
            i32::try_from(full).unwrap_or(i32::MAX)
        })
    }

    /// Mirrors `host_log`: appends the bytes to the captured output.
    pub unsafe fn log(ptr: *const u8, len: usize) {
        let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
        HOST.with(|h| h.borrow_mut().output.extend_from_slice(&bytes));
    }

    /// Mirrors `host_read_file`: copies `min(out_cap, budget, len)` bytes and
    /// returns the number COPIED, with one code for "denied" and "missing".
    pub unsafe fn read_file(
        path_ptr: *const u8,
        path_len: usize,
        out_ptr: *mut u8,
        out_cap: usize,
    ) -> i32 {
        let path = String::from_utf8(std::slice::from_raw_parts(path_ptr, path_len).to_vec());
        let Ok(path) = path else {
            return crate::BAD_ARGUMENT;
        };
        HOST.with(|h| {
            let mut h = h.borrow_mut();
            h.read_caps.push(out_cap);
            let Some((_, contents)) = h.files.iter().find(|(p, _)| *p == path) else {
                return crate::REFUSED;
            };
            let n = out_cap.min(h.read_budget).min(contents.len());
            if n > 0 {
                std::ptr::copy_nonoverlapping(contents.as_ptr(), out_ptr, n);
            }
            i32::try_from(n).unwrap_or(i32::MAX)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::test_host::with_host;
    use super::*;

    /// The regression this whole module exists for: the macro used to emit
    /// `run() -> i32`, which the host refuses with
    /// `MissingExport("codypendent_run")` — every SDK-built plugin was
    /// unloadable. Coercing the generated item to the host's exact function
    /// type makes a wrong name or a wrong arity a *compile* error, which is the
    /// only place a symbol contract can be checked without a wasm toolchain.
    mod abi_contract {
        use std::cell::Cell;

        thread_local! {
            static OUTCOME: Cell<Result<(), i32>> = const { Cell::new(Ok(())) };
        }

        fn entry() -> Result<(), i32> {
            OUTCOME.with(Cell::get)
        }

        crate::export_plugin!(entry);

        /// Name and signature. If `export_plugin!` emits anything other than
        /// `codypendent_run(i32) -> i32`, this line does not compile.
        const HOST_ENTRY: extern "C" fn(i32) -> i32 = codypendent_run;

        #[test]
        fn generated_entry_point_has_the_host_abi_signature() {
            OUTCOME.with(|o| o.set(Ok(())));
            // The host passes 0 for the reserved parameter and reads the result
            // as the outcome code.
            assert_eq!(HOST_ENTRY(0), 0, "Ok must map to the success code 0");
        }

        #[test]
        fn generated_entry_point_forwards_the_error_code() {
            OUTCOME.with(|o| o.set(Err(7)));
            assert_eq!(HOST_ENTRY(0), 7);
            OUTCOME.with(|o| o.set(Ok(())));
        }

        #[test]
        fn reserved_parameter_is_ignored() {
            OUTCOME.with(|o| o.set(Ok(())));
            // The host passes 0 today; a future handle must not change the
            // guest's behaviour.
            assert_eq!(HOST_ENTRY(0), HOST_ENTRY(1234));
        }
    }

    #[test]
    fn input_uses_the_two_call_sizing_protocol() {
        let (got, host) = with_host(|h| h.input = b"hello sandbox".to_vec(), input);
        assert_eq!(got, b"hello sandbox");
        // Exactly two calls: size, then fill with a buffer of that exact size.
        assert_eq!(host.input_caps, vec![0, 13]);
    }

    #[test]
    fn input_reads_the_whole_input_not_a_fixed_prefix() {
        // A wrapper with a fixed buffer passes the small cases and silently
        // truncates here.
        let big = vec![b'x'; 300 * 1024];
        let (got, host) = with_host(|h| h.input = big.clone(), input);
        assert_eq!(got.len(), big.len());
        assert_eq!(got, big);
        assert_eq!(host.input_caps, vec![0, 300 * 1024]);
    }

    #[test]
    fn empty_input_costs_one_call_and_no_buffer() {
        let (got, host) = with_host(|_| {}, input);
        assert!(got.is_empty());
        assert_eq!(host.input_caps, vec![0], "the sizing call answers it");
    }

    #[test]
    fn input_string_decodes_utf8_and_reports_invalid_bytes() {
        let (got, _) = with_host(|h| h.input = "héllo".as_bytes().to_vec(), input_string);
        assert_eq!(got.unwrap(), "héllo");

        let (got, _) = with_host(|h| h.input = vec![0xff, 0xfe], input_string);
        assert!(got.is_err(), "untrusted input is not assumed to be UTF-8");
    }

    #[test]
    fn log_appends_to_captured_output() {
        let ((), host) = with_host(
            |_| {},
            || {
                log("first");
                log(" second");
            },
        );
        assert_eq!(host.output, b"first second");
    }

    #[test]
    fn wasm_log_macro_formats() {
        let ((), host) = with_host(|_| {}, || wasm_log!("n={}", 42));
        assert_eq!(host.output, b"n=42");
    }

    #[test]
    fn read_file_returns_exact_contents() {
        let (got, host) = with_host(
            |h| {
                h.files
                    .push(("/allowed/a.txt".into(), b"contents".to_vec()))
            },
            || read_file("/allowed/a.txt"),
        );
        assert_eq!(got.unwrap(), b"contents");
        assert_eq!(host.read_caps, vec![READ_FILE_INITIAL_CAP]);
    }

    #[test]
    fn read_file_grows_past_the_initial_buffer() {
        // The old wrapper used a fixed 64 KiB buffer and truncated silently.
        let big = vec![b'z'; READ_FILE_INITIAL_CAP * 3 + 11];
        let (got, host) = with_host(
            |h| h.files.push(("/allowed/big.bin".into(), big.clone())),
            || read_file("/allowed/big.bin"),
        );
        assert_eq!(got.unwrap().len(), big.len());
        // 64 KiB filled exactly -> 128 KiB filled exactly -> 256 KiB short.
        assert_eq!(
            host.read_caps,
            vec![
                READ_FILE_INITIAL_CAP,
                READ_FILE_INITIAL_CAP * 2,
                READ_FILE_INITIAL_CAP * 4
            ]
        );
    }

    #[test]
    fn read_file_stops_when_the_host_budget_clamps_the_copy() {
        let big = vec![b'z'; READ_FILE_INITIAL_CAP * 4];
        let (got, host) = with_host(
            |h| {
                h.files.push(("/allowed/big.bin".into(), big));
                h.read_budget = 1024;
            },
            || read_file("/allowed/big.bin"),
        );
        assert_eq!(got.unwrap().len(), 1024);
        assert_eq!(
            host.read_caps.len(),
            1,
            "a short read must not be retried forever"
        );
    }

    #[test]
    fn read_file_with_capacity_makes_exactly_one_call() {
        let (got, host) = with_host(
            |h| h.files.push(("/allowed/a.txt".into(), b"abcdef".to_vec())),
            || read_file_with_capacity("/allowed/a.txt", 3),
        );
        assert_eq!(got.unwrap(), b"abc");
        assert_eq!(host.read_caps, vec![3]);
    }

    #[test]
    fn denied_and_missing_files_share_one_error_code() {
        let (got, _) = with_host(|_| {}, || read_file("/forbidden/secret"));
        assert_eq!(got.unwrap_err(), REFUSED);
    }
}
