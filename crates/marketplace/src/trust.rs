//! Publisher trust, trust store synchronization, and revocation management (Milestone 5).
//!
//! Important security invariant: Publisher trust is DISTINCT from registry trust.
//! Trusting a registry that lists or serves a package asserts nothing about whether
//! the publisher who signed it is trusted by the local operator.

use codypendent_sandbox::TrustedPublishers;

use crate::error::MarketplaceError;
use crate::store::{MarketplacePublisher, MarketplaceStore, PublisherTrustTier};

/// Service managing publisher trust and revocation.
#[derive(Debug, Clone)]
pub struct TrustManager {
    store: MarketplaceStore,
}

impl TrustManager {
    #[must_use]
    pub fn new(store: MarketplaceStore) -> Self {
        Self { store }
    }

    /// Load all trusted/first-party publisher keys from the database into a [`TrustedPublishers`] store.
    pub async fn load_trusted_publishers(&self) -> Result<TrustedPublishers, MarketplaceError> {
        let mut trust_store = TrustedPublishers::default();
        let publishers = self.store.list_publishers().await?;

        for pub_record in publishers {
            // Only add if trusted/first_party and NOT revoked
            if pub_record.trust_tier.is_trusted() && pub_record.revoked_at.is_none() {
                // Convert hex to base64 for TrustedPublishers::add
                let raw_bytes = hex::decode(&pub_record.public_key_hex).map_err(|e| {
                    MarketplaceError::InvalidState(format!(
                        "invalid public key hex for publisher {}: {e}",
                        pub_record.id
                    ))
                })?;
                let b64_key =
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &raw_bytes);
                trust_store
                    .add(&pub_record.id, &b64_key)
                    .map_err(|e| MarketplaceError::InvalidState(e.to_string()))?;
            }
        }

        Ok(trust_store)
    }

    /// Register a publisher with a given public key hex (or base64) and trust tier.
    pub async fn register_publisher(
        &self,
        id: &str,
        display_name: &str,
        public_key_hex: &str,
        trust_tier: PublisherTrustTier,
        trusted_by: Option<&str>,
    ) -> Result<(), MarketplaceError> {
        if public_key_hex.len() != 64 || hex::decode(public_key_hex).is_err() {
            return Err(MarketplaceError::InvalidState(
                "public_key_hex must be exactly 64 hexadecimal characters".into(),
            ));
        }

        let now = chrono::Utc::now().to_rfc3339();
        let (trusted_at, trusted_by_val) = if trust_tier.is_trusted() {
            (Some(now.clone()), trusted_by.map(String::from))
        } else {
            (None, None)
        };

        let publisher = MarketplacePublisher {
            id: id.to_string(),
            display_name: display_name.to_string(),
            public_key_hex: public_key_hex.to_string(),
            trust_tier,
            trusted_at,
            trusted_by: trusted_by_val,
            revoked_at: None,
            revoked_reason: None,
            created_at: now,
        };

        self.store.upsert_publisher(&publisher).await?;
        Ok(())
    }

    /// Trust an existing publisher.
    pub async fn trust_publisher(
        &self,
        id: &str,
        trusted_by: Option<&str>,
    ) -> Result<(), MarketplaceError> {
        self.store
            .set_publisher_trust(id, PublisherTrustTier::Trusted, trusted_by)
            .await
    }

    /// Untrust a publisher (downgrade to untrusted).
    pub async fn untrust_publisher(&self, id: &str) -> Result<(), MarketplaceError> {
        self.store
            .set_publisher_trust(id, PublisherTrustTier::Untrusted, None)
            .await
    }

    /// Revoke a publisher.
    ///
    /// Retroactively disables installed packages from this publisher,
    /// invalidates pending permission receipts, and records the revocation event.
    pub async fn revoke_publisher(
        &self,
        id: &str,
        reason: &str,
        source: &str,
    ) -> Result<(), MarketplaceError> {
        self.store.revoke_publisher(id, reason, source).await
    }

    /// Check if a publisher is trusted and active (not revoked).
    pub async fn is_publisher_trusted(&self, id: &str) -> Result<bool, MarketplaceError> {
        let publisher = self.store.get_publisher(id).await?;
        Ok(publisher.is_some_and(|p| p.trust_tier.is_trusted() && p.revoked_at.is_none()))
    }
}
