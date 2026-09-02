//! Model readiness on the wire.
//!
//! "Can this model actually serve a run?" was, until now, a question every
//! client answered for itself by compiling `codypendent-runtime`'s
//! `ModelRegistry` and reading `models.toml`, `auth.json` and `providers.toml`
//! off the daemon's disk. The TUI does it, the CLI's `models check` does it,
//! and the desktop shell links the whole registry (and a provider feature) for
//! nothing else. A client that is not on the daemon's machine — the VS Code
//! extension over a forwarded socket, the web console — cannot do it at all,
//! so those two show no readiness.
//!
//! [`CommandBody::ProbeModel`](crate::CommandBody::ProbeModel) moves the
//! question to the side that owns the answer, and this module is the shape of
//! the reply.
//!
//! # Why the verdict carries a structured error
//!
//! A probe that fails is exactly the moment a client wants to offer the right
//! next step, and "unavailable" alone does not say whether that step is
//! *re-authenticate* or *pick another model*. So an
//! [`ModelReadiness::Unavailable`] may carry the same
//! [`CodypendentError`](crate::CodypendentError) a failed run does, with the
//! same `user_action` — one classification, read the same way in both places.

use serde::{Deserialize, Serialize};

use crate::error::CodypendentError;
use crate::ids::ModelId;

/// One configured model's readiness, as the daemon sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelProbe {
    /// The `models.toml` id, not the provider's model name.
    pub id: ModelId,
    pub readiness: ModelReadiness,
    /// Whether the network was actually used to reach this verdict.
    ///
    /// A client shows a credentials-only verdict differently from a probed
    /// one, because only the latter proves the provider lists the model.
    pub probed: bool,
}

/// What the daemon can say about a model without running it.
///
/// `#[serde(other)] Unknown` so a client built against an older protocol folds
/// a verdict it has never heard of into something it can still render, rather
/// than failing to parse the whole reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ModelReadiness {
    /// Credentials resolve and, when the probe reached the network, the
    /// provider lists this model.
    Ready { detail: String },
    /// Nothing is known to be wrong, but nothing was proved either — an ACP
    /// agent the daemon checks when a run starts, or a network probe that was
    /// not asked for.
    Unverified { detail: String },
    /// This model cannot serve a run as configured.
    Unavailable {
        detail: String,
        /// The classified cause, when the daemon has one: a stable code,
        /// retryability and the `user_action` a client turns into an
        /// affordance. Absent for a verdict with no typed cause behind it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<CodypendentError>,
    },
    /// A verdict this build does not know.
    #[serde(other)]
    Unknown,
}

impl ModelReadiness {
    /// The human sentence behind any verdict, for a client that only renders
    /// text. Empty for [`ModelReadiness::Unknown`], which carries none.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Ready { detail } | Self::Unverified { detail } => detail,
            Self::Unavailable { detail, .. } => detail,
            Self::Unknown => "",
        }
    }

    /// Whether this verdict means a run would be refused now.
    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }
}
