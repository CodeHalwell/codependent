//! Protocol version negotiation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

/// The current protocol version. Additive changes bump `minor`; breaking
/// changes bump `major` and require negotiation.
///
/// Phase 1 adds handshake, command, catch-up, artifact-reference, and run/tool/
/// approval event payloads — all additive over Phase 0, so `major` stays `1`
/// and `minor` advances to `1`.
///
/// The daemon-auto-restart-on-version-mismatch feature adds
/// `ServerHello.build_id` and `DaemonStatus.build_id`/`active_run_count`, all
/// `#[serde(default)]` — additive again, so `major` stays `1` and `minor`
/// advances to `2`.
///
/// The daemon-side idle-guarded shutdown (`Payload::ShutdownIfIdle` /
/// `ShutdownRefused`) that closes the auto-restart TOCTOU window adds two new
/// payload variants — additive again (old peers never emit them, and a client
/// only sends `ShutdownIfIdle` to a daemon whose negotiated minor is ≥ 3), so
/// `major` stays `1` and `minor` advances to `3`.
///
/// External ACP agents add the `ProposedAction::AcpToolCall` approval payload.
/// It is additive, so `major` remains `1` and `minor` advances to `4`.
///
/// Adoption 11 adds `CommandBody::ListSessions`, `Payload::SessionList`,
/// `CommandBody::SearchWorkspaceFiles`, and `Payload::FileSearchResults`.
/// All are additive, so `major` stays `1` and `minor` advances to `5`.
pub const PROTOCOL_V1: ProtocolVersion = ProtocolVersion { major: 1, minor: 5 };

impl ProtocolVersion {
    /// Two versions are compatible when their major versions match.
    pub fn compatible_with(&self, other: &ProtocolVersion) -> bool {
        self.major == other.major
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}
