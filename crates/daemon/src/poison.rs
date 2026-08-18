//! Poison-tolerant locking for the daemon's shared in-memory registries.
//!
//! **Why this exists.** `std::sync::Mutex` poisons permanently the moment a
//! thread panics while holding it. Every later `lock()` returns `Err`, so the
//! usual `.expect("… poisoned")` turns ONE panicking run into a subsystem that
//! is dead for the rest of the daemon's lifetime — approvals that can never be
//! answered, runs that can never be cancelled, workers that can never be shut
//! down. The daemon is a long-lived process; a localized fault must stay
//! localized.
//!
//! **Why recovery is the right containment HERE, and not everywhere.**
//! Recovering a poisoned mutex continues with state a panicking thread may have
//! left half-updated, so it is only sound when a partial update degrades
//! *toward* the safe answer. Every registry this helper guards satisfies that:
//!
//! - **Approval waiters** ([`crate::approvals`]) hold `watch` senders whose
//!   value only ever moves `None -> Some(decision)` under a caller-supplied
//!   decision. A panic between inserting an entry and sending its decision
//!   leaves `None`, which parks the run; a missing entry makes
//!   `await_decision` return `NotFound`. Both outcomes are "not approved" —
//!   there is no partial state that fabricates an approval. The durable row in
//!   `approvals` remains the authority either way.
//! - **Run control** ([`crate::executor`]'s registry) holds cancellation
//!   handles and the two pending sets. A lost entry means a cancel is not
//!   delivered — which is exactly what a poisoned lock guarantees for EVERY
//!   run, since `cancel_run`/`shutdown` would panic instead. Recovery strictly
//!   widens the set of runs that can still be stopped.
//! - **UI-worker admission** ([`crate::remote_ui_workers`]) recomputes its
//!   quotas from the live map on each admission, so an intact-but-arbitrary map
//!   is still enforced against. The map is never reset: dropping it would
//!   orphan running workers (their cancellation senders are the only way to
//!   stop them) *and* zero the quota, which is the fail-open direction.
//! - **Fan-out and memoization** ([`crate::subscriptions`], the code-graph
//!   scanned/watcher maps) are caches over a durable source of truth; a lost
//!   entry costs a re-subscribe or a redundant scan, nothing more.
//!
//! Where a guarded value's invariants ARE load bearing across a partial update,
//! do not reach for this: reset the value to a known-good state, or keep the
//! panic and say why. Two sites in this workspace deliberately keep `expect`
//! for that reason — see the notes in the review, not a blanket sweep.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock `mutex`, recovering the guard if a previous holder panicked.
///
/// See the module docs for why observing possibly-half-updated state is the
/// safe direction for the registries this is used on.
pub fn lock_recovering<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
