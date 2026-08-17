/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Data sensitivity classification hierarchy.
 */
type DataClassification = "public" | "internal" | "confidential" | "secret" | "unknown";
/**
 * Publication class hierarchy. Defines how far data may be synchronized or shared.
 */
type PublicationClass =
  "private-local" | "metadata-shared" | "content-shared" | "organization-knowledge" | "public-marketplace" | "unknown";

export interface OrganizationCatalog {
  create_request: CreateOrganizationRequest;
  organization: Organization;
  summary: OrganizationSummary;
  update_request: UpdateOrganizationRequest;
}
/**
 * Request to create a new organization.
 */
export interface CreateOrganizationRequest {
  data_residency?: string | null;
  display_name: string;
  max_classification?: DataClassification | null;
  max_publication_class?: PublicationClass | null;
  retention_days?: number | null;
  slug: string;
}
/**
 * Core Organization entity in the control plane.
 */
export interface Organization {
  created_at: string;
  data_residency?: string | null;
  display_name: string;
  id: string;
  max_classification: DataClassification;
  max_publication_class: PublicationClass;
  policy_version: number;
  retention_days?: number | null;
  slug: string;
  updated_at: string;
}
/**
 * Compact summary of an organization for listings.
 */
export interface OrganizationSummary {
  created_at: string;
  display_name: string;
  id: string;
  max_publication_class: PublicationClass;
  member_count: number;
  repository_count: number;
  slug: string;
}
/**
 * Request to update an existing organization.
 */
export interface UpdateOrganizationRequest {
  data_residency?: string | null;
  display_name?: string | null;
  max_classification?: DataClassification | null;
  max_publication_class?: PublicationClass | null;
  retention_days?: number | null;
}
