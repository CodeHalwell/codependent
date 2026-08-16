/** Mirrors `crates/protocol/src/artifact.rs`. */

import type { ArtifactId } from "./ids.js";

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type DataClassification =
  | { type: "Public" }
  | { type: "Internal" }
  | { type: "Confidential" }
  | { type: "Secret" }
  | { type: "Unknown" };

export interface ArtifactRef {
  id: ArtifactId;
  media_type: string;
  byte_length: number;
  sha256: string;
  sensitivity: DataClassification;
}
