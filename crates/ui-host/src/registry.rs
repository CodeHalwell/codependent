//! Governed component contribution slots.

use std::collections::{HashMap, HashSet};

use codypendent_protocol::{
    UiCapabilitySelection, UiContributionId, UiContributionRegistration, UiHardLimits,
    UiSlotDefinition, UiSlotId, UiValidationError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationTrust {
    /// Shipped, signed Codypendent presentation. May use core-only slots.
    Core,
    /// Installed extension presentation. Restricted to public slots.
    Extension,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UiRegistryError {
    #[error("remote UI slot id is empty")]
    EmptySlot,
    #[error("remote UI slot `{0}` is already defined")]
    DuplicateSlot(String),
    #[error("remote UI contribution id is empty")]
    EmptyContribution,
    #[error("remote UI contribution `{0}` is already registered")]
    DuplicateContribution(String),
    #[error("remote UI registry validation failed ({code} at {path}): {message}")]
    Validation {
        code: String,
        path: String,
        message: String,
    },
    #[error("remote UI contribution targets unknown slot `{0}`")]
    UnknownSlot(String),
    #[error("slot `{slot}` accepts point `{expected}`, not `{actual}`")]
    WrongPoint {
        slot: String,
        expected: String,
        actual: String,
    },
    #[error("slot `{0}` is reserved for trusted core presentation")]
    TrustedOnly(String),
    #[error("slot `{slot}` is full (maximum {maximum})")]
    SlotFull { slot: String, maximum: u32 },
    #[error("remote UI contribution registry is full (maximum {maximum})")]
    RegistryFull { maximum: u32 },
    #[error("client did not negotiate contribution point `{0}`")]
    PointUnavailable(String),
    #[error("client did not negotiate required UI capability `{0}`")]
    CapabilityUnavailable(String),
    #[error("contribution `{contribution}` has no document id")]
    EmptyDocument { contribution: String },
    #[error("contribution `{contribution}` has no extension id")]
    EmptyExtension { contribution: String },
}

/// The registry does not execute `when` expressions. It retains their signed,
/// validated text for a higher-level projection evaluator, and returns a stable
/// priority order for each host-owned slot.
#[derive(Debug, Clone, Default)]
pub struct ContributionRegistry {
    limits: UiHardLimits,
    slots: HashMap<UiSlotId, UiSlotDefinition>,
    registrations: HashMap<UiContributionId, (RegistrationTrust, UiContributionRegistration)>,
}

impl ContributionRegistry {
    #[must_use]
    pub fn new(limits: UiHardLimits) -> Self {
        Self {
            limits,
            slots: HashMap::new(),
            registrations: HashMap::new(),
        }
    }

    #[must_use]
    pub fn limits(&self) -> UiHardLimits {
        self.limits
    }

    pub fn define_slot(&mut self, definition: UiSlotDefinition) -> Result<(), UiRegistryError> {
        if definition.id.is_empty() {
            return Err(UiRegistryError::EmptySlot);
        }
        if self.slots.contains_key(&definition.id) {
            return Err(UiRegistryError::DuplicateSlot(definition.id.to_string()));
        }
        definition
            .validate(&self.limits)
            .map_err(validation_error)?;
        self.slots.insert(definition.id.clone(), definition);
        Ok(())
    }

    pub fn remove_slot(&mut self, id: &UiSlotId) -> Option<UiSlotDefinition> {
        let removed = self.slots.remove(id);
        if removed.is_some() {
            self.registrations
                .retain(|_, (_, registration)| &registration.slot != id);
        }
        removed
    }

    #[must_use]
    pub fn slot(&self, id: &UiSlotId) -> Option<&UiSlotDefinition> {
        self.slots.get(id)
    }

    pub fn register(
        &mut self,
        trust: RegistrationTrust,
        registration: UiContributionRegistration,
        negotiated: &UiCapabilitySelection,
    ) -> Result<(), UiRegistryError> {
        negotiated.validate().map_err(validation_error)?;
        let limits = self.limits.intersection(negotiated.limits);
        if registration.id.is_empty() {
            return Err(UiRegistryError::EmptyContribution);
        }
        if self.registrations.contains_key(&registration.id) {
            return Err(UiRegistryError::DuplicateContribution(
                registration.id.to_string(),
            ));
        }
        if registration.extension_id.is_empty() {
            return Err(UiRegistryError::EmptyExtension {
                contribution: registration.id.to_string(),
            });
        }
        if registration.document_id.is_empty() {
            return Err(UiRegistryError::EmptyDocument {
                contribution: registration.id.to_string(),
            });
        }
        registration.validate(&limits).map_err(validation_error)?;
        let slot = self
            .slots
            .get(&registration.slot)
            .ok_or_else(|| UiRegistryError::UnknownSlot(registration.slot.to_string()))?;
        if slot.point != registration.point {
            return Err(UiRegistryError::WrongPoint {
                slot: slot.id.to_string(),
                expected: slot.point.to_string(),
                actual: registration.point.to_string(),
            });
        }
        if slot.trusted_only && trust != RegistrationTrust::Core {
            return Err(UiRegistryError::TrustedOnly(slot.id.to_string()));
        }
        if !negotiated.contribution_points.contains(&registration.point) {
            return Err(UiRegistryError::PointUnavailable(
                registration.point.to_string(),
            ));
        }
        if self.registrations.len() >= limits.max_contributions as usize {
            return Err(UiRegistryError::RegistryFull {
                maximum: limits.max_contributions,
            });
        }
        for required in &registration.requires {
            if !negotiated.capabilities.contains(required) {
                return Err(UiRegistryError::CapabilityUnavailable(required.to_string()));
            }
        }
        if let Some(maximum) = slot.maximum_contributions {
            let count = self
                .registrations
                .values()
                .filter(|(_, current)| current.slot == registration.slot)
                .count();
            if count >= maximum as usize {
                return Err(UiRegistryError::SlotFull {
                    slot: slot.id.to_string(),
                    maximum,
                });
            }
        }
        self.registrations
            .insert(registration.id.clone(), (trust, registration));
        Ok(())
    }

    pub fn unregister(&mut self, id: &UiContributionId) -> Option<UiContributionRegistration> {
        self.registrations.remove(id).map(|(_, value)| value)
    }

    pub fn unregister_extension(&mut self, extension_id: &str) -> Vec<UiContributionRegistration> {
        let ids: Vec<_> = self
            .registrations
            .iter()
            .filter(|(_, (_, registration))| registration.extension_id.as_str() == extension_id)
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .filter_map(|id| self.unregister(&id))
            .collect()
    }

    #[must_use]
    pub fn for_slot(&self, slot: &UiSlotId) -> Vec<&UiContributionRegistration> {
        let mut registrations: Vec<_> = self
            .registrations
            .values()
            .filter(|(_, registration)| &registration.slot == slot)
            .map(|(_, registration)| registration)
            .collect();
        registrations.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.id.cmp(&right.id))
        });
        registrations
    }

    #[must_use]
    pub fn mounted_document_ids(&self) -> HashSet<&str> {
        self.registrations
            .values()
            .map(|(_, registration)| registration.document_id.as_str())
            .collect()
    }

    /// Host-attested extension identity for immutable surface chrome. The
    /// value comes from a broker-validated registration, never document props.
    #[must_use]
    pub fn extension_for_document(&self, document_id: &str) -> Option<&str> {
        self.registrations
            .values()
            .find(|(_, registration)| registration.document_id.as_str() == document_id)
            .map(|(_, registration)| registration.extension_id.as_str())
    }

    /// Broker-attested registration for immutable extension identity chrome.
    #[must_use]
    pub fn registration_for_document(
        &self,
        document_id: &str,
    ) -> Option<&UiContributionRegistration> {
        self.registrations
            .values()
            .map(|(_, registration)| registration)
            .find(|registration| registration.document_id.as_str() == document_id)
    }
}

