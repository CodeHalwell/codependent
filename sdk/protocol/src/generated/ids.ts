/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

export interface IdCatalog {
  agent_id: string;
  approval_id: string;
  artifact_id: string;
  branch_id: string;
  change_set_id: string;
  checkpoint_id: string;
  client_id: string;
  code_node_id: string;
  command_id: string;
  correlation_id: string;
  council_result_id: string;
  daemon_instance_id: string;
  document_id: string;
  learning_id: string;
  memory_id: string;
  message_id: string;
  model_id: string;
  organization_id: string;
  plugin_id: string;
  prompt_id: string;
  question_id: string;
  registry_item_id: string;
  repository_id: string;
  run_id: string;
  session_id: string;
  skill_id: string;
  task_id: string;
  timestamp: string;
  tool_id: string;
  user_id: string;
  workflow_id: string;
  workspace_id: string;
}

export type AgentId = IdCatalog["agent_id"];
export type ApprovalId = IdCatalog["approval_id"];
export type ArtifactId = IdCatalog["artifact_id"];
export type BranchId = IdCatalog["branch_id"];
export type ChangeSetId = IdCatalog["change_set_id"];
export type CheckpointId = IdCatalog["checkpoint_id"];
export type ClientId = IdCatalog["client_id"];
export type CodeNodeId = IdCatalog["code_node_id"];
export type CommandId = IdCatalog["command_id"];
export type CorrelationId = IdCatalog["correlation_id"];
export type CouncilResultId = IdCatalog["council_result_id"];
export type DaemonInstanceId = IdCatalog["daemon_instance_id"];
export type DocumentId = IdCatalog["document_id"];
export type LearningId = IdCatalog["learning_id"];
export type MemoryId = IdCatalog["memory_id"];
export type MessageId = IdCatalog["message_id"];
export type ModelId = IdCatalog["model_id"];
export type OrganizationId = IdCatalog["organization_id"];
export type PluginId = IdCatalog["plugin_id"];
export type PromptId = IdCatalog["prompt_id"];
export type QuestionId = IdCatalog["question_id"];
export type RegistryItemId = IdCatalog["registry_item_id"];
export type RepositoryId = IdCatalog["repository_id"];
export type RunId = IdCatalog["run_id"];
export type SessionId = IdCatalog["session_id"];
export type SkillId = IdCatalog["skill_id"];
export type TaskId = IdCatalog["task_id"];
export type Timestamp = IdCatalog["timestamp"];
export type ToolId = IdCatalog["tool_id"];
export type UserId = IdCatalog["user_id"];
export type WorkflowId = IdCatalog["workflow_id"];
export type WorkspaceId = IdCatalog["workspace_id"];

export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
