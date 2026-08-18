//! Poison-tolerant locking for this crate's in-memory caches.
//!
//! `std::sync::Mutex` poisons permanently once a holder panics, so a plain
//! `unwrap()` turns one panicking query into an LSP layer that is dead for the
//! daemon's whole lifetime. Every map behind this helper is a cache over a
//! re-derivable source — a spawned server handle, a "this server is broken"
//! marker, a document version counter, the last published diagnostics — so the
//! worst a half-updated observation costs is a respawn, a redundant retry, or a
//! `wait_for_diagnostics` that times out and falls back to what is cached. None
//! of them can invent a result: `diagnostics_for` still answers only with what
//! a server actually published.
//!
//! This mirrors `codypendent_daemon::poison`, which carries the full reasoning;
//! this crate cannot depend on the daemon (the fabric is deliberately below it),
//! hence the second definition rather than a shared one.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock `mutex`, recovering the guard if a previous holder panicked.
pub fn lock_recovering<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
