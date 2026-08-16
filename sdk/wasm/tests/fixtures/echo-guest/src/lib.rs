//! A guest that exercises the whole SDK surface a plugin normally touches:
//! it reads its input through the two-call sizing protocol, echoes it to the
//! captured log, and returns an outcome code.
//!
//! Built only by `tests/wasm_roundtrip_it.rs`.

use codypendent_wasm_sdk::{export_plugin, input, log};

fn entry() -> Result<(), i32> {
    let bytes = input();
    let text = String::from_utf8(bytes).map_err(|_| 2)?;
    log(&format!("echo:{text}"));
    // A second outcome so the test can prove the return value reaches the host.
    if text == "fail" {
        return Err(3);
    }
    Ok(())
}

export_plugin!(entry);
