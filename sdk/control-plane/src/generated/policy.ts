/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Data sensitivity classification hierarchy.
 */
export type DataClassification = "public" | "internal" | "confidential" | "secret" | "unknown";
/**
 * Publication class hierarchy. Defines how far data may be synchronized or shared.
 */
export type PublicationClass =
  "private-local" | "metadata-shared" | "content-shared" | "organization-knowledge" | "public-marketplace" | "unknown";

export interface PolicyCatalog {
  data_classification: DataClassification;
  publication_class: PublicationClass;
  restrictions: PolicyRestrictions;
  snapshot: PolicySnapshot;
}
/**
 * Provider, model, and regional restrictions configured by the organization.
 */
export interface PolicyRestrictions {
  /**
   * Optional allow-list of model IDs.
   */
  allowed_models?: string[] | null;
  /**
   * Optional allow-list of LLM provider names. If None, all non-denied providers are allowed.
   */
  allowed_providers?: string[] | null;
  /**
   * Optional allow-list of geographic regions for cloud processing.
   */
  allowed_regions?: string[] | null;
  /**
   * Denied third-party integrations or tool names.
   */
  denied_integrations?: string[];
  /**
   * Explicit deny-list of model IDs.
   */
  denied_models?: string[];
  /**
   * Explicit deny-list of LLM provider names.
   */
  denied_providers?: string[];
  /**
   * Explicit deny-list of geographic regions.
   */
  denied_regions?: string[];
}
/**
 * Narrowed policy snapshot delivered from control plane to daemon. The local daemon uses this strictly as narrowing inputs (`local.strictest(remote)`).
 */
export interface PolicySnapshot {
  max_classification: DataClassification;
  max_publication_class: PublicationClass;
  payload_hash: string;
  policy_version: number;
  received_at: string;
  restrictions: PolicyRestrictions;
}
