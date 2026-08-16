/** Mirrors `crates/protocol/src/input.rs`. */

import type { ArtifactRef, DataClassification } from "./artifact.js";
import type { EditorSelection } from "./ide.js";
import type { ArtifactId, ModelId } from "./ids.js";

/** `#[serde(tag = "type", rename_all = "kebab-case")]` — lowercase tags. */
export type InputSource =
  | { type: "tui" }
  | { type: "ide" }
  | { type: "cli" }
  | { type: "web" }
  | { type: "voice" }
  | { type: "unknown" };

/** `#[serde(tag = "type", rename_all = "kebab-case")]`. */
export type TranscriptionMode = { type: "local" } | { type: "remote" } | { type: "unknown" };

/** `#[serde(tag = "type", rename_all = "kebab-case")]`. */
export type ScopeLevel =
  | { type: "system" }
  | { type: "organization" }
  | { type: "user" }
  | { type: "workspace" }
  | { type: "repository" }
  | { type: "branch" }
  | { type: "session" }
  | { type: "task" }
  | { type: "unknown" };

/** `#[serde(tag = "type", rename_all = "kebab-case")]`. */
export type GitHubRefKind =
  | { type: "pull-request" }
  | { type: "issue" }
  | { type: "commit" }
  | { type: "comment" }
  | { type: "unknown" };

export interface Transcript {
  text: string;
  mode: TranscriptionMode;
  model?: ModelId;
  /** `#[serde(default)]` with no skip — always present. */
  reviewed: boolean;
  source_audio: ArtifactId;
}

export interface AudioArtifact {
  original: ArtifactRef;
  transcript?: Transcript;
  duration_ms?: number;
  sample_rate_hz?: number;
}

export interface ModelObservation {
  text: string;
  model?: ModelId;
}

export interface ImageRegion {
  label?: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface ImageArtifact {
  original: ArtifactRef;
  extracted_text?: ArtifactRef;
  /** `skip_serializing_if = "Vec::is_empty"`. */
  observations?: ModelObservation[];
  /** `skip_serializing_if = "Vec::is_empty"`. */
  regions?: ImageRegion[];
  width?: number;
  height?: number;
}

export interface SymbolRef {
  path: string;
  symbol: string;
  kind?: string;
  line?: number;
}

export interface GitHubReference {
  owner: string;
  repo: string;
  kind: GitHubRefKind;
  number?: number;
  url?: string;
}

/**
 * `#[serde(tag = "block", rename_all = "kebab-case")]` — the discriminator is
 * `block`, not `type`. Every non-`Text` variant is a newtype variant, so its
 * struct fields are flattened alongside the tag.
 */
export type InputBlock =
  | { block: "text"; text: string }
  | ({ block: "audio" } & AudioArtifact)
  | ({ block: "image" } & ImageArtifact)
  | ({ block: "file" } & ArtifactRef)
  | ({ block: "editor-selection" } & EditorSelection)
  | ({ block: "code-symbol" } & SymbolRef)
  | ({ block: "github-reference" } & GitHubReference)
  | { block: "unknown" };

export interface InputEnvelope {
  source: InputSource;
  blocks: InputBlock[];
  scope: ScopeLevel;
  /** `skip_serializing_if = "Vec::is_empty"`. */
  attachments?: ArtifactRef[];
}

export interface OffDevicePolicy {
  max_off_device: DataClassification;
}
