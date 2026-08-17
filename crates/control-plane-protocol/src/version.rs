//! Control-plane independent protocol versioning and negotiation.
//!
//! Control-plane protocol versioning is independent of the local protocol versioning
//! to allow different release cadences.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VersionParseError {
    #[error("invalid version format: expected <major>.<minor>, got '{0}'")]
    InvalidFormat(String),
    #[error("invalid number in version: {0}")]
    InvalidNumber(#[from] std::num::ParseIntError),
}

/// Control plane wire protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

/// Current control plane protocol version (v1.0).
pub const CONTROL_PLANE_PROTOCOL_V1: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

/// Minimum supported control plane protocol version for backward compatibility.
pub const CONTROL_PLANE_PROTOCOL_MIN_SUPPORTED: ProtocolVersion =
    ProtocolVersion { major: 1, minor: 0 };

impl ProtocolVersion {
    /// Create a new protocol version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Check whether this version is compatible with another version.
    /// Breaking changes bump `major`; additive changes bump `minor`.
    #[must_use]
    pub fn is_compatible_with(&self, other: &ProtocolVersion) -> bool {
        self.major == other.major
    }

    /// Negotiate a common protocol version between client and server.
    /// Returns the highest minor version supported by both within the same major version.
    #[must_use]
    pub fn negotiate(&self, client_version: &ProtocolVersion) -> Option<ProtocolVersion> {
        if self.major != client_version.major {
            return None;
        }
        Some(ProtocolVersion {
            major: self.major,
            minor: std::cmp::min(self.minor, client_version.minor),
        })
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl FromStr for ProtocolVersion {
    type Err = VersionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 2 {
            return Err(VersionParseError::InvalidFormat(s.to_string()));
        }
        let major = parts[0].parse::<u16>()?;
        let minor = parts[1].parse::<u16>()?;
        Ok(Self { major, minor })
    }
}

/// Initial protocol handshake request from a client or daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ProtocolHandshakeRequest {
    /// The client's supported protocol version.
    pub client_version: ProtocolVersion,
    /// Client identifier or kind (e.g. "daemon", "web-ui", "cli").
    pub client_kind: String,
    /// Client build identifier if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_build_id: Option<String>,
    /// Capabilities requested by the client.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Server response to a protocol handshake request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ProtocolHandshakeResponse {
    /// The negotiated protocol version to use for subsequent interactions.
    pub negotiated_version: ProtocolVersion,
    /// The server's full supported protocol version.
    pub server_version: ProtocolVersion,
    /// The oldest supported client version.
    pub min_compatible_version: ProtocolVersion,
    /// Active capabilities negotiated for this connection.
    #[serde(default)]
    pub supported_capabilities: Vec<String>,
}
