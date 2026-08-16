/**
 * Golden-vector conformance suite for `@codypendent/protocol`.
 *
 * These TypeScript types are hand-maintained, not generated. What makes them
 * trustworthy is this file: it reads the SAME committed golden vectors the Rust
 * side emits and verifies against (`crates/protocol/tests/golden_vectors.rs`
 * writing `<repo-root>/protocol-vectors/`, see that directory's README) — no
 * copy, no second source of truth — and proves each declared type can represent
 * every field of the real serialized shape.
 *
 * The mechanism (borrowed from `extensions/vscode/test/protocol-vectors.test.ts`):
 * each vector is `unknown` JSON at runtime, so it is run through a
 * `reconstructX` function that copies named fields one by one into an object
 * literal ANNOTATED with the exact type from `src/`. Two independent things can
 * then go wrong, and each is a real drift signal:
 *
 *   1. `npm run typecheck` fails — the literal names a field the TS type does
 *      not declare (or omits a required one).
 *   2. The reconstructed value, JSON-normalized, does not deep-equal the
 *      original vector — a field present on the wire was never copied across,
 *      because the type had nowhere to put it.
 *
 * Coverage is total: every vector in every committed file is reconstructed.
 * `EXCLUDED_VECTORS` and `EXCLUDED_FILES` exist so a future vector CAN be
 * declared out of scope, but it must be listed there explicitly — silence is
 * a failure, both per file and across the directory listing.
 */
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { foldUnknownCommandBody, foldUnknownEventBody, foldUnknownPayload } from "../src/tags.js";
import type {
  Actor,
  ArtifactRef,
  AudioArtifact,
  BlackboardItemDraft,
  BlackboardItemView,
  BlackboardScope,
  Catchup,
  ClientCapabilities,
  ClientHello,
  ClientRole,
  CodeGraphEdgeAssertion,
  CodeGraphEdgeView,
  CodeGraphGrammar,
  CodeGraphLanguageCount,
  CodeGraphNodeView,
  CodeGraphPage,
  CodeGraphQuery,
  CodeGraphScanReport,
  CodeGraphSkippedExtension,
  CodeGraphStatusView,
  CodeGraphTally,
  Command,
  CommandBody,
  CodypendentError,
  DaemonStatus,
  DataClassification,
  Diagnostic,
  DiagnosticSeverity,
  DiffRequest,
  DirtyBufferDigest,
  DocumentEditLease,
  DocumentLeaseGrant,
  DocumentMutation,
  DocumentSync,
  EditorSelection,
  EventBody,
  FileMatchWire,
  GitHubRefKind,
  GitHubReference,
  IdeContextUpdate,
  IdeRequest,
  ImageArtifact,
  ImageRegion,
  InputBlock,
  InputEnvelope,
  InputSource,
  JsonValue,
  Location,
  MemoryEvidence,
  MemoryScope,
  MemoryScopeTier,
  MemoryView,
  ModelObservation,
  Payload,
  PendingApprovalProjection,
  PendingPromptView,
  Position,
  ProposedAction,
  ProtocolError,
  ProtocolVersion,
  PublishTarget,
  QuestionOption,
  QuestionOutcome,
  QuestionPrompt,
  Range,
  Risk,
  RiskLevel,
  RunDisposition,
  RunState,
  ScopeLevel,
  ServerHello,
  SessionEvent,
  SessionProjection,
  SessionSummary,
  SourceProvenance,
  Subscription,
  SuggestionInput,
  SymbolRef,
  TextEdit,
  ToolOutcome,
  Transcript,
  TranscriptionMode,
  UserAction,
  WorkflowEvent,
  WorkflowNodeState,
  WorkflowNodeView,
  WorkflowRunPhase,
  WorkflowRunSnapshot,
  WorkspaceEdit,
  AgentMode,
  ApprovalDecision,
  ApprovalScope,
  BudgetDimension,
  CanaryMetrics,
  CheckpointKind,
  OffDevicePolicy,
  PromotionAction,
  PromptDelivery,
} from "../src/index.js";

// ---------------------------------------------------------------------------
// Vector loading — the same committed files the Rust emitter writes.
// ---------------------------------------------------------------------------

const VECTORS_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "protocol-vectors");

function loadVectors(filename: string): Record<string, unknown> {
  return JSON.parse(readFileSync(join(VECTORS_DIR, filename), "utf8")) as Record<string, unknown>;
}

/** `"CommandBody_StartWorkflow_inline_manifest"` -> `"CommandBody"`. */
function vectorFamily(name: string): string {
  const underscore = name.indexOf("_");
  return underscore === -1 ? name : name.slice(0, underscore);
}

function expectReconstructionMatches(vectorName: string, original: unknown, reconstructed: unknown): void {
  const originalNormalized: unknown = JSON.parse(JSON.stringify(original));
  const reconstructedNormalized: unknown = JSON.parse(JSON.stringify(reconstructed));
  expect(reconstructedNormalized, vectorName).toEqual(originalNormalized);
}

// ---------------------------------------------------------------------------
// Small readers over `unknown` JSON (no `any`).
// ---------------------------------------------------------------------------

