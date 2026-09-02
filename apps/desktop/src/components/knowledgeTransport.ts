/**
 * The typed call surface the knowledge surfaces (Skills, Memory, Docs, UI
 * plugins) need from the Tauri shell.
 *
 * The views are written against this interface and nothing else. The shell
 * implements it (`createKnowledgeTransport` in `../transport.ts`, over the
 * commands in `src-tauri/src/bridge.rs`); outside the shell — a browser tab, a
 * test — there is no transport, and a surface without one renders an explicit
 * UNAVAILABLE panel naming the exact commands it needs. It never renders a
 * plausible empty list, because "read, empty" and "never answered" are
 * different facts, and a read that fails says why for the same reason.
 *
 * The card shapes below mirror the TUI's own projections one-for-one
 * (`crates/tui/src/state.rs`: `SkillCard`, `MemoryCard`, `LearningCard`,
 * `DocCard`, `DocBlockView`, `DocSuggestionView`); the shell maps the same
 * `RegistryItem` / `MemoryRecord` / `LearningRecord` / document snapshot the
 * CLI harness maps in `crates/cli/src/tui.rs` (`src-tauri/src/knowledge.rs`),
 * so the two clients show the same facts about the same database.
 */
import type { PublishTarget, UiPluginLifecycleStatus } from "@codypendent/protocol";

/**
 * Whether a surface's data reflects a real answer.
 *
 * `"loaded"` is the only value that licenses an empty state; `"unavailable"`
 * carries the reason and must be shown instead.
 */
export type LoadStatus = "unloaded" | "loading" | "loaded" | "unavailable";

/** A surface's data plus whether it was actually read. */
export interface Loaded<T> {
  items: T[];
  status: LoadStatus;
  /** Why the read failed, when `status` is `"unavailable"`. */
  detail: string | null;
}

export function unloaded<T>(): Loaded<T> {
  return { items: [], status: "unloaded", detail: null };
}

/** `crates/tui/src/state.rs` `SkillCard`. `permissions` are verbatim strings. */
export interface SkillCard {
  name: string;
  kind: string;
  scope: string;
  trust: string;
  status: string;
  risk: string;
  description: string;
  /** Requested capabilities, one verbatim string each ("command: cargo"). */
  permissions: string[];
}

/**
 * `crates/tui/src/state.rs` `MemoryCard`, plus the memory's id.
 *
 * The TUI's card has no id because its browser only reads; the desktop offers
 * `CorrectMemory`/`ForgetMemory`, which are keyed by `{ id, repository }`.
 */
export interface MemoryCard {
  id: string;
  statement: string;
  /** `semantic` / `procedural` / `preference` / … */
  class: string;
  scope: string;
  revision: string;
  /** A date string, exactly as the store rendered it. */
  observed: string;
  confidence: number;
  /** The human-readable evidence source; "(no evidence)" when there is none. */
  source: string;
}

/** `crates/tui/src/state.rs` `LearningCard`. */
export interface LearningCard {
  id: string;
  statement: string;
  kind: string;
  state: string;
  scope: string;
  provenance: string;
  confidence: number;
  pinned: boolean;
  revision: number;
}

/** `crates/tui/src/action.rs` `LearningMutation`. */
export type LearningMutation =
  | { type: "Activate" }
  | { type: "Reject" }
  | { type: "SetPinned"; pinned: boolean }
  | { type: "EditStatement"; statement: string }
  | { type: "Delete" };

/** `crates/tui/src/state.rs` `DocBlockView`. */
export interface DocBlockView {
  id: string;
  kind: string;
  /** A one-line, lossy display rendering. */
  text: string;
  /**
   * The block's primary text VERBATIM, or `null` for a structured block with
   * no single editable container. This is what an edit prefills and what its
   * full replace deletes.
   */
  editable: string | null;
}

/** `crates/tui/src/state.rs` `DocSuggestionView`. */
export interface DocSuggestionView {
  id: string;
  block_id: string;
  source_revision: number;
  status: string;
  author: string;
  range: string;
  original: string;
  replacement: string;
  rationale: string | null;
}

/** `crates/tui/src/state.rs` `DocCard`. */
export interface DocCard {
  document_id: string;
  title: string;
  scope: string;
  status: string;
  /** `ask` / `suggest` / `edit` / `co_author` / `review` / `maintain`. */
  mode: string;
  /** Pre-rendered, e.g. `"r7"`. */
  revision: string;
  blocks: DocBlockView[];
  suggestions: DocSuggestionView[];
}

export type { UiPluginLifecycleStatus };

