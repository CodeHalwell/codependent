/** Mirrors `crates/protocol/src/question.rs`. */

export interface QuestionOption {
  label: string;
  /** `skip_serializing_if = "String::is_empty"` — absent, never `""`. */
  description?: string;
}

export interface QuestionPrompt {
  question: string;
  header: string;
  options: QuestionOption[];
  /** `#[serde(default)]` with no skip — always present. */
  multiple: boolean;
  /** `#[serde(default = "default_true")]` with no skip — always present. */
  custom: boolean;
}

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type QuestionOutcome =
  | { type: "Answered"; answers: string[][] }
  | { type: "Rejected"; feedback?: string }
  | { type: "Unknown" };
