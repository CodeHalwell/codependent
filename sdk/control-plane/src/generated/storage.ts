/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Encryption mode for stored objects.
 */
export type ObjectEncryption = ("none" | "envelope") | "unknown";
/**
 * Publication class hierarchy. Defines how far data may be synchronized or shared.
 */
type PublicationClass =
  "private-local" | "metadata-shared" | "content-shared" | "organization-knowledge" | "public-marketplace" | "unknown";
/**
 * Lifecycle state of a published object in object storage.
 */
export type ObjectState = "uploading" | "available" | "tombstoned" | "unknown";

export interface ObjectStorageCatalog {
  complete_upload_request: CompleteUploadRequest;
  encryption: ObjectEncryption;
  object: PublishedObject;
  presigned_download_request: PresignedDownloadRequest;
  presigned_download_response: PresignedDownloadResponse;
  presigned_upload_request: PresignedUploadRequest;
  presigned_upload_response: PresignedUploadResponse;
  state: ObjectState;
}
/**
 * Confirmation that an upload has finished and is ready for verification.
 */
export interface CompleteUploadRequest {
  actual_byte_length: number;
  content_hash: string;
  object_id: string;
}
/**
 * Published object metadata record.
 */
export interface PublishedObject {
  byte_length: number;
  class: PublicationClass;
  /**
   * Content address (SHA-256 digest of object bytes).
   */
  content_hash: string;
  created_at: string;
  encryption: ObjectEncryption;
  id: string;
  media_type: string;
  organization_id: string;
  repository_id?: string | null;
  state: ObjectState;
  uploaded_by_daemon?: string | null;
}
/**
 * Request for a presigned download URL.
 */
export interface PresignedDownloadRequest {
  object_id: string;
}
/**
 * Response containing a presigned download URL.
 */
export interface PresignedDownloadResponse {
  byte_length: number;
  content_hash: string;
  download_url: string;
  expires_at: string;
  headers?: {
    [k: string]: string | undefined;
  };
  media_type: string;
}
/**
 * Request for a presigned upload URL.
 */
export interface PresignedUploadRequest {
  byte_length: number;
  class: PublicationClass;
  content_hash: string;
  encryption?: ObjectEncryption & string;
  media_type: string;
  repository_id?: string | null;
}
/**
 * Response containing a presigned direct upload URL and required headers.
 */
export interface PresignedUploadResponse {
  expires_at: string;
  headers?: {
    [k: string]: string | undefined;
  };
  object_id: string;
  upload_url: string;
}
