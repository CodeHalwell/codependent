/** Mirrors `crates/protocol/src/ide.rs`. */

export interface Position {
  line: number;
  character: number;
}

export interface Range {
  start: Position;
  end: Position;
}

export interface EditorSelection {
  path: string;
  range: Range;
}

export interface DirtyBufferDigest {
  path: string;
  sha256: string;
  byte_length: number;
}

export interface IdeContextUpdate {
  active_file?: string;
  selection?: EditorSelection;
  /** `skip_serializing_if = "Vec::is_empty"`. */
  open_files?: string[];
  /** `skip_serializing_if = "Vec::is_empty"`. */
  dirty_buffers?: DirtyBufferDigest[];
  /** `#[serde(default)]` with no skip — always present. */
  diagnostics_revision: number;
}

export interface Location {
  path: string;
  range?: Range;
}

export interface TextEdit {
  path: string;
  range: Range;
  new_text: string;
}

export interface WorkspaceEdit {
  /** `skip_serializing_if = "Vec::is_empty"`. */
  edits?: TextEdit[];
}

export interface DiffRequest {
  title: string;
  left_label: string;
  right_label: string;
  left: string;
  right: string;
}

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type IdeRequest =
  | { type: "ApplyEdit"; edit: WorkspaceEdit }
  | { type: "RevealLocation"; location: Location }
  | { type: "ShowDiff"; request: DiffRequest }
  | { type: "Unknown" };

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type DiagnosticSeverity =
  | { type: "Error" }
  | { type: "Warning" }
  | { type: "Information" }
  | { type: "Hint" }
  | { type: "Unknown" };

export interface Diagnostic {
  path: string;
  range: Range;
  severity: DiagnosticSeverity;
  message: string;
  source?: string;
}

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type SourceProvenance =
  | { type: "CommittedAt"; revision: string }
  | { type: "Filesystem" }
  | { type: "UnsavedIdeBuffer" }
  | { type: "GeneratedPatch" }
  | { type: "AgentWorktree" }
  | { type: "Unknown" };
