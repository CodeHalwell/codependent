//! Poison-tolerant locking for the per-connection state this crate shares
//! between a client handle and its driver task.
//!
//! `std::sync::Mutex` poisons permanently once a holder panics, so a plain
//! `expect` turns one panicking callback into an ACP session that answers
//! nothing for as long as its agent process lives. Both slots behind this
//! helper degrade toward the safe answer when observed mid-update: handshake
//! discovery is advisory (the agent, not this cache, is the authority on its
//! own catalog — an unknown id fails at the agent), and the active-prompt slot
//! resolves to `RequestPermissionOutcome::Cancelled` when it is empty or its
//! turn has ended, which is the fail-closed direction. Neither can turn a
//! missing permission into a granted one.
//!
//! This mirrors `codypendent_daemon::poison`, which carries the full reasoning;
//! this crate depends only on the protocol crate, hence the second definition
//! rather than a shared one.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock `mutex`, recovering the guard if a previous holder panicked.
pub fn lock_recovering<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
