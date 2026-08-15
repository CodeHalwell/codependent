//! PTY end-to-end testing harness (Adoption 12 A2).
//!
//! Spawns real interactive terminal processes under pseudoterminals (`portable-pty`),
//! captures virtual terminal screen state in `vt100::Parser`, and provides
//! synchronous assertion and keystroke driving APIs for integration testing.

pub mod session;

pub use session::{E2eError, TuiSession};