fn validation_error(error: UiValidationError) -> UiRegistryError {
    UiRegistryError::Validation {
        code: error.code,
        path: error.path,
        message: error.message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::{
        UiCapability, UiContributionPoint, UiDocumentId, UiExtensionId, UiFallback, UiHardLimits,
        UiProtocolVersion, UiViewport,
    };
    use std::collections::BTreeMap;

    fn point() -> UiContributionPoint {
        UiContributionPoint::from("artifact-renderer")
    }

    fn selection() -> UiCapabilitySelection {
        UiCapabilitySelection {
            protocol_version: UiProtocolVersion::V1,
            primitives: Vec::new(),
            capabilities: vec![UiCapability::from("artifact-read")],
            contribution_points: vec![point()],
            image_protocols: Vec::new(),
            color_depth: 24,
            unicode: true,
            mouse: true,
            screen_reader: false,
            viewport: Some(UiViewport {
                width: 100,
                height: 30,
                pixel_width: None,
                pixel_height: None,
                density: None,
            }),
            limits: UiHardLimits::default(),
        }
    }

    fn slot(trusted_only: bool, maximum: u32) -> UiSlotDefinition {
        UiSlotDefinition {
            id: UiSlotId::from("artifact.detail"),
            point: point(),
            accepts: Vec::new(),
            trusted_only,
            maximum_contributions: Some(maximum),
            fallback: Some(UiFallback {
                plain_text: Some("artifact".into()),
                replacement: None,
                behavior: None,
            }),
        }
    }

    fn registration(id: &str, priority: i32) -> UiContributionRegistration {
        UiContributionRegistration {
            id: UiContributionId::from(id),
            extension_id: UiExtensionId::from("acme"),
            point: point(),
            slot: UiSlotId::from("artifact.detail"),
            document_id: UiDocumentId::from(format!("doc.{id}")),
            priority,
            when: None,
            requires: vec![UiCapability::from("artifact-read")],
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn extensions_cannot_mount_trusted_slots() {
        let mut registry = ContributionRegistry::default();
        registry.define_slot(slot(true, 1)).unwrap();
        assert!(matches!(
            registry.register(
                RegistrationTrust::Extension,
                registration("one", 0),
                &selection()
            ),
            Err(UiRegistryError::TrustedOnly(_))
        ));
    }

    #[test]
    fn registrations_are_bounded_and_priority_ordered() {
        let mut registry = ContributionRegistry::default();
        registry.define_slot(slot(false, 2)).unwrap();
        registry
            .register(
                RegistrationTrust::Extension,
                registration("low", 1),
                &selection(),
            )
            .unwrap();
        registry
            .register(
                RegistrationTrust::Extension,
                registration("high", 5),
                &selection(),
            )
            .unwrap();
        assert!(matches!(
            registry.register(
                RegistrationTrust::Extension,
                registration("overflow", 10),
                &selection()
            ),
            Err(UiRegistryError::SlotFull { .. })
        ));
        let ordered = registry.for_slot(&UiSlotId::from("artifact.detail"));
        assert_eq!(ordered[0].id.as_str(), "high");
        assert_eq!(ordered[1].id.as_str(), "low");
    }

    #[test]
    fn negotiated_global_contribution_limit_is_enforced() {
        let mut registry = ContributionRegistry::default();
        registry.define_slot(slot(false, 10)).unwrap();
        let mut negotiated = selection();
        negotiated.limits.max_contributions = 1;
        registry
            .register(
                RegistrationTrust::Extension,
                registration("one", 0),
                &negotiated,
            )
            .unwrap();
        assert!(matches!(
            registry.register(
                RegistrationTrust::Extension,
                registration("two", 0),
                &negotiated,
            ),
            Err(UiRegistryError::RegistryFull { maximum: 1 })
        ));
    }
}
