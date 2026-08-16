/**
 * Identifier aliases mirroring `crates/protocol/src/ids.rs`.
 *
 * Every id newtype there is `#[serde(transparent)]`, so on the wire it is a
 * bare JSON string — a UUID (v7) for the `uuid_id!` newtypes, and a free-form
 * provider/OS string for {@link ModelId} and {@link UserId}. They are plain
 * `string` aliases rather than branded types so a value decoded straight from
 * `JSON.parse` is directly assignable.
 */

/** UUIDv7 string. */
export type SessionId = string;
/** UUIDv7 string. */
export type RunId = string;
/** UUIDv7 string. */
export type TaskId = string;
/** UUIDv7 string. */
export type AgentId = string;
/** UUIDv7 string. */
export type ArtifactId = string;
/** UUIDv7 string. */
export type ChangeSetId = string;
/** UUIDv7 string. */
export type WorkflowId = string;
/** UUIDv7 string. */
export type CouncilResultId = string;
/** UUIDv7 string. */
export type ToolId = string;
/** UUIDv7 string. */
export type SkillId = string;
/** UUIDv7 string. */
export type PluginId = string;
/** UUIDv7 string. */
export type DocumentId = string;
/** UUIDv7 string. */
export type WorkspaceId = string;
/** UUIDv7 string. */
export type ClientId = string;
/** UUIDv7 string. */
export type MessageId = string;
/** UUIDv7 string. */
export type CommandId = string;
/** UUIDv7 string. */
export type CorrelationId = string;
/** UUIDv7 string. */
export type ApprovalId = string;
/** UUIDv7 string. */
export type QuestionId = string;
/** UUIDv7 string. */
export type CheckpointId = string;
/** UUIDv7 string. */
export type PromptId = string;
/** UUIDv7 string. */
export type DaemonInstanceId = string;
/** UUIDv7 string. */
export type RegistryItemId = string;
/** UUIDv7 string. */
export type MemoryId = string;
/** UUIDv7 string. */
export type LearningId = string;
/** UUIDv7 string. */
export type CodeNodeId = string;
/** UUIDv7 string. */
export type RepositoryId = string;
/** UUIDv7 string. */
export type BranchId = string;
/** UUIDv7 string. */
export type OrganizationId = string;

/**
 * A provider model string such as `"claude-sonnet-5"` or `"qwen2.5-coder:32b"`
 * — not a UUID (`ids.rs`: `ModelId(pub String)`).
 */
export type ModelId = string;

/**
 * An OS user or configured identity string — not a UUID
 * (`ids.rs`: `UserId(pub String)`).
 */
export type UserId = string;

/**
 * An RFC 3339 / ISO 8601 timestamp string, as produced by serde for
 * `chrono::DateTime<Utc>` (e.g. `"2026-01-01T00:00:00Z"`).
 */
export type Timestamp = string;

/**
 * Any `serde_json::Value`. Wire fields typed as `Value` on the Rust side carry
 * arbitrary JSON and are deliberately not narrowed here.
 */
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