/**
 * What `PublishDocument` parked for approval — `Payload::DocumentPublishRequested`
 * as the shell projects it. Shown before any write; the approval card carries
 * the same plan.
 */
export interface DocumentPublishPlan {
  approval_id: string;
  target: string;
  changed_files: string[];
  git_action: string;
}

/**
 * Every bridge command the knowledge surfaces need.
 *
 * Each method is REQUIRED, not optional: a partially-implemented transport
 * would let a view offer an affordance that cannot fire. A surface either has
 * its transport or says it does not.
 */
export interface KnowledgeTransport {
  /** LOCAL SQLITE, read-only: `codypendent_knowledge::Registry::list`. */
  listSkills(): Promise<SkillCard[]>;

  /** LOCAL SQLITE, read-only: `MemoryStore::query` over the three scopes. */
  listMemories(): Promise<MemoryCard[]>;
  /** PROTOCOL: `CommandBody::CorrectMemory`. Returns the daemon's notice. */
  correctMemory(memoryId: string, statement: string): Promise<string>;
  /** PROTOCOL: `CommandBody::ForgetMemory`. Returns the daemon's notice. */
  forgetMemory(memoryId: string): Promise<string>;

  /** LOCAL SQLITE, read-only: `LearningStore::query`. */
  listLearnings(): Promise<LearningCard[]>;
  /**
   * LOCAL SQLITE, optimistic-revision write. Returns the outcome sentence the
   * TUI shows ("learning activated", "learning updated and returned to
   * review"); a conflict or duplicate rejects with its own message.
   */
  mutateLearning(
    learningId: string,
    revision: number,
    mutation: LearningMutation,
  ): Promise<string>;

  /** LOCAL SQLITE, read-only: `DocumentStore::list` + snapshots + suggestions. */
  listDocuments(): Promise<DocCard[]>;
  /** PROTOCOL: `CommandBody::CreateDocument`. Returns the new document id. */
  createDocument(title: string): Promise<string>;
  /**
   * PROTOCOL: `AcquireDocumentLease` on the block, then `MutateDocument` with
   * an `edit_text` op that deletes `original`'s code points at 0 and inserts
   * `replacement`, then `ReleaseDocumentLease`. A FULL REPLACE, not a prepend.
   */
  replaceDocumentBlock(
    documentId: string,
    blockId: string,
    original: string,
    replacement: string,
  ): Promise<void>;
  /** PROTOCOL: `MutateDocument` with a `delete` op, under the whole-document lease. */
  deleteDocumentBlock(documentId: string, blockId: string): Promise<void>;
  /**
   * PROTOCOL: `CommandBody::PublishDocument`. Nothing is written: the reply
   * is the plan the daemon parked for a human to approve.
   */
  publishDocument(documentId: string, target: PublishTarget): Promise<DocumentPublishPlan>;

  /** PROTOCOL: `CommandBody::ListUiPlugins` → `Payload::UiPluginLifecycle`. */
  listUiPlugins(): Promise<UiPluginLifecycleStatus[]>;
  /** PROTOCOL: `SmokeTestUiPlugin`. */
  smokeTestUiPlugin(pluginId: string): Promise<UiPluginLifecycleStatus[]>;
  /** PROTOCOL: `EnableUiPlugin { plugin_id, scope, session_id }`. */
  enableUiPlugin(pluginId: string, scope: string): Promise<UiPluginLifecycleStatus[]>;
  /** PROTOCOL: `ApproveUiPluginUpdate` with the daemon-issued receipt. */
  approveUiPluginUpdate(
    pluginId: string,
    approvalReceipt: string,
  ): Promise<UiPluginLifecycleStatus[]>;
  /** PROTOCOL: `RejectUiPluginUpdate` with the daemon-issued receipt. */
  rejectUiPluginUpdate(
    pluginId: string,
    approvalReceipt: string,
  ): Promise<UiPluginLifecycleStatus[]>;
  /** PROTOCOL: `RevokeUiPlugin`. */
  revokeUiPlugin(pluginId: string): Promise<UiPluginLifecycleStatus[]>;
}

/**
 * The sentence a surface shows when the shell has no handler for it yet.
 * Names the exact commands so the gap is actionable rather than mysterious.
 */
export function missingBridge(commands: readonly string[]): string {
  const list = commands.join(", ");
  return (
    `Unavailable: this build has no shell bridge for this surface ` +
    `(needs ${list} from apps/desktop/src-tauri/src/bridge.rs). ` +
    `Nothing is shown rather than an empty list, because an empty list would ` +
    `assert there is nothing here.`
  );
}
