//! Codypendent WASM Plugin SDK.
//!
//! Provides idiomatic, safe Rust bindings for the Codypendent WASM guest ABI.
//!
//! # Example
//! ```ignore
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

/// Retrieve the raw input bytes passed to the plugin invocation.
pub fn input() -> Vec<u8> {
    unsafe {
        // Step 1: query length with cap 0
        let len = ffi::input(std::ptr::null_mut(), 0);
        if len <= 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; len as usize];
        let copied = ffi::input(buf.as_mut_ptr(), buf.len());
        if copied > 0 {
            buf.truncate(copied as usize);
            buf
        } else {
            Vec::new()
        }
    }
}

/// Retrieve the input parsed as a UTF-8 string.
pub fn input_string() -> Result<String, std::string::FromUtf8Error> {
    String::from_utf8(input())
}

/// Append a message to the invocation's captured log output.
pub fn log(message: &str) {
    unsafe {
        ffi::log(message.as_ptr(), message.len());
    }
}

/// Read a file within the sandbox's permitted roots.
pub fn read_file(path: &str) -> Result<Vec<u8>, i32> {
    unsafe {
        let mut buf = vec![0u8; 64 * 1024];
        let res = ffi::read_file(path.as_ptr(), path.len(), buf.as_mut_ptr(), buf.len());
        if res >= 0 {
            buf.truncate(res as usize);
            Ok(buf)
        } else {
            Err(res)
        }
    }
}

/// Convenience macro for logging formatted text.
#[macro_export]
macro_rules! wasm_log {
    ($($arg:tt)*) => {
        $crate::log(&format!($($arg)*))
    };
}

/// Export the entrypoint function `run() -> i32`.
#[macro_export]
macro_rules! export_plugin {
    ($entry_fn:expr) => {
        #[no_mangle]
        pub extern "C" fn run() -> i32 {
            match $entry_fn() {
                Ok(_) => 0,
                Err(code) => code as i32,
            }
        }
    };
}
