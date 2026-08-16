/** Mirrors `crates/protocol/src/blackboard.rs`. */

import type { JsonValue } from "./ids.js";

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type BlackboardScope =
  | { type: "WorkflowRun"; workflow_run_id: string }
  | { type: "RepositoryBoard"; repository: string }
  | { type: "Unknown" };

export interface BlackboardItemDraft {
  kind: string;
  payload: JsonValue;
  confidence?: number;
  /** `skip_serializing_if = "Vec::is_empty"`. */
  evidence?: JsonValue[];
  status?: string;
  assignee?: string;
  ordinal?: number;
}

export interface BlackboardItemView {
  id: string;
  workflow_run_id: string;
  kind: string;
  payload: JsonValue;
  author: JsonValue;
  confidence?: number;
  /** `skip_serializing_if = "Vec::is_empty"`. */
  evidence?: JsonValue[];
  revision: number;
  superseded_by?: string;
  board_scope?: string;
  status?: string;
  assignee?: string;
  ordinal?: number;
}