function asRecord(value: unknown, context: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${context}: expected an object, got ${JSON.stringify(value)}`);
  }
  return value as Record<string, unknown>;
}

function rec(r: Record<string, unknown>, key: string): Record<string, unknown> {
  return asRecord(r[key], key);
}

function optRec(r: Record<string, unknown>, key: string): Record<string, unknown> | undefined {
  return r[key] === undefined ? undefined : asRecord(r[key], key);
}

function str(r: Record<string, unknown>, key: string): string {
  const v = r[key];
  if (typeof v !== "string") throw new Error(`expected string field '${key}', got ${JSON.stringify(v)}`);
  return v;
}

function optStr(r: Record<string, unknown>, key: string): string | undefined {
  const v = r[key];
  if (v === undefined) return undefined;
  if (typeof v !== "string") throw new Error(`expected optional string '${key}', got ${JSON.stringify(v)}`);
  return v;
}

/** A field the Rust side always serializes, as `null` when unset. */
function strOrNull(r: Record<string, unknown>, key: string): string | null {
  const v = r[key];
  if (v === null) return null;
  if (typeof v !== "string") throw new Error(`expected string-or-null '${key}', got ${JSON.stringify(v)}`);
  return v;
}

function num(r: Record<string, unknown>, key: string): number {
  const v = r[key];
  if (typeof v !== "number") throw new Error(`expected number field '${key}', got ${JSON.stringify(v)}`);
  return v;
}

function optNum(r: Record<string, unknown>, key: string): number | undefined {
  const v = r[key];
  if (v === undefined) return undefined;
  if (typeof v !== "number") throw new Error(`expected optional number '${key}', got ${JSON.stringify(v)}`);
  return v;
}

function numOrNull(r: Record<string, unknown>, key: string): number | null {
  const v = r[key];
  if (v === null) return null;
  if (typeof v !== "number") throw new Error(`expected number-or-null '${key}', got ${JSON.stringify(v)}`);
  return v;
}

function bool(r: Record<string, unknown>, key: string): boolean {
  const v = r[key];
  if (typeof v !== "boolean") throw new Error(`expected boolean field '${key}', got ${JSON.stringify(v)}`);
  return v;
}

function optBool(r: Record<string, unknown>, key: string): boolean | undefined {
  const v = r[key];
  if (v === undefined) return undefined;
  if (typeof v !== "boolean") throw new Error(`expected optional boolean '${key}', got ${JSON.stringify(v)}`);
  return v;
}

function arr(r: Record<string, unknown>, key: string): unknown[] {
  const v = r[key];
  if (!Array.isArray(v)) throw new Error(`expected array field '${key}', got ${JSON.stringify(v)}`);
  return v;
}

function optArr(r: Record<string, unknown>, key: string): unknown[] | undefined {
  return r[key] === undefined ? undefined : arr(r, key);
}

function strArr(r: Record<string, unknown>, key: string): string[] {
  return arr(r, key).map((v, i) => {
    if (typeof v !== "string") throw new Error(`expected string at '${key}[${i}]'`);
    return v;
  });
}

function optStrArr(r: Record<string, unknown>, key: string): string[] | undefined {
  return r[key] === undefined ? undefined : strArr(r, key);
}

function numArr(r: Record<string, unknown>, key: string): number[] {
  return arr(r, key).map((v, i) => {
    if (typeof v !== "number") throw new Error(`expected number at '${key}[${i}]'`);
    return v;
  });
}

function recArr(r: Record<string, unknown>, key: string): Record<string, unknown>[] {
  return arr(r, key).map((v, i) => asRecord(v, `${key}[${i}]`));
}

function optRecArr(r: Record<string, unknown>, key: string): Record<string, unknown>[] | undefined {
  return r[key] === undefined ? undefined : recArr(r, key);
}

/** An arbitrary `serde_json::Value` field. */
function json(r: Record<string, unknown>, key: string): JsonValue {
  if (!(key in r)) throw new Error(`expected JSON field '${key}'`);
  return r[key] as JsonValue;
}

function optJson(r: Record<string, unknown>, key: string): JsonValue | undefined {
  return r[key] === undefined ? undefined : (r[key] as JsonValue);
}

function optJsonArr(r: Record<string, unknown>, key: string): JsonValue[] | undefined {
  return r[key] === undefined ? undefined : (arr(r, key) as JsonValue[]);
}

/** Pick the exact union member with tag field `type` equal to `T`. */
type Variant<U, T extends string> = Extract<U, { type: T }>;

function unknownTag(family: string, tag: string): never {
  throw new Error(`unmodeled or unknown ${family} tag: ${tag}`);
}

// ---------------------------------------------------------------------------
// artifact.rs
// ---------------------------------------------------------------------------

function reconstructDataClassification(r: Record<string, unknown>): DataClassification {
  const tag = str(r, "type");
  switch (tag) {
    case "Public":
    case "Internal":
    case "Confidential":
    case "Secret":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("DataClassification", tag);
  }
}

function reconstructArtifactRef(r: Record<string, unknown>): ArtifactRef {
  return {
    id: str(r, "id"),
    media_type: str(r, "media_type"),
    byte_length: num(r, "byte_length"),
    sha256: str(r, "sha256"),
    sensitivity: reconstructDataClassification(rec(r, "sensitivity")),
  };
}

// ---------------------------------------------------------------------------
// run.rs
// ---------------------------------------------------------------------------

function reconstructAgentMode(r: Record<string, unknown>): AgentMode {
  const tag = str(r, "type");
  switch (tag) {
    case "Ask":
    case "Explore":
    case "Plan":
    case "Build":
    case "Review":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("AgentMode", tag);
  }
}

function reconstructRunState(r: Record<string, unknown>): RunState {
  const tag = str(r, "type");
  switch (tag) {
    case "Queued":
    case "Preparing":
    case "Running":
    case "WaitingForApproval":
    case "WaitingForUserInput":
    case "Paused":
    case "Recovering":
    case "Completed":
    case "Failed":
    case "Cancelled":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("RunState", tag);
  }
}

function reconstructRunDisposition(r: Record<string, unknown>): RunDisposition {
  const tag = str(r, "type");
  switch (tag) {
    case "Completed":
      return { type: "Completed", summary: optStr(r, "summary") };
    case "Failed":
      return { type: "Failed", reason: str(r, "reason") };
    case "Cancelled":
      return { type: "Cancelled", reason: optStr(r, "reason") };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("RunDisposition", tag);
  }
}

function reconstructCheckpointKind(r: Record<string, unknown>): CheckpointKind {
  const tag = str(r, "type");
  switch (tag) {
    case "Stash":
    case "Commit":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("CheckpointKind", tag);
  }
}

function reconstructRiskLevel(r: Record<string, unknown>): RiskLevel {
  const tag = str(r, "type");
  switch (tag) {
    case "Low":
    case "Medium":
    case "High":
    case "Critical":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("RiskLevel", tag);
  }
}

function reconstructRisk(r: Record<string, unknown>): Risk {
  return { level: reconstructRiskLevel(rec(r, "level")), reasons: optStrArr(r, "reasons") };
}

function reconstructApprovalDecision(r: Record<string, unknown>): ApprovalDecision {
  const tag = str(r, "type");
  switch (tag) {
    case "Approve":
    case "Reject":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("ApprovalDecision", tag);
  }
}

function reconstructApprovalScope(r: Record<string, unknown>): ApprovalScope {
  const tag = str(r, "type");
  switch (tag) {
    case "Once":
    case "Run":
    case "Pattern":
    case "Repository":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("ApprovalScope", tag);
  }
}

function reconstructBudgetDimension(r: Record<string, unknown>): BudgetDimension {
  const tag = str(r, "type");
  switch (tag) {
    case "Tokens":
    case "Cost":
    case "WallClock":
    case "ToolCalls":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("BudgetDimension", tag);
  }
}

function reconstructToolOutcome(r: Record<string, unknown>): ToolOutcome {
  const tag = str(r, "type");
  switch (tag) {
    case "Succeeded":
      return { type: "Succeeded" };
    case "Failed":
      return { type: "Failed", message: str(r, "message") };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("ToolOutcome", tag);
  }
}

function reconstructPromptDelivery(r: Record<string, unknown>): PromptDelivery {
  const tag = str(r, "type");
  switch (tag) {
    case "Queue":
    case "Steer":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("PromptDelivery", tag);
  }
}

function reconstructPendingPromptView(r: Record<string, unknown>): PendingPromptView {
  return {
    id: str(r, "id"),
    text: str(r, "text"),
    mode: reconstructAgentMode(rec(r, "mode")),
    delivery: reconstructPromptDelivery(rec(r, "delivery")),
  };
}

function reconstructProposedAction(r: Record<string, unknown>): ProposedAction {
  const tag = str(r, "type");
  switch (tag) {
    case "ReadFiles":
      return { type: "ReadFiles", paths: strArr(r, "paths") };
    case "WritePatch":
      return { type: "WritePatch", patch: str(r, "patch") };
    case "ExecuteCommand": {
      // The S1 case: `environment` and `cwd` are always on the wire, so both
      // are required here. Dropping either from `ProposedAction` breaks this
      // line at compile time, not in an approval card at runtime.
      const action: Variant<ProposedAction, "ExecuteCommand"> = {
        type: "ExecuteCommand",
        program: str(r, "program"),
        args: strArr(r, "args"),
        environment: arr(r, "environment").map((pair, i) => {
          if (!Array.isArray(pair) || pair.length !== 2 || typeof pair[0] !== "string" || typeof pair[1] !== "string") {
            throw new Error(`expected a [string, string] pair at environment[${i}]`);
          }
          return [pair[0], pair[1]] as [string, string];
        }),
        cwd: strOrNull(r, "cwd"),
      };
      return action;
    }
    case "NetworkRequest":
      return { type: "NetworkRequest", destination: str(r, "destination") };
    case "GitCommit":
      return { type: "GitCommit", repository: str(r, "repository") };
    case "GitPush":
      return { type: "GitPush", remote: str(r, "remote"), branch: str(r, "branch") };
    case "GitHubMutation":
      return { type: "GitHubMutation", repository: str(r, "repository"), summary: str(r, "summary") };
    case "PublishDocument":
      return {
        type: "PublishDocument",
        document_id: str(r, "document_id"),
        target: str(r, "target"),
        changed_files: strArr(r, "changed_files"),
        git_action: str(r, "git_action"),
      };
    case "BlackboardPost":
      return { type: "BlackboardPost", workflow_run_id: str(r, "workflow_run_id"), kind: str(r, "kind") };
    case "BlackboardQuery":
      return { type: "BlackboardQuery", workflow_run_id: str(r, "workflow_run_id") };
    case "McpToolCall":
      return {
        type: "McpToolCall",
        server: str(r, "server"),
        tool: str(r, "tool"),
        summary: str(r, "summary"),
        args: str(r, "args"),
      };
    case "AcpToolCall":
      return { type: "AcpToolCall", agent: str(r, "agent"), title: str(r, "title"), details: str(r, "details") };
    case "RecordMemory":
      return { type: "RecordMemory" };
    case "SearchRegistry":
      return { type: "SearchRegistry" };
    case "DocumentEdit":
      return { type: "DocumentEdit", document_id: str(r, "document_id"), summary: str(r, "summary") };
    case "WorkflowQuery":
      return { type: "WorkflowQuery", workflow_run_id: str(r, "workflow_run_id") };
    case "WorkflowCreate":
      return { type: "WorkflowCreate", workflow_id: str(r, "workflow_id"), summary: str(r, "summary") };
    case "WorkflowRun":
      return {
        type: "WorkflowRun",
        workflow_id: str(r, "workflow_id"),
        kind: str(r, "kind"),
        summary: str(r, "summary"),
      };
    case "TaskWrite":
      return { type: "TaskWrite", repository: str(r, "repository"), summary: str(r, "summary") };
    case "TaskRead":
      return { type: "TaskRead", repository: str(r, "repository") };
    case "CouncilCreate":
      return { type: "CouncilCreate", name: str(r, "name"), summary: str(r, "summary") };
    case "CouncilRun":
      return { type: "CouncilRun", name: str(r, "name"), summary: str(r, "summary") };
    case "CouncilResultRead":
      return { type: "CouncilResultRead", selector: str(r, "selector") };
    case "CodeGraphQuery":
      return { type: "CodeGraphQuery", repository: str(r, "repository"), summary: str(r, "summary") };
    case "CodeGraphAssert":
      return { type: "CodeGraphAssert", repository: str(r, "repository"), summary: str(r, "summary") };
    case "AskUser":
      return { type: "AskUser", question_count: num(r, "question_count"), headers: strArr(r, "headers") };
    case "RestoreCheckpoint":
      return {
        type: "RestoreCheckpoint",
        run_id: str(r, "run_id"),
        ordinal: num(r, "ordinal"),
        worktree: str(r, "worktree"),
        commit: str(r, "commit"),
      };
    case "WriteProcessStdin":
      return { type: "WriteProcessStdin", process_id: num(r, "process_id"), byte_len: num(r, "byte_len") };
    case "PlanTransition":
      return { type: "PlanTransition", target: reconstructAgentMode(rec(r, "target")) };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("ProposedAction", tag);
  }
}

// ---------------------------------------------------------------------------
// question.rs
// ---------------------------------------------------------------------------

function reconstructQuestionOption(r: Record<string, unknown>): QuestionOption {
  return { label: str(r, "label"), description: optStr(r, "description") };
}

function reconstructQuestionPrompt(r: Record<string, unknown>): QuestionPrompt {
  return {
    question: str(r, "question"),
    header: str(r, "header"),
    options: recArr(r, "options").map(reconstructQuestionOption),
    multiple: bool(r, "multiple"),
    custom: bool(r, "custom"),
  };
}

function reconstructQuestionOutcome(r: Record<string, unknown>): QuestionOutcome {
  const tag = str(r, "type");
  switch (tag) {
    case "Answered":
      return {
        type: "Answered",
        answers: arr(r, "answers").map((row, i) => {
          if (!Array.isArray(row)) throw new Error(`expected string[] at answers[${i}]`);
          return row.map((v) => {
            if (typeof v !== "string") throw new Error(`expected string in answers[${i}]`);
            return v;
          });
        }),
      };
    case "Rejected":
      return { type: "Rejected", feedback: optStr(r, "feedback") };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("QuestionOutcome", tag);
  }
}

// ---------------------------------------------------------------------------
// ide.rs
// ---------------------------------------------------------------------------

function reconstructPosition(r: Record<string, unknown>): Position {
  return { line: num(r, "line"), character: num(r, "character") };
}

function reconstructRange(r: Record<string, unknown>): Range {
  return { start: reconstructPosition(rec(r, "start")), end: reconstructPosition(rec(r, "end")) };
}

function reconstructEditorSelection(r: Record<string, unknown>): EditorSelection {
  return { path: str(r, "path"), range: reconstructRange(rec(r, "range")) };
}

function reconstructDirtyBufferDigest(r: Record<string, unknown>): DirtyBufferDigest {
  return { path: str(r, "path"), sha256: str(r, "sha256"), byte_length: num(r, "byte_length") };
}

function reconstructIdeContextUpdate(r: Record<string, unknown>): IdeContextUpdate {
  const selection = optRec(r, "selection");
  return {
    active_file: optStr(r, "active_file"),
    selection: selection ? reconstructEditorSelection(selection) : undefined,
    open_files: optStrArr(r, "open_files"),
    dirty_buffers: optRecArr(r, "dirty_buffers")?.map(reconstructDirtyBufferDigest),
    diagnostics_revision: num(r, "diagnostics_revision"),
  };
}

function reconstructLocation(r: Record<string, unknown>): Location {
  const range = optRec(r, "range");
  return { path: str(r, "path"), range: range ? reconstructRange(range) : undefined };
}

function reconstructTextEdit(r: Record<string, unknown>): TextEdit {
  return { path: str(r, "path"), range: reconstructRange(rec(r, "range")), new_text: str(r, "new_text") };
}

function reconstructWorkspaceEdit(r: Record<string, unknown>): WorkspaceEdit {
  return { edits: optRecArr(r, "edits")?.map(reconstructTextEdit) };
}

function reconstructDiffRequest(r: Record<string, unknown>): DiffRequest {
  return {
    title: str(r, "title"),
    left_label: str(r, "left_label"),
    right_label: str(r, "right_label"),
    left: str(r, "left"),
    right: str(r, "right"),
  };
}

function reconstructIdeRequest(r: Record<string, unknown>): IdeRequest {
  const tag = str(r, "type");
  switch (tag) {
    case "ApplyEdit":
      return { type: "ApplyEdit", edit: reconstructWorkspaceEdit(rec(r, "edit")) };
    case "RevealLocation":
      return { type: "RevealLocation", location: reconstructLocation(rec(r, "location")) };
    case "ShowDiff":
      return { type: "ShowDiff", request: reconstructDiffRequest(rec(r, "request")) };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("IdeRequest", tag);
  }
}

function reconstructDiagnosticSeverity(r: Record<string, unknown>): DiagnosticSeverity {
  const tag = str(r, "type");
  switch (tag) {
    case "Error":
    case "Warning":
    case "Information":
    case "Hint":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("DiagnosticSeverity", tag);
  }
}

function reconstructDiagnostic(r: Record<string, unknown>): Diagnostic {
  return {
    path: str(r, "path"),
    range: reconstructRange(rec(r, "range")),
    severity: reconstructDiagnosticSeverity(rec(r, "severity")),
    message: str(r, "message"),
    source: optStr(r, "source"),
  };
}

function reconstructSourceProvenance(r: Record<string, unknown>): SourceProvenance {
  const tag = str(r, "type");
  switch (tag) {
    case "CommittedAt":
      return { type: "CommittedAt", revision: str(r, "revision") };
    case "Filesystem":
    case "UnsavedIdeBuffer":
    case "GeneratedPatch":
    case "AgentWorktree":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("SourceProvenance", tag);
  }
}

// ---------------------------------------------------------------------------
// error.rs
// ---------------------------------------------------------------------------

function reconstructUserAction(r: Record<string, unknown>): UserAction {
  const tag = str(r, "type");
  switch (tag) {
    case "Retry":
    case "Reauthenticate":
    case "GrantApproval":
    case "AdjustPolicy":
    case "ReconfigureModel":
    case "ContactSupport":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("UserAction", tag);
  }
}

function reconstructCodypendentError(r: Record<string, unknown>): CodypendentError {
  const userAction = optRec(r, "user_action");
  return {
    code: str(r, "code"),
    message: str(r, "message"),
    retryable: bool(r, "retryable"),
    user_action: userAction ? reconstructUserAction(userAction) : undefined,
    details: optJson(r, "details"),
    correlation_id: str(r, "correlation_id"),
  };
}

function reconstructProtocolError(r: Record<string, unknown>): ProtocolError {
  return { code: str(r, "code"), message: str(r, "message"), retryable: bool(r, "retryable") };
}

// ---------------------------------------------------------------------------
// capabilities.rs / version.rs / handshake.rs
// ---------------------------------------------------------------------------

function reconstructClientCapabilities(r: Record<string, unknown>): ClientCapabilities {
  return {
    rich_text: bool(r, "rich_text"),
    image_display: bool(r, "image_display"),
    audio_capture: bool(r, "audio_capture"),
    editor_mutations: bool(r, "editor_mutations"),
    diff_view: bool(r, "diff_view"),
    mouse: bool(r, "mouse"),
    unicode: bool(r, "unicode"),
    true_color: bool(r, "true_color"),
  };
}

function reconstructProtocolVersion(r: Record<string, unknown>): ProtocolVersion {
  return { major: num(r, "major"), minor: num(r, "minor") };
}

function reconstructClientHello(r: Record<string, unknown>): ClientHello {
  return {
    client_name: str(r, "client_name"),
    client_version: str(r, "client_version"),
    supported_protocols: recArr(r, "supported_protocols").map(reconstructProtocolVersion),
    capabilities: reconstructClientCapabilities(rec(r, "capabilities")),
    resume_token: optStr(r, "resume_token"),
  };
}

function reconstructServerHello(r: Record<string, unknown>): ServerHello {
  return {
    selected_protocol: reconstructProtocolVersion(rec(r, "selected_protocol")),
    daemon_version: str(r, "daemon_version"),
    daemon_instance: str(r, "daemon_instance"),
    heartbeat_interval_ms: num(r, "heartbeat_interval_ms"),
    resume_token: optStr(r, "resume_token"),
    build_id: str(r, "build_id"),
  };
}

function reconstructClientRole(r: Record<string, unknown>): ClientRole {
  const tag = str(r, "type");
  switch (tag) {
    case "Observer":
    case "Contributor":
    case "Controller":
    case "Approver":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("ClientRole", tag);
  }
}

function reconstructSubscription(r: Record<string, unknown>): Subscription {
  const tag = str(r, "type");
  switch (tag) {
    case "SessionSummary":
      return { type: "SessionSummary" };
    case "RunTrace":
      return { type: "RunTrace", run_id: str(r, "run_id") };
    case "AgentActivity":
      return { type: "AgentActivity" };
    case "RepositoryStatus":
      return { type: "RepositoryStatus" };
    case "BudgetState":
      return { type: "BudgetState" };
    case "Document":
      return { type: "Document", document_id: str(r, "document_id") };
    case "Blackboard":
      return { type: "Blackboard", workflow_run_id: str(r, "workflow_run_id") };
    case "Workflow":
      return { type: "Workflow", workflow_run_id: str(r, "workflow_run_id") };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("Subscription", tag);
  }
}

// ---------------------------------------------------------------------------
// document.rs
// ---------------------------------------------------------------------------

function reconstructSuggestionInput(r: Record<string, unknown>): SuggestionInput {
  return {
    block_id: str(r, "block_id"),
    range_start: num(r, "range_start"),
    range_end: num(r, "range_end"),
    replacement: str(r, "replacement"),
    rationale: optStr(r, "rationale"),
  };
}

/** Discriminated by `op`, not `type` (`#[serde(tag = "op")]`). */
function reconstructDocumentMutation(r: Record<string, unknown>): DocumentMutation {
  const tag = str(r, "op");
  switch (tag) {
    case "insert":
      return { op: "insert", index: num(r, "index"), block_id: str(r, "block_id"), content: json(r, "content") };
    case "delete":
      return { op: "delete", block_id: str(r, "block_id") };
    case "edit_text":
      return {
        op: "edit_text",
        block_id: str(r, "block_id"),
        position: num(r, "position"),
        delete_len: num(r, "delete_len"),
        insert: str(r, "insert"),
      };
    case "annotate":
      return { op: "annotate", suggestion: reconstructSuggestionInput(rec(r, "suggestion")) };
    case "accept_suggestion":
      return { op: "accept_suggestion", suggestion_id: str(r, "suggestion_id") };
    case "reject_suggestion":
      return { op: "reject_suggestion", suggestion_id: str(r, "suggestion_id") };
    case "unknown":
      return { op: "unknown" };
    default:
      return unknownTag("DocumentMutation", tag);
  }
}

/** Discriminated by `kind` (`#[serde(tag = "kind")]`). */
function reconstructPublishTarget(r: Record<string, unknown>): PublishTarget {
  const tag = str(r, "kind");
  switch (tag) {
    case "repository_file":
      return { kind: "repository_file", path: str(r, "path") };
    case "docs_branch_commit":
      return { kind: "docs_branch_commit", branch: str(r, "branch"), path: str(r, "path") };
    case "documentation_pr":
      return { kind: "documentation_pr", branch: str(r, "branch"), path: str(r, "path"), title: str(r, "title") };
    case "unknown":
      return { kind: "unknown" };
    default:
      return unknownTag("PublishTarget", tag);
  }
}

function reconstructDocumentSync(r: Record<string, unknown>): DocumentSync {
  return { document_id: str(r, "document_id"), revision: num(r, "revision"), update: numArr(r, "update") };
}

function reconstructDocumentEditLease(r: Record<string, unknown>): DocumentEditLease {
  return { document_id: str(r, "document_id"), block_id: optStr(r, "block_id") };
}

function reconstructDocumentLeaseGrant(r: Record<string, unknown>): DocumentLeaseGrant {
  return {
    lease_id: str(r, "lease_id"),
    document_id: str(r, "document_id"),
    block_id: optStr(r, "block_id"),
    expires_at: str(r, "expires_at"),
  };
}

// ---------------------------------------------------------------------------
// blackboard.rs
// ---------------------------------------------------------------------------

function reconstructBlackboardScope(r: Record<string, unknown>): BlackboardScope {
  const tag = str(r, "type");
  switch (tag) {
    case "WorkflowRun":
      return { type: "WorkflowRun", workflow_run_id: str(r, "workflow_run_id") };
    case "RepositoryBoard":
      return { type: "RepositoryBoard", repository: str(r, "repository") };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("BlackboardScope", tag);
  }
}

function reconstructBlackboardItemDraft(r: Record<string, unknown>): BlackboardItemDraft {
  return {
    kind: str(r, "kind"),
    payload: json(r, "payload"),
    confidence: optNum(r, "confidence"),
    evidence: optJsonArr(r, "evidence"),
    status: optStr(r, "status"),
    assignee: optStr(r, "assignee"),
    ordinal: optNum(r, "ordinal"),
  };
}

function reconstructBlackboardItemView(r: Record<string, unknown>): BlackboardItemView {
  return {
    id: str(r, "id"),
    workflow_run_id: str(r, "workflow_run_id"),
    kind: str(r, "kind"),
    payload: json(r, "payload"),
    author: json(r, "author"),
    confidence: optNum(r, "confidence"),
    evidence: optJsonArr(r, "evidence"),
    revision: num(r, "revision"),
    superseded_by: optStr(r, "superseded_by"),
    board_scope: optStr(r, "board_scope"),
    status: optStr(r, "status"),
    assignee: optStr(r, "assignee"),
    ordinal: optNum(r, "ordinal"),
  };
}

// ---------------------------------------------------------------------------
// workflow.rs
// ---------------------------------------------------------------------------

function reconstructWorkflowNodeState(r: Record<string, unknown>): WorkflowNodeState {
  const tag = str(r, "type");
  switch (tag) {
    case "Pending":
    case "Running":
    case "WaitingApproval":
    case "Blocked":
    case "Completed":
    case "Failed":
    case "Skipped":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("WorkflowNodeState", tag);
  }
}

function reconstructWorkflowRunPhase(r: Record<string, unknown>): WorkflowRunPhase {
  const tag = str(r, "type");
  switch (tag) {
    case "Pending":
    case "Running":
    case "Paused":
    case "Completed":
    case "Failed":
    case "Cancelled":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("WorkflowRunPhase", tag);
  }
}

function reconstructWorkflowNodeView(r: Record<string, unknown>): WorkflowNodeView {
  return {
    workflow_run_id: str(r, "workflow_run_id"),
    node_id: str(r, "node_id"),
    state: reconstructWorkflowNodeState(rec(r, "state")),
    attempt: num(r, "attempt"),
    cost: optJson(r, "cost"),
    error: optStr(r, "error"),
    warnings: optStrArr(r, "warnings"),
    depends_on: optStrArr(r, "depends_on"),
  };
}

function reconstructWorkflowRunSnapshot(r: Record<string, unknown>): WorkflowRunSnapshot {
  return {
    workflow_run_id: str(r, "workflow_run_id"),
    phase: reconstructWorkflowRunPhase(rec(r, "phase")),
    nodes: recArr(r, "nodes").map(reconstructWorkflowNodeView),
  };
}

function reconstructWorkflowEvent(r: Record<string, unknown>): WorkflowEvent {
  const tag = str(r, "type");
  switch (tag) {
    case "NodeTransitioned":
      return { type: "NodeTransitioned", ...reconstructWorkflowNodeView(r) };
    case "RunPhaseChanged":
      return {
        type: "RunPhaseChanged",
        workflow_run_id: str(r, "workflow_run_id"),
        phase: reconstructWorkflowRunPhase(rec(r, "phase")),
      };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("WorkflowEvent", tag);
  }
}

// ---------------------------------------------------------------------------
// memory.rs
// ---------------------------------------------------------------------------

function reconstructMemoryScope(r: Record<string, unknown>): MemoryScope {
  return { tier: str(r, "tier"), key: optStr(r, "key") };
}

function reconstructMemoryScopeTier(r: Record<string, unknown>): MemoryScopeTier {
  const tag = str(r, "type");
  switch (tag) {
    case "System":
    case "User":
    case "Repository":
    case "Unknown":
      return { type: tag };
    default:
      return unknownTag("MemoryScopeTier", tag);
  }
}

function reconstructMemoryView(r: Record<string, unknown>): MemoryView {
  return {
    id: str(r, "id"),
    scope: reconstructMemoryScope(rec(r, "scope")),
    class: str(r, "class"),
    statement: str(r, "statement"),
    structured_value: optJson(r, "structured_value"),
    confidence: num(r, "confidence"),
    observed_at: str(r, "observed_at"),
    sensitivity: reconstructDataClassification(rec(r, "sensitivity")),
    supersedes: optStrArr(r, "supersedes"),
    evidence: optStrArr(r, "evidence"),
  };
}

function reconstructMemoryEvidence(r: Record<string, unknown>): MemoryEvidence {
  const tag = str(r, "type");
  switch (tag) {
    case "Events":
      return { type: "Events", events: recArr(r, "events").map(reconstructSessionEvent) };
    case "Artifact":
      return { type: "Artifact", media_type: str(r, "media_type"), bytes_base64: str(r, "bytes_base64") };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("MemoryEvidence", tag);
  }
}

// ---------------------------------------------------------------------------
// codegraph.rs
// ---------------------------------------------------------------------------

function reconstructCodeGraphLanguageCount(r: Record<string, unknown>): CodeGraphLanguageCount {
  return { language: str(r, "language"), files: num(r, "files"), nodes: num(r, "nodes"), edges: num(r, "edges") };
}

function reconstructCodeGraphTally(r: Record<string, unknown>): CodeGraphTally {
  return { label: str(r, "label"), count: num(r, "count") };
}

function reconstructCodeGraphSkippedExtension(r: Record<string, unknown>): CodeGraphSkippedExtension {
  return { extension: str(r, "extension"), files: num(r, "files") };
}

function reconstructCodeGraphGrammar(r: Record<string, unknown>): CodeGraphGrammar {
  return { language: str(r, "language"), extensions: strArr(r, "extensions") };
}

function reconstructCodeGraphScanReport(r: Record<string, unknown>): CodeGraphScanReport {
  return {
    repository_root: str(r, "repository_root"),
    revision: str(r, "revision"),
    files_walked: num(r, "files_walked"),
    files_supported: num(r, "files_supported"),
    files_folded: num(r, "files_folded"),
    files_unsupported: num(r, "files_unsupported"),
    files_ignored: num(r, "files_ignored"),
    nodes: num(r, "nodes"),
    edges: num(r, "edges"),
    by_language: recArr(r, "by_language").map(reconstructCodeGraphLanguageCount),
    not_folded: optRecArr(r, "not_folded")?.map(reconstructCodeGraphSkippedExtension),
    grammars: optRecArr(r, "grammars")?.map(reconstructCodeGraphGrammar),
    file_cap: num(r, "file_cap"),
    cap_hit: bool(r, "cap_hit"),
    elapsed_ms: num(r, "elapsed_ms"),
  };
}

function reconstructCodeGraphStatusView(r: Record<string, unknown>): CodeGraphStatusView {
  return {
    repository_root: str(r, "repository_root"),
    nodes: num(r, "nodes"),
    edges: num(r, "edges"),
    files: num(r, "files"),
    by_language: recArr(r, "by_language").map(reconstructCodeGraphLanguageCount),
    by_kind: recArr(r, "by_kind").map(reconstructCodeGraphTally),
    revisions: recArr(r, "revisions").map(reconstructCodeGraphTally),
    head_revision: str(r, "head_revision"),
    working_tree_dirty: bool(r, "working_tree_dirty"),
    stale: bool(r, "stale"),
    stale_reason: optStr(r, "stale_reason"),
  };
}

function reconstructCodeGraphNodeView(r: Record<string, unknown>): CodeGraphNodeView {
  return {
    id: str(r, "id"),
    language: str(r, "language"),
    package: optStr(r, "package"),
    source_path: optStr(r, "source_path"),
    qualified_name: str(r, "qualified_name"),
    kind: str(r, "kind"),
    revision: str(r, "revision"),
  };
}

function reconstructCodeGraphEdgeAssertion(r: Record<string, unknown>): CodeGraphEdgeAssertion {
  return { session_id: str(r, "session_id"), run_id: str(r, "run_id"), rationale: str(r, "rationale") };
}

function reconstructCodeGraphEdgeView(r: Record<string, unknown>): CodeGraphEdgeView {
  const assertedBy = optRec(r, "asserted_by");
  return {
    from_id: str(r, "from_id"),
    from_name: str(r, "from_name"),
    to_id: str(r, "to_id"),
    to_name: str(r, "to_name"),
    relation: str(r, "relation"),
    confidence: num(r, "confidence"),
    evidence_kind: str(r, "evidence_kind"),
    revision: str(r, "revision"),
    asserted_by: assertedBy ? reconstructCodeGraphEdgeAssertion(assertedBy) : undefined,
  };
}

function reconstructCodeGraphQuery(r: Record<string, unknown>): CodeGraphQuery {
  return {
    path: optStr(r, "path"),
    language: optStr(r, "language"),
    kind: optStr(r, "kind"),
    name: optStr(r, "name"),
    node_id: optStr(r, "node_id"),
    include_edges: optBool(r, "include_edges"),
    include_nodes: optBool(r, "include_nodes"),
    limit: optNum(r, "limit"),
  };
}

function reconstructCodeGraphPage(r: Record<string, unknown>): CodeGraphPage {
  return {
    nodes: recArr(r, "nodes").map(reconstructCodeGraphNodeView),
    edges: recArr(r, "edges").map(reconstructCodeGraphEdgeView),
    total_nodes: num(r, "total_nodes"),
    total_edges: num(r, "total_edges"),
    limit: num(r, "limit"),
  };
}

// ---------------------------------------------------------------------------
// input.rs
// ---------------------------------------------------------------------------

function reconstructInputSource(r: Record<string, unknown>): InputSource {
  const tag = str(r, "type");
  switch (tag) {
    case "tui":
    case "ide":
    case "cli":
    case "web":
    case "voice":
    case "unknown":
      return { type: tag };
    default:
      return unknownTag("InputSource", tag);
  }
}

function reconstructTranscriptionMode(r: Record<string, unknown>): TranscriptionMode {
  const tag = str(r, "type");
  switch (tag) {
    case "local":
    case "remote":
    case "unknown":
      return { type: tag };
    default:
      return unknownTag("TranscriptionMode", tag);
  }
}

function reconstructScopeLevel(r: Record<string, unknown>): ScopeLevel {
  const tag = str(r, "type");
  switch (tag) {
    case "system":
    case "organization":
    case "user":
    case "workspace":
    case "repository":
    case "branch":
    case "session":
    case "task":
    case "unknown":
      return { type: tag };
    default:
      return unknownTag("ScopeLevel", tag);
  }
}

function reconstructGitHubRefKind(r: Record<string, unknown>): GitHubRefKind {
  const tag = str(r, "type");
  switch (tag) {
    case "pull-request":
    case "issue":
    case "commit":
    case "comment":
    case "unknown":
      return { type: tag };
    default:
      return unknownTag("GitHubRefKind", tag);
  }
}

function reconstructTranscript(r: Record<string, unknown>): Transcript {
  return {
    text: str(r, "text"),
    mode: reconstructTranscriptionMode(rec(r, "mode")),
    model: optStr(r, "model"),
    reviewed: bool(r, "reviewed"),
    source_audio: str(r, "source_audio"),
  };
}

function reconstructAudioArtifact(r: Record<string, unknown>): AudioArtifact {
  const transcript = optRec(r, "transcript");
  return {
    original: reconstructArtifactRef(rec(r, "original")),
    transcript: transcript ? reconstructTranscript(transcript) : undefined,
    duration_ms: optNum(r, "duration_ms"),
    sample_rate_hz: optNum(r, "sample_rate_hz"),
  };
}

function reconstructModelObservation(r: Record<string, unknown>): ModelObservation {
  return { text: str(r, "text"), model: optStr(r, "model") };
}

function reconstructImageRegion(r: Record<string, unknown>): ImageRegion {
  return {
    label: optStr(r, "label"),
    x: num(r, "x"),
    y: num(r, "y"),
    width: num(r, "width"),
    height: num(r, "height"),
  };
}

function reconstructImageArtifact(r: Record<string, unknown>): ImageArtifact {
  const extracted = optRec(r, "extracted_text");
  return {
    original: reconstructArtifactRef(rec(r, "original")),
    extracted_text: extracted ? reconstructArtifactRef(extracted) : undefined,
    observations: optRecArr(r, "observations")?.map(reconstructModelObservation),
    regions: optRecArr(r, "regions")?.map(reconstructImageRegion),
    width: optNum(r, "width"),
    height: optNum(r, "height"),
  };
}

function reconstructSymbolRef(r: Record<string, unknown>): SymbolRef {
  return { path: str(r, "path"), symbol: str(r, "symbol"), kind: optStr(r, "kind"), line: optNum(r, "line") };
}

function reconstructGitHubReference(r: Record<string, unknown>): GitHubReference {
  return {
    owner: str(r, "owner"),
    repo: str(r, "repo"),
    kind: reconstructGitHubRefKind(rec(r, "kind")),
    number: optNum(r, "number"),
    url: optStr(r, "url"),
  };
}

/** Discriminated by `block` (`#[serde(tag = "block")]`), kebab-case tags. */
function reconstructInputBlock(r: Record<string, unknown>): InputBlock {
  const tag = str(r, "block");
  switch (tag) {
    case "text":
      return { block: "text", text: str(r, "text") };
    case "audio":
      return { block: "audio", ...reconstructAudioArtifact(r) };
    case "image":
      return { block: "image", ...reconstructImageArtifact(r) };
    case "file":
      return { block: "file", ...reconstructArtifactRef(r) };
    case "editor-selection":
      return { block: "editor-selection", ...reconstructEditorSelection(r) };
    case "code-symbol":
      return { block: "code-symbol", ...reconstructSymbolRef(r) };
    case "github-reference":
      return { block: "github-reference", ...reconstructGitHubReference(r) };
    case "unknown":
      return { block: "unknown" };
    default:
      return unknownTag("InputBlock", tag);
  }
}

function reconstructInputEnvelope(r: Record<string, unknown>): InputEnvelope {
  return {
    source: reconstructInputSource(rec(r, "source")),
    blocks: recArr(r, "blocks").map(reconstructInputBlock),
    scope: reconstructScopeLevel(rec(r, "scope")),
    attachments: optRecArr(r, "attachments")?.map(reconstructArtifactRef),
  };
}

function reconstructOffDevicePolicy(r: Record<string, unknown>): OffDevicePolicy {
  return { max_off_device: reconstructDataClassification(rec(r, "max_off_device")) };
}

// ---------------------------------------------------------------------------
// events.rs
// ---------------------------------------------------------------------------

function reconstructActor(r: Record<string, unknown>): Actor {
  const tag = str(r, "type");
  switch (tag) {
    case "Human":
      return { type: "Human", user_id: str(r, "user_id") };
    case "Agent":
      return { type: "Agent", agent_id: str(r, "agent_id"), run_id: str(r, "run_id"), model: str(r, "model") };
    case "Client":
      return { type: "Client", client_id: str(r, "client_id") };
    case "Integration":
      return { type: "Integration", integration_id: str(r, "integration_id") };
    case "System":
      return { type: "System" };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("Actor", tag);
  }
}

function reconstructEventBody(r: Record<string, unknown>): EventBody {
  const tag = str(r, "type");
  switch (tag) {
    case "SessionCreated":
      return { type: "SessionCreated", title: str(r, "title") };
    case "NoteAppended":
      return { type: "NoteAppended", text: str(r, "text"), run_id: optStr(r, "run_id") };
    case "SessionClosed":
      return { type: "SessionClosed" };
    case "RunStarted":
      // The headline drift this suite exists to prevent: the wire field is
      // `mode: AgentMode`, NOT `model: string`.
      return {
        type: "RunStarted",
        run_id: str(r, "run_id"),
        objective: str(r, "objective"),
        mode: reconstructAgentMode(rec(r, "mode")),
      };
    case "RunStateChanged":
      return { type: "RunStateChanged", run_id: str(r, "run_id"), state: reconstructRunState(rec(r, "state")) };
    case "ModelStreamDelta":
      return { type: "ModelStreamDelta", run_id: str(r, "run_id"), text: str(r, "text") };
    case "ModelRetrying":
      return {
        type: "ModelRetrying",
        run_id: str(r, "run_id"),
        attempt: num(r, "attempt"),
        max_attempts: num(r, "max_attempts"),
        message: str(r, "message"),
        delay_ms: num(r, "delay_ms"),
      };
    case "ToolProposed":
      return {
        type: "ToolProposed",
        run_id: str(r, "run_id"),
        approval_id: str(r, "approval_id"),
        action: reconstructProposedAction(rec(r, "action")),
      };
    case "ToolDenied":
      return {
        type: "ToolDenied",
        run_id: str(r, "run_id"),
        action: reconstructProposedAction(rec(r, "action")),
        reasons: optStrArr(r, "reasons"),
      };
    case "ToolStarted":
      // Also drifted historically: the wire carries `tool`/`args_digest`/
      // `label`, never `call_id`/`tool_name`/`args`.
      return {
        type: "ToolStarted",
        run_id: str(r, "run_id"),
        tool: str(r, "tool"),
        args_digest: str(r, "args_digest"),
        label: optStr(r, "label"),
      };
    case "ToolCompleted": {
      const artifact = optRec(r, "artifact");
      return {
        type: "ToolCompleted",
        run_id: str(r, "run_id"),
        tool: str(r, "tool"),
        outcome: reconstructToolOutcome(rec(r, "outcome")),
        artifact: artifact ? reconstructArtifactRef(artifact) : undefined,
      };
    }
    case "PatchProposed":
      return {
        type: "PatchProposed",
        run_id: str(r, "run_id"),
        changeset_id: str(r, "changeset_id"),
        artifact: reconstructArtifactRef(rec(r, "artifact")),
        files: optStrArr(r, "files"),
        additions: optNum(r, "additions"),
        deletions: optNum(r, "deletions"),
        preview: optStr(r, "preview"),
        preview_truncated: optBool(r, "preview_truncated"),
      };
    case "ApprovalRequested":
      return {
        type: "ApprovalRequested",
        approval_id: str(r, "approval_id"),
        action: reconstructProposedAction(rec(r, "action")),
        risk: reconstructRisk(rec(r, "risk")),
        pattern: optStr(r, "pattern"),
      };
    case "ApprovalResolved":
      return {
        type: "ApprovalResolved",
        approval_id: str(r, "approval_id"),
        decision: reconstructApprovalDecision(rec(r, "decision")),
      };
    case "SteeringQueued":
      return { type: "SteeringQueued", run_id: str(r, "run_id") };
    case "SteeringApplied":
      return { type: "SteeringApplied", run_id: str(r, "run_id") };
    case "BudgetWarning":
      return {
        type: "BudgetWarning",
        run_id: str(r, "run_id"),
        dimension: reconstructBudgetDimension(rec(r, "dimension")),
        used: num(r, "used"),
        limit: num(r, "limit"),
      };
    case "ContextUsage":
      return {
        type: "ContextUsage",
        run_id: str(r, "run_id"),
        used_tokens: num(r, "used_tokens"),
        window_tokens: num(r, "window_tokens"),
        system_tokens: num(r, "system_tokens"),
        tool_tokens: num(r, "tool_tokens"),
        transcript_tokens: num(r, "transcript_tokens"),
      };
    case "RunCompleted":
      return {
        type: "RunCompleted",
        run_id: str(r, "run_id"),
        disposition: reconstructRunDisposition(rec(r, "disposition")),
        chronicle: reconstructArtifactRef(rec(r, "chronicle")),
      };
    case "RunUsage":
      return {
        type: "RunUsage",
        run_id: str(r, "run_id"),
        prompt_tokens: optNum(r, "prompt_tokens"),
        completion_tokens: optNum(r, "completion_tokens"),
        cost_micros: optNum(r, "cost_micros"),
      };
    case "LearningsCaptured":
      return {
        type: "LearningsCaptured",
        run_id: str(r, "run_id"),
        proposed_count: num(r, "proposed_count"),
        proposed_ids: optStrArr(r, "proposed_ids"),
        activated_count: num(r, "activated_count"),
        activated_ids: optStrArr(r, "activated_ids"),
      };
    case "ClientPresenceChanged":
      return {
        type: "ClientPresenceChanged",
        client_id: str(r, "client_id"),
        role: reconstructClientRole(rec(r, "role")),
        present: bool(r, "present"),
      };
    case "QuestionAsked":
      return {
        type: "QuestionAsked",
        question_id: str(r, "question_id"),
        run_id: str(r, "run_id"),
        questions: recArr(r, "questions").map(reconstructQuestionPrompt),
      };
    case "QuestionResolved":
      return {
        type: "QuestionResolved",
        question_id: str(r, "question_id"),
        outcome: reconstructQuestionOutcome(rec(r, "outcome")),
      };
    case "CheckpointRecorded":
      return {
        type: "CheckpointRecorded",
        run_id: str(r, "run_id"),
        checkpoint_id: str(r, "checkpoint_id"),
        ordinal: num(r, "ordinal"),
        kind: reconstructCheckpointKind(rec(r, "kind")),
        commit: str(r, "commit"),
        base_commit: str(r, "base_commit"),
      };
    case "CheckpointRestored":
      return {
        type: "CheckpointRestored",
        run_id: str(r, "run_id"),
        checkpoint_id: str(r, "checkpoint_id"),
        restored: bool(r, "restored"),
      };
    case "SessionForked":
      return { type: "SessionForked", from_session: str(r, "from_session"), checkpoint: str(r, "checkpoint") };
    case "PendingPromptsChanged":
      return { type: "PendingPromptsChanged", prompts: recArr(r, "prompts").map(reconstructPendingPromptView) };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("EventBody", tag);
  }
}

function reconstructSessionEvent(r: Record<string, unknown>): SessionEvent {
  return {
    sequence: num(r, "sequence"),
    occurred_at: str(r, "occurred_at"),
    causation_id: optStr(r, "causation_id"),
    correlation_id: optStr(r, "correlation_id"),
    actor: reconstructActor(rec(r, "actor")),
    body: reconstructEventBody(rec(r, "body")),
  };
}

// ---------------------------------------------------------------------------
// catchup.rs
// ---------------------------------------------------------------------------

function reconstructPendingApprovalProjection(r: Record<string, unknown>): PendingApprovalProjection {
  return {
    approval_id: str(r, "approval_id"),
    run_id: str(r, "run_id"),
    action: reconstructProposedAction(rec(r, "action")),
    risk: reconstructRisk(rec(r, "risk")),
  };
}

function reconstructSessionProjection(r: Record<string, unknown>): SessionProjection {
  return {
    session_id: str(r, "session_id"),
    title: str(r, "title"),
    last_sequence: num(r, "last_sequence"),
    active_runs: optStrArr(r, "active_runs"),
    pending_approvals: optRecArr(r, "pending_approvals")?.map(reconstructPendingApprovalProjection),
    pending_prompts: optRecArr(r, "pending_prompts")?.map(reconstructPendingPromptView),
    closed: bool(r, "closed"),
  };
}

function reconstructCatchup(r: Record<string, unknown>): Catchup {
  const tag = str(r, "type");
  switch (tag) {
    case "Events":
      return {
        type: "Events",
        from: num(r, "from"),
        through: num(r, "through"),
        events: recArr(r, "events").map(reconstructSessionEvent),
      };
    case "Snapshot":
      return {
        type: "Snapshot",
        through: num(r, "through"),
        projection: reconstructSessionProjection(rec(r, "projection")),
      };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("Catchup", tag);
  }
}

// ---------------------------------------------------------------------------
// command.rs
// ---------------------------------------------------------------------------

function reconstructCanaryMetrics(r: Record<string, unknown>): CanaryMetrics {
  return {
    sample_count: num(r, "sample_count"),
    error_rate_bps: num(r, "error_rate_bps"),
    baseline_error_rate_bps: num(r, "baseline_error_rate_bps"),
    p95_latency_ms: num(r, "p95_latency_ms"),
    baseline_p95_latency_ms: num(r, "baseline_p95_latency_ms"),
  };
}

function reconstructPromotionAction(r: Record<string, unknown>): PromotionAction {
  const tag = str(r, "type");
  switch (tag) {
    case "RunRegression":
    case "ReviewPermissions":
    case "StartShadow":
    case "StartCanary":
    case "FinishCanary":
    case "Unknown":
      return { type: tag };
    case "ObserveCanary":
      return { type: "ObserveCanary", metrics: reconstructCanaryMetrics(rec(r, "metrics")) };
    default:
      return unknownTag("PromotionAction", tag);
  }
}

function reconstructSessionSummary(r: Record<string, unknown>): SessionSummary {
  return {
    session_id: str(r, "session_id"),
    workspace_id: strOrNull(r, "workspace_id"),
    title: str(r, "title"),
    state: str(r, "state"),
    updated_at: str(r, "updated_at"),
    created_at: str(r, "created_at"),
  };
}

function reconstructFileMatchWire(r: Record<string, unknown>): FileMatchWire {
  return { path: str(r, "path"), indices: numArr(r, "indices"), score: num(r, "score") };
}

function reconstructCommandBody(r: Record<string, unknown>): CommandBody {
  const tag = str(r, "type");
  switch (tag) {
    case "InstallUiPlugin":
      return {
        type: "InstallUiPlugin",
        manifest_toml: str(r, "manifest_toml"),
        artifact_base64: str(r, "artifact_base64"),
        allow_unsigned: bool(r, "allow_unsigned"),
      };
    case "SmokeTestUiPlugin":
      return { type: "SmokeTestUiPlugin", plugin_id: str(r, "plugin_id") };
    case "EnableUiPlugin":
      return {
        type: "EnableUiPlugin",
        plugin_id: str(r, "plugin_id"),
        scope: str(r, "scope"),
        session_id: optStr(r, "session_id"),
      };
    case "ListUiPlugins":
      return { type: "ListUiPlugins" };
    case "UpdateUiPlugin":
      return {
        type: "UpdateUiPlugin",
        plugin_id: str(r, "plugin_id"),
        manifest_toml: str(r, "manifest_toml"),
        artifact_base64: str(r, "artifact_base64"),
        allow_unsigned: bool(r, "allow_unsigned"),
      };
    case "ApproveUiPluginUpdate":
      return {
        type: "ApproveUiPluginUpdate",
        plugin_id: str(r, "plugin_id"),
        approval_receipt: str(r, "approval_receipt"),
      };
    case "RejectUiPluginUpdate":
      return {
        type: "RejectUiPluginUpdate",
        plugin_id: str(r, "plugin_id"),
        approval_receipt: str(r, "approval_receipt"),
      };
    case "RevokeUiPlugin":
      return { type: "RevokeUiPlugin", plugin_id: str(r, "plugin_id") };
    case "RemoveTrustedUiPublisher":
      return { type: "RemoveTrustedUiPublisher", publisher_id: str(r, "publisher_id") };
    case "ListSessions":
      return { type: "ListSessions", workspace: optStr(r, "workspace"), limit: numOrNull(r, "limit") };
    case "SearchWorkspaceFiles":
      return {
        type: "SearchWorkspaceFiles",
        repository: str(r, "repository"),
        query: str(r, "query"),
        limit: numOrNull(r, "limit"),
      };
    case "CreateSession":
      return {
        type: "CreateSession",
        workspace: str(r, "workspace"),
        title: str(r, "title"),
        repository: optStr(r, "repository"),
      };
    case "CloseSession":
      return { type: "CloseSession", session_id: str(r, "session_id") };
    case "AttachSession":
      return {
        type: "AttachSession",
        session_id: str(r, "session_id"),
        last_seen_sequence: optNum(r, "last_seen_sequence"),
        subscriptions: recArr(r, "subscriptions").map(reconstructSubscription),
        requested_role: reconstructClientRole(rec(r, "requested_role")),
        repository: optStr(r, "repository"),
      };
    case "SubmitUserInput": {
      const envelope = optRec(r, "envelope");
      return {
        type: "SubmitUserInput",
        session_id: str(r, "session_id"),
        text: str(r, "text"),
        mode: reconstructAgentMode(rec(r, "mode")),
        model: optStr(r, "model"),
        envelope: envelope ? reconstructInputEnvelope(envelope) : undefined,
      };
    }
    case "StartRun":
      return {
        type: "StartRun",
        session_id: str(r, "session_id"),
        objective: str(r, "objective"),
        mode: reconstructAgentMode(rec(r, "mode")),
        repository: optStr(r, "repository"),
        model: optStr(r, "model"),
      };
    case "ResolveApproval":
      return {
        type: "ResolveApproval",
        approval_id: str(r, "approval_id"),
        decision: reconstructApprovalDecision(rec(r, "decision")),
        scope: reconstructApprovalScope(rec(r, "scope")),
      };
    case "ResolveQuestion":
      return {
        type: "ResolveQuestion",
        question_id: str(r, "question_id"),
        outcome: reconstructQuestionOutcome(rec(r, "outcome")),
      };
    case "CancelRun":
      return { type: "CancelRun", run_id: str(r, "run_id") };
    case "PauseRun":
      return { type: "PauseRun", run_id: str(r, "run_id") };
    case "ResumeRun":
      return { type: "ResumeRun", run_id: str(r, "run_id") };
    case "QueueSteering":
      return { type: "QueueSteering", run_id: str(r, "run_id"), text: str(r, "text") };
    case "UpdateIdeContext":
      return {
        type: "UpdateIdeContext",
        session_id: str(r, "session_id"),
        update: reconstructIdeContextUpdate(rec(r, "update")),
      };
    case "CreateDocument":
      return {
        type: "CreateDocument",
        title: str(r, "title"),
        scope: optStr(r, "scope"),
        repository: optStr(r, "repository"),
        initial_markdown: optStr(r, "initial_markdown"),
      };
    case "CheckDocuments":
      return { type: "CheckDocuments", repository: optStr(r, "repository"), session_id: optStr(r, "session_id") };
    case "MutateDocument":
      return {
        type: "MutateDocument",
        document_id: str(r, "document_id"),
        mutation: reconstructDocumentMutation(rec(r, "mutation")),
      };
    case "AcquireDocumentLease":
      return {
        type: "AcquireDocumentLease",
        lease: reconstructDocumentEditLease(rec(r, "lease")),
        ttl_seconds: optNum(r, "ttl_seconds"),
      };
    case "ReleaseDocumentLease":
      return { type: "ReleaseDocumentLease", lease_id: str(r, "lease_id") };
    case "PublishDocument":
      return {
        type: "PublishDocument",
        document_id: str(r, "document_id"),
        target: reconstructPublishTarget(rec(r, "target")),
      };
    case "StartWorkflow":
      return {
        type: "StartWorkflow",
        manifest: str(r, "manifest"),
        workflow_id: optStr(r, "workflow_id"),
        inputs: optJson(r, "inputs"),
        repository: optStr(r, "repository"),
      };
    case "PauseWorkflow":
      return { type: "PauseWorkflow", workflow_run_id: str(r, "workflow_run_id") };
    case "ResumeWorkflow":
      return { type: "ResumeWorkflow", workflow_run_id: str(r, "workflow_run_id") };
    case "RetryWorkflowNode":
      return {
        type: "RetryWorkflowNode",
        workflow_run_id: str(r, "workflow_run_id"),
        node_id: str(r, "node_id"),
      };
    case "CancelWorkflow":
      return { type: "CancelWorkflow", workflow_run_id: str(r, "workflow_run_id") };
    case "ReadWorkflowRun":
      return { type: "ReadWorkflowRun", workflow_run_id: str(r, "workflow_run_id") };
    case "ProposePromotion":
      return {
        type: "ProposePromotion",
        kind: str(r, "kind"),
        name: str(r, "name"),
        version: num(r, "version"),
        requires_permission_review: bool(r, "requires_permission_review"),
      };
    case "AdvancePromotion":
      return {
        type: "AdvancePromotion",
        candidate_id: str(r, "candidate_id"),
        action: reconstructPromotionAction(rec(r, "action")),
      };
    case "ApprovePromotion":
      return { type: "ApprovePromotion", candidate_id: str(r, "candidate_id") };
    case "RollbackPromotion":
      return { type: "RollbackPromotion", candidate_id: str(r, "candidate_id") };
    case "ReadBlackboard":
      return {
        type: "ReadBlackboard",
        workflow_run_id: str(r, "workflow_run_id"),
        kind: optStr(r, "kind"),
        include_superseded: optBool(r, "include_superseded"),
        board_repository: optStr(r, "board_repository"),
      };
    case "PostBlackboardItem":
      return {
        type: "PostBlackboardItem",
        scope: reconstructBlackboardScope(rec(r, "scope")),
        item: reconstructBlackboardItemDraft(rec(r, "item")),
      };
    case "UpdateBlackboardItem":
      return {
        type: "UpdateBlackboardItem",
        scope: reconstructBlackboardScope(rec(r, "scope")),
        item_id: str(r, "item_id"),
        status: optStr(r, "status"),
        assignee: optStr(r, "assignee"),
        ordinal: optNum(r, "ordinal"),
        payload: optJson(r, "payload"),
      };
    case "ReadSessionEvents":
      return {
        type: "ReadSessionEvents",
        session_id: str(r, "session_id"),
        after_sequence: optNum(r, "after_sequence"),
        limit: optNum(r, "limit"),
      };
    case "InspectMemory":
      return { type: "InspectMemory", id: str(r, "id"), repository: str(r, "repository") };
    case "CorrectMemory":
      return {
        type: "CorrectMemory",
        id: str(r, "id"),
        repository: str(r, "repository"),
        statement: str(r, "statement"),
        structured_value: optJson(r, "structured_value"),
        confidence: num(r, "confidence"),
      };
    case "ForgetMemory":
      return { type: "ForgetMemory", id: str(r, "id"), repository: str(r, "repository") };
    case "ForgetMemoryScope":
      return {
        type: "ForgetMemoryScope",
        repository: str(r, "repository"),
        tier: reconstructMemoryScopeTier(rec(r, "tier")),
      };
    case "OpenMemoryEvidence":
      return {
        type: "OpenMemoryEvidence",
        id: str(r, "id"),
        repository: str(r, "repository"),
        evidence_index: num(r, "evidence_index"),
      };
    case "SubmitEvalEvidence":
      return {
        type: "SubmitEvalEvidence",
        candidate_id: str(r, "candidate_id"),
        suite: str(r, "suite"),
        routing_policy: str(r, "routing_policy"),
        report_json: str(r, "report_json"),
      };
    case "PutArtifact":
      return {
        type: "PutArtifact",
        media_type: str(r, "media_type"),
        bytes_base64: str(r, "bytes_base64"),
        sensitivity: reconstructDataClassification(rec(r, "sensitivity")),
      };
    case "ReadArtifact":
      return {
        type: "ReadArtifact",
        artifact_id: str(r, "artifact_id"),
        offset: num(r, "offset"),
        limit: num(r, "limit"),
        expected_sha256: str(r, "expected_sha256"),
      };
    case "BuildCodeGraph":
      return { type: "BuildCodeGraph", repository: str(r, "repository") };
    case "ReadCodeGraphStatus":
      return { type: "ReadCodeGraphStatus", repository: str(r, "repository") };
    case "ReadCodeGraph":
      return {
        type: "ReadCodeGraph",
        repository: str(r, "repository"),
        query: reconstructCodeGraphQuery(rec(r, "query")),
      };
    case "RestoreCheckpoint":
      return { type: "RestoreCheckpoint", run_id: str(r, "run_id"), checkpoint: str(r, "checkpoint") };
    case "ForkSession":
      return {
        type: "ForkSession",
        session_id: str(r, "session_id"),
        checkpoint: str(r, "checkpoint"),
        name: optStr(r, "name"),
      };
    case "QueuePrompt":
      return {
        type: "QueuePrompt",
        session_id: str(r, "session_id"),
        text: str(r, "text"),
        mode: reconstructAgentMode(rec(r, "mode")),
        delivery: reconstructPromptDelivery(rec(r, "delivery")),
      };
    case "UpdateQueuedPrompt": {
      const delivery = optRec(r, "delivery");
      return {
        type: "UpdateQueuedPrompt",
        session_id: str(r, "session_id"),
        prompt_id: str(r, "prompt_id"),
        text: optStr(r, "text"),
        delivery: delivery ? reconstructPromptDelivery(delivery) : undefined,
      };
    }
    case "PromoteQueuedPrompt":
      return { type: "PromoteQueuedPrompt", session_id: str(r, "session_id"), prompt_id: str(r, "prompt_id") };
    case "DeleteQueuedPrompt":
      return { type: "DeleteQueuedPrompt", session_id: str(r, "session_id"), prompt_id: str(r, "prompt_id") };
    case "RunUserShell":
      return { type: "RunUserShell", session_id: str(r, "session_id"), command: str(r, "command") };
    case "RememberMemory":
      return { type: "RememberMemory", session_id: str(r, "session_id"), text: str(r, "text") };
    case "Unknown":
      return { type: "Unknown" };
    default:
      return unknownTag("CommandBody", tag);
  }
}

function reconstructCommand(r: Record<string, unknown>): Command {
  return {
    command_id: str(r, "command_id"),
    idempotency_key: str(r, "idempotency_key"),
    expected_revision: optNum(r, "expected_revision"),
    body: reconstructCommandBody(rec(r, "body")),
  };
}

// ---------------------------------------------------------------------------
// envelope.rs
// ---------------------------------------------------------------------------

function reconstructDaemonStatus(r: Record<string, unknown>): DaemonStatus {
  return {
    daemon_version: str(r, "daemon_version"),
    protocol_version: reconstructProtocolVersion(rec(r, "protocol_version")),
    instance_id: str(r, "instance_id"),
    pid: num(r, "pid"),
    started_at: str(r, "started_at"),
    uptime_seconds: num(r, "uptime_seconds"),
    boot_count: num(r, "boot_count"),
    database_path: str(r, "database_path"),
    socket_path: str(r, "socket_path"),
    session_count: num(r, "session_count"),
    build_id: str(r, "build_id"),
    active_run_count: num(r, "active_run_count"),
    integration_issues: optStrArr(r, "integration_issues"),
  };
}

function reconstructPayload(r: Record<string, unknown>): Payload {
  const tag = str(r, "type");
  switch (tag) {
    case "Ping":
    case "Pong":
    case "DaemonStatusRequest":
    case "Shutdown":
    case "ShutdownAck":
    case "ShutdownIfIdle":
    case "Unknown":
      return { type: tag };
    case "DaemonStatusResponse":
      return { type: "DaemonStatusResponse", ...reconstructDaemonStatus(r) };
    case "ShutdownRefused":
      return { type: "ShutdownRefused", active_run_count: num(r, "active_run_count") };
    case "Error":
      return { type: "Error", ...reconstructProtocolError(r) };
    case "ClientHello":
      return { type: "ClientHello", ...reconstructClientHello(r) };
    case "ServerHello":
      return { type: "ServerHello", ...reconstructServerHello(r) };
    case "Command":
      return { type: "Command", ...reconstructCommand(r) };
    case "CommandAccepted":
      return {
        type: "CommandAccepted",
        command_id: str(r, "command_id"),
        sequence: optNum(r, "sequence"),
        created_run: optStr(r, "created_run"),
      };
    case "CommandRejected":
      return { type: "CommandRejected", ...reconstructCodypendentError(r) };
    case "DocumentLeaseGranted":
      return {
        type: "DocumentLeaseGranted",
        command_id: str(r, "command_id"),
        grant: reconstructDocumentLeaseGrant(rec(r, "grant")),
      };
    case "DocumentCreated":
      return { type: "DocumentCreated", command_id: str(r, "command_id"), document_id: str(r, "document_id") };
    case "DocumentSync":
      return { type: "DocumentSync", ...reconstructDocumentSync(r) };
    case "DocumentPublishRequested":
      return {
        type: "DocumentPublishRequested",
        command_id: str(r, "command_id"),
        approval_id: str(r, "approval_id"),
        target: str(r, "target"),
        changed_files: strArr(r, "changed_files"),
        git_action: str(r, "git_action"),
      };
    case "DocsCheckCompleted":
      return {
        type: "DocsCheckCompleted",
        command_id: str(r, "command_id"),
        documents_checked: num(r, "documents_checked"),
        links_resolved: num(r, "links_resolved"),
        stale_findings: num(r, "stale_findings"),
        suggestions_filed: num(r, "suggestions_filed"),
      };
    case "SessionForked":
      return { type: "SessionForked", command_id: str(r, "command_id"), session_id: str(r, "session_id") };
    case "SessionList":
      return {
        type: "SessionList",
        command_id: str(r, "command_id"),
        sessions: recArr(r, "sessions").map(reconstructSessionSummary),
      };
    case "FileSearchResults":
      return {
        type: "FileSearchResults",
        command_id: str(r, "command_id"),
        query: str(r, "query"),
        matches: recArr(r, "matches").map(reconstructFileMatchWire),
        truncated: bool(r, "truncated"),
      };
    case "SessionEventsPage":
      return {
        type: "SessionEventsPage",
        command_id: str(r, "command_id"),
        session_id: str(r, "session_id"),
        events: recArr(r, "events").map(reconstructSessionEvent),
        through: num(r, "through"),
        has_more: bool(r, "has_more"),
      };
    case "Event":
      return { type: "Event", ...reconstructSessionEvent(r) };
    case "Catchup":
      return { type: "Catchup", catchup: reconstructCatchup(rec(r, "catchup")) };
    case "WorkflowRunStarted":
      return {
        type: "WorkflowRunStarted",
        command_id: str(r, "command_id"),
        workflow_run_id: str(r, "workflow_run_id"),
      };
    case "WorkflowRunSnapshot":
      return {
        type: "WorkflowRunSnapshot",
        command_id: str(r, "command_id"),
        snapshot: reconstructWorkflowRunSnapshot(rec(r, "snapshot")),
      };
    case "WorkflowEvent":
      return { type: "WorkflowEvent", event: reconstructWorkflowEvent(rec(r, "event")) };
    case "PromotionProposed":
      return { type: "PromotionProposed", command_id: str(r, "command_id"), candidate_id: str(r, "candidate_id") };
    case "UiPluginLifecycle":
      return {
        type: "UiPluginLifecycle",
        command_id: str(r, "command_id"),
        plugins: recArr(r, "plugins").map((p) => ({
          id: str(p, "id"),
          version: str(p, "version"),
          state: str(p, "state"),
          enabledScope: optStr(p, "enabledScope"),
          updateApprovalReceipt: optStr(p, "updateApprovalReceipt"),
          updatePermissionDiff: optStr(p, "updatePermissionDiff"),
        })),
      };
    case "ArtifactStored":
      return {
        type: "ArtifactStored",
        command_id: str(r, "command_id"),
        artifact: reconstructArtifactRef(rec(r, "artifact")),
      };
    case "ArtifactChunk":
      return {
        type: "ArtifactChunk",
        artifact_id: str(r, "artifact_id"),
        offset: num(r, "offset"),
        bytes_base64: str(r, "bytes_base64"),
        eof: bool(r, "eof"),
        sha256: str(r, "sha256"),
      };
    case "Memory":
      return { type: "Memory", command_id: str(r, "command_id"), memory: reconstructMemoryView(rec(r, "memory")) };
    case "MemoryForgotten":
      return { type: "MemoryForgotten", command_id: str(r, "command_id"), forgotten: strArr(r, "forgotten") };
    case "MemoryEvidence":
      return {
        type: "MemoryEvidence",
        command_id: str(r, "command_id"),
        evidence: reconstructMemoryEvidence(rec(r, "evidence")),
      };
    case "BlackboardItems":
      return {
        type: "BlackboardItems",
        command_id: str(r, "command_id"),
        items: recArr(r, "items").map(reconstructBlackboardItemView),
      };
    case "BlackboardPosted":
      return { type: "BlackboardPosted", ...reconstructBlackboardItemView(r) };
    case "BlackboardItemApplied":
      return {
        type: "BlackboardItemApplied",
        command_id: str(r, "command_id"),
        item: reconstructBlackboardItemView(rec(r, "item")),
      };
    case "CodeGraphBuilt":
      return {
        type: "CodeGraphBuilt",
        command_id: str(r, "command_id"),
        report: reconstructCodeGraphScanReport(rec(r, "report")),
      };
    case "CodeGraphStatus":
      return {
        type: "CodeGraphStatus",
        command_id: str(r, "command_id"),
        status: reconstructCodeGraphStatusView(rec(r, "status")),
      };
    case "CodeGraphPage":
      return {
        type: "CodeGraphPage",
        command_id: str(r, "command_id"),
        page: reconstructCodeGraphPage(rec(r, "page")),
      };
    case "RemoteUi":
      return {
        type: "RemoteUi",
        message: r.message as Extract<Payload, { type: "RemoteUi" }>["message"],
      };
    default:
      return unknownTag("Payload", tag);
  }
}

// ---------------------------------------------------------------------------
// Vector family -> reconstructor. A vector whose family is absent here is
// unmodeled, and the partition check below fails unless it is declared in
// `EXCLUDED_VECTORS`.
// ---------------------------------------------------------------------------

type Reconstructor = (r: Record<string, unknown>) => unknown;

const RECONSTRUCTORS: Readonly<Record<string, Reconstructor>> = {
  // artifact.rs
  ArtifactRef: reconstructArtifactRef,
  DataClassification: reconstructDataClassification,
  // run.rs
  AgentMode: reconstructAgentMode,
  RunState: reconstructRunState,
  RunDisposition: reconstructRunDisposition,
  ProposedAction: reconstructProposedAction,
  CheckpointKind: reconstructCheckpointKind,
  Risk: reconstructRisk,
  RiskLevel: reconstructRiskLevel,
  ApprovalDecision: reconstructApprovalDecision,
  ApprovalScope: reconstructApprovalScope,
  BudgetDimension: reconstructBudgetDimension,
  ToolOutcome: reconstructToolOutcome,
  PromptDelivery: reconstructPromptDelivery,
  PendingPromptView: reconstructPendingPromptView,
  // question.rs
  QuestionOption: reconstructQuestionOption,
  QuestionPrompt: reconstructQuestionPrompt,
  QuestionOutcome: reconstructQuestionOutcome,
  // ide.rs
  Position: reconstructPosition,
  Range: reconstructRange,
  EditorSelection: reconstructEditorSelection,
  DirtyBufferDigest: reconstructDirtyBufferDigest,
  IdeContextUpdate: reconstructIdeContextUpdate,
  Location: reconstructLocation,
  TextEdit: reconstructTextEdit,
  WorkspaceEdit: reconstructWorkspaceEdit,
  DiffRequest: reconstructDiffRequest,
  IdeRequest: reconstructIdeRequest,
  DiagnosticSeverity: reconstructDiagnosticSeverity,
  Diagnostic: reconstructDiagnostic,
  SourceProvenance: reconstructSourceProvenance,
  // error.rs
  UserAction: reconstructUserAction,
  CodypendentError: reconstructCodypendentError,
  ProtocolError: reconstructProtocolError,
  // capabilities.rs / version.rs / handshake.rs
  ClientCapabilities: reconstructClientCapabilities,
  ProtocolVersion: reconstructProtocolVersion,
  ClientHello: reconstructClientHello,
  ServerHello: reconstructServerHello,
  ClientRole: reconstructClientRole,
  Subscription: reconstructSubscription,
  // document.rs
  SuggestionInput: reconstructSuggestionInput,
  DocumentMutation: reconstructDocumentMutation,
  PublishTarget: reconstructPublishTarget,
  DocumentSync: reconstructDocumentSync,
  DocumentEditLease: reconstructDocumentEditLease,
  DocumentLeaseGrant: reconstructDocumentLeaseGrant,
  // blackboard.rs
  BlackboardScope: reconstructBlackboardScope,
  BlackboardItemDraft: reconstructBlackboardItemDraft,
  BlackboardItemView: reconstructBlackboardItemView,
  // workflow.rs
  WorkflowNodeState: reconstructWorkflowNodeState,
  WorkflowRunPhase: reconstructWorkflowRunPhase,
  WorkflowNodeView: reconstructWorkflowNodeView,
  WorkflowRunSnapshot: reconstructWorkflowRunSnapshot,
  WorkflowEvent: reconstructWorkflowEvent,
  // memory.rs
  MemoryScope: reconstructMemoryScope,
  MemoryScopeTier: reconstructMemoryScopeTier,
  MemoryView: reconstructMemoryView,
  MemoryEvidence: reconstructMemoryEvidence,
  // codegraph.rs
  CodeGraphScanReport: reconstructCodeGraphScanReport,
  CodeGraphStatusView: reconstructCodeGraphStatusView,
  CodeGraphPage: reconstructCodeGraphPage,
  CodeGraphQuery: reconstructCodeGraphQuery,
  // input.rs
  InputSource: reconstructInputSource,
  TranscriptionMode: reconstructTranscriptionMode,
  ScopeLevel: reconstructScopeLevel,
  GitHubRefKind: reconstructGitHubRefKind,
  Transcript: reconstructTranscript,
  AudioArtifact: reconstructAudioArtifact,
  ModelObservation: reconstructModelObservation,
  ImageRegion: reconstructImageRegion,
  ImageArtifact: reconstructImageArtifact,
  SymbolRef: reconstructSymbolRef,
  GitHubReference: reconstructGitHubReference,
  InputBlock: reconstructInputBlock,
  InputEnvelope: reconstructInputEnvelope,
  OffDevicePolicy: reconstructOffDevicePolicy,
  // events.rs — every `EventBody_*` vector is a whole SessionEvent.
  Actor: reconstructActor,
  EventBody: reconstructSessionEvent,
  SessionEvent: reconstructSessionEvent,
  // catchup.rs
  Catchup: reconstructCatchup,
  SessionProjection: reconstructSessionProjection,
  PendingApprovalProjection: reconstructPendingApprovalProjection,
  // command.rs
  CommandBody: reconstructCommandBody,
  Command: reconstructCommand,
  PromotionAction: reconstructPromotionAction,
  CanaryMetrics: reconstructCanaryMetrics,
  SessionSummary: reconstructSessionSummary,
  FileMatchWire: reconstructFileMatchWire,
  // envelope.rs
  DaemonStatus: reconstructDaemonStatus,
  Payload: reconstructPayload,
};

/**
 * Vectors deliberately NOT modeled, keyed by file. Empty today — every
 * committed vector is covered. A wire type this package chooses not to model
 * must be listed here with a reason; that is the only way past the partition
 * check below.
 */
const EXCLUDED_VECTORS: Readonly<Record<string, readonly string[]>> = {};

/**
 * Vector FILES deliberately not exercised. Empty today. A new file must either
 * get a describe block (it does automatically — the suite walks the directory)
 * or be listed here.
 */
const EXCLUDED_FILES: readonly string[] = [];

const VECTOR_FILES: string[] = readdirSync(VECTORS_DIR)
  .filter((name) => name.endsWith(".json"))
  .sort();

// ---------------------------------------------------------------------------
// The suite proper: every vector in every committed file.
// ---------------------------------------------------------------------------

describe("protocol-vectors/ file inventory", () => {
  it("finds the committed vector directory", () => {
    expect(VECTOR_FILES.length).toBeGreaterThan(0);
  });

  it("accounts for every committed vector file as covered or explicitly excluded", () => {
    const unaccounted = EXCLUDED_FILES.filter((name) => !VECTOR_FILES.includes(name));
    expect(unaccounted, "excluded file(s) that do not exist on disk").toEqual([]);
  });
});

for (const file of VECTOR_FILES) {
  if (EXCLUDED_FILES.includes(file)) continue;

  describe(`${file} against src/ types`, () => {
    const vectors = loadVectors(file);
    const names = Object.keys(vectors).sort();
    const excluded = EXCLUDED_VECTORS[file] ?? [];
    const covered = names.filter((name) => !excluded.includes(name));

    it("accounts for every vector as modeled or explicitly excluded", () => {
      const unmodeled = covered.filter((name) => RECONSTRUCTORS[vectorFamily(name)] === undefined);
      expect(unmodeled, `${file}: vector(s) with no reconstructor and no exclusion entry`).toEqual([]);
      const phantom = excluded.filter((name) => !names.includes(name));
      expect(phantom, `${file}: excluded name(s) that do not exist in the vector file`).toEqual([]);
    });

    for (const name of covered) {
      it(`decodes and re-encodes ${name} identically`, () => {
        const reconstruct = RECONSTRUCTORS[vectorFamily(name)];
        if (reconstruct === undefined) throw new Error(`no reconstructor for ${name}`);
        const original = vectors[name];
        expectReconstructionMatches(name, original, reconstruct(asRecord(original, name)));
      });
    }
  });
}

// ---------------------------------------------------------------------------
// Targeted assertions for the specific divergences that motivated this suite.
// The loop above would catch each of them, but these name them so a future
// failure reads as the regression it is.
// ---------------------------------------------------------------------------

describe("regressions these bindings previously shipped", () => {
  const events = loadVectors("events.json");

  it("RunStarted carries mode: AgentMode — there is no `model` field", () => {
    const body = rec(asRecord(events.EventBody_RunStarted, "EventBody_RunStarted"), "body");
    expect(Object.keys(body).sort()).toEqual(["mode", "objective", "run_id", "type"]);
    const decoded = reconstructEventBody(body);
    if (decoded.type !== "RunStarted") throw new Error(`expected RunStarted, got ${decoded.type}`);
    expect(decoded.mode).toEqual({ type: "Build" });
  });

  it("ToolStarted carries tool/args_digest/label — not call_id/tool_name/args", () => {
    const body = rec(asRecord(events.EventBody_ToolStarted, "EventBody_ToolStarted"), "body");
    expect(Object.keys(body).sort()).toEqual(["args_digest", "label", "run_id", "tool", "type"]);
    const decoded = reconstructEventBody(body);
    if (decoded.type !== "ToolStarted") throw new Error(`expected ToolStarted, got ${decoded.type}`);
    expect(decoded.args_digest).toBe("abc123");
    expect(decoded.label).toBe("cargo test --all-features");
  });

  it("ExecuteCommand keeps environment and cwd (the S1 drift)", () => {
    const body = rec(asRecord(events.EventBody_ApprovalRequested_ExecuteCommand, "S1"), "body");
    const action = reconstructProposedAction(rec(body, "action"));
    if (action.type !== "ExecuteCommand") throw new Error(`expected ExecuteCommand, got ${action.type}`);
    expect(action.environment).toEqual([
      ["RUST_BACKTRACE", "1"],
      ["PATH", "/usr/bin:/bin"],
    ]);
    expect(action.cwd).toBe("/home/user/project");
  });

  it("keeps an unmeasured RunUsage dimension absent rather than zero", () => {
    const usage = loadVectors("usage.json");
    const body = reconstructEventBody(rec(asRecord(usage.EventBody_RunUsage, "EventBody_RunUsage"), "body"));
    if (body.type !== "RunUsage") throw new Error(`expected RunUsage, got ${body.type}`);
    expect(body.prompt_tokens).toBe(1002);
    expect(body.completion_tokens).toBe(60);
    expect(body.cost_micros).toBeUndefined();
  });

  it("RunCompleted carries a disposition and a chronicle, not duration_ms/cost_usd", () => {
    const body = rec(asRecord(events.EventBody_RunCompleted, "EventBody_RunCompleted"), "body");
    expect(Object.keys(body).sort()).toEqual(["chronicle", "disposition", "run_id", "type"]);
  });
});

describe("forward compatibility (Rust's #[serde(other)] Unknown)", () => {
  it("preserves additive fields on known variants", () => {
    const payload = { type: "Ping", future_field: { nested: true } };
    const event = { type: "SessionClosed", future_field: 42 };
    const command = { type: "ListUiPlugins", future_field: "new" };

    expect(foldUnknownPayload(payload)).toBe(payload);
    expect(foldUnknownEventBody(event)).toBe(event);
    expect(foldUnknownCommandBody(command)).toBe(command);
  });

  it("folds an unrecognized Payload tag to Unknown", () => {
    expect(foldUnknownPayload({ type: "SomethingANewerDaemonSends" })).toEqual({ type: "Unknown" });
    expect(foldUnknownPayload({ type: "Ping" })).toEqual({ type: "Ping" });
  });

  it("folds an unrecognized EventBody tag to Unknown", () => {
    expect(foldUnknownEventBody({ type: "BrandNewEvent" })).toEqual({ type: "Unknown" });
    expect(foldUnknownEventBody({ type: "SessionClosed" })).toEqual({ type: "SessionClosed" });
  });

  it("folds an unrecognized CommandBody tag to Unknown", () => {
    expect(foldUnknownCommandBody({ type: "BrandNewCommand" })).toEqual({ type: "Unknown" });
    expect(foldUnknownCommandBody({ type: "ListUiPlugins" })).toEqual({ type: "ListUiPlugins" });
  });
});
