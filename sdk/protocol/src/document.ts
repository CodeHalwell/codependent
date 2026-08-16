/** Mirrors `crates/protocol/src/document.rs`. */

import type { DocumentId, JsonValue, Timestamp } from "./ids.js";

export interface SuggestionInput {
  block_id: string;
  range_start: number;
  range_end: number;
  replacement: string;
  rationale?: string;
}

/**
 * `#[serde(tag = "op", rename_all = "snake_case")]` — note the discriminator is
 * `op`, NOT `type`, and the variant names are snake_case on the wire.
 * `#[serde(other)] Unknown` becomes `"unknown"` under the same rename.
 */
export type DocumentMutation =
  | { op: "insert"; index: number; block_id: string; content: JsonValue }
  | { op: "delete"; block_id: string }
  | {
      op: "edit_text";
      block_id: string;
      position: number;
      /** `#[serde(default)]` with no skip — always present. */
      delete_len: number;
      /** `#[serde(default)]` with no skip — always present. */
      insert: string;
    }
  | { op: "annotate"; suggestion: SuggestionInput }
  | { op: "accept_suggestion"; suggestion_id: string }
  | { op: "reject_suggestion"; suggestion_id: string }
  | { op: "unknown" };

/**
 * `#[serde(tag = "kind", rename_all = "snake_case")]` — the discriminator is
 * `kind`, not `type`.
 */
export type PublishTarget =
  | { kind: "repository_file"; path: string }
  | { kind: "docs_branch_commit"; branch: string; path: string }
  | { kind: "documentation_pr"; branch: string; path: string; title: string }
  | { kind: "unknown" };

export interface DocumentSync {
  document_id: DocumentId;
  revision: number;
  /** `Vec<u8>` serialized as a JSON array of byte values, not base64. */
  update: number[];
}

export interface DocumentEditLease {
  document_id: DocumentId;
  block_id?: string;
}

export interface DocumentLeaseGrant {
  lease_id: string;
  document_id: DocumentId;
  block_id?: string;
  expires_at: Timestamp;
}
