import type { UUID } from "./common.js";
import type { PublicationClass } from "./organization.js";

export type ObjectState = "uploading" | "available" | "tombstoned" | "unknown";
export type ObjectEncryption = "none" | "envelope" | "unknown";

export interface PublishedObject {
  id: UUID;
  organizationId: UUID;
  repositoryId: UUID | null;
  contentHash: string;
  byteLength: number;
  mediaType: string;
  class: PublicationClass;
  encryption: ObjectEncryption;
  state: ObjectState;
  uploadedByDaemon: UUID | null;
  createdAt: string;
}

/**
 * A verified upload accepted by the current Axum `/objects/upload` route.
 * The route hashes the bytes itself; it does not issue a direct-upload URL.
 */
export interface ObjectUploadRequest {
  body: BodyInit;
  mediaType?: string | undefined;
  expectedContentHash?: string | undefined;
}

export type ObjectUploadReceipt = PublishedObject;

/** Exact response from the current Axum `/objects/presign` route. */
export interface PresignedUrlResponse {
  url: string;
  key: string;
  method: "GET";
}
