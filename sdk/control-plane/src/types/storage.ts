import type { UUID } from "./common.js";
import type { PublicationClass } from "./organization.js";

export type ObjectState = "uploading" | "available" | "tombstoned";
export type ObjectEncryption = "none" | "envelope";

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

export interface ObjectUploadRequest {
  repositoryId?: UUID | undefined;
  contentHash: string;
  byteLength: number;
  mediaType: string;
  class: PublicationClass;
}

export interface ObjectUploadReceipt {
  objectId: UUID;
  uploadUrl: string;
  headers?: Record<string, string> | undefined;
  expiresAt: string;
}

export interface PresignedUrlResponse {
  objectId: UUID;
  downloadUrl: string;
  expiresAt: string;
}
