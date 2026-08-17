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

export interface RepositoryCatalog {
  register_request: RegisterRepositoryRequest;
  repository: Repository;
  summary: RepositorySummary;
  update_request: UpdateRepositoryRequest;
}
/**
 * Request to register a repository in an organization.
 */
export interface RegisterRepositoryRequest {
  display_name: string;
  federated_id: string;
  max_classification?: DataClassification | null;
  max_publication_class?: PublicationClass | null;
}
/**
 * Repository entity registered with an organization in the control plane.
 */
export interface Repository {
  created_at: string;
  display_name: string;
  /**
   * Cross-machine federated identity (SHA-256 hex).
   */
  federated_id: string;
  id: string;
  max_classification: DataClassification;
  max_publication_class: PublicationClass;
  organization_id: string;
  policy_version: number;
}
/**
 * Compact repository summary for listings.
 */
export interface RepositorySummary {
  created_at: string;
  display_name: string;
  federated_id: string;
  id: string;
  max_classification: DataClassification;
  max_publication_class: PublicationClass;
  organization_id: string;
  published_object_count: number;
  shared_session_count: number;
}
/**
 * Request to update repository settings.
 */
export interface UpdateRepositoryRequest {
  display_name?: string | null;
  max_classification?: DataClassification | null;
  max_publication_class?: PublicationClass | null;
}
