/** Mirrors `crates/protocol/src/memory.rs`. */

import type { DataClassification } from "./artifact.js";
import type { SessionEvent } from "./events.js";
import type { JsonValue, MemoryId, Timestamp } from "./ids.js";

/**
 * Note `tier` is a plain lowercase string here (`"system"`, `"user"`,
 * `"repository"`), NOT the internally-tagged {@link MemoryScopeTier} enum.
 */
export interface MemoryScope {
  tier: string;
  key?: string;
}

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type MemoryScopeTier =
  | { type: "System" }
  | { type: "User" }
  | { type: "Repository" }
  | { type: "Unknown" };

export interface MemoryView {
  id: MemoryId;
  scope: MemoryScope;
  class: string;
  statement: string;
  structured_value?: JsonValue;
  confidence: number;
  observed_at: Timestamp;
  sensitivity: DataClassification;
  /** `skip_serializing_if = "Vec::is_empty"`. */
  supersedes?: MemoryId[];
  /** `skip_serializing_if = "Vec::is_empty"`. */
  evidence?: string[];
}

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type MemoryEvidence =
  | { type: "Events"; events: SessionEvent[] }
  | { type: "Artifact"; media_type: string; bytes_base64: string }
  | { type: "Unknown" };
