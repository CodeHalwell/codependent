//! Concrete secret backend implementations.
//!
//! # Which of these are real
//!
//! | Backend | Status |
//! |---|---|
//! | [`EnvironmentBackend`] | **Real.** Reads the daemon's own environment. |
//! | [`ManagedBackend`] | **Real when keyed.** `ChaCha20-Poly1305` envelope store, in process memory, no persistence. Refuses without a KEK. |
//! | [`WorkloadIdentityBackend`] | **Real when seeded**, but the token is an `HMAC-SHA256` bearer token bound to a shared seed — not an IdP-issued assertion. Refuses without a seed. |
//! | [`KeychainBackend`] | **Not implemented.** No Security-framework/libsecret/DPAPI client. Always refuses. |
//! | [`VaultBackend`] | **Not implemented.** No HTTP client. Always refuses. |
//!
//! Every "not implemented" case refuses with a typed dotted code. None of them
//! reports a miss for a store it never consulted, and none of them falls back
//! to another source.

pub mod environment;
pub mod keychain;
pub mod managed;
pub mod vault;
pub mod workload_identity;

pub use environment::EnvironmentBackend;
pub use keychain::KeychainBackend;
pub use managed::ManagedBackend;
pub use vault::VaultBackend;
pub use workload_identity::WorkloadIdentityBackend;
