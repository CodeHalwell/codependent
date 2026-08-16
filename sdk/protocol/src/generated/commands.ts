/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * The specific change a command requests. A wire enum: internally tagged with an [`CommandBody::Unknown`] fallback so a command from a newer client deserializes and is rejected structurally rather than crashing the peer.
 */
export type CommandBody =
  | {
      allow_unsigned?: boolean;
      artifact_base64: string;
      manifest_toml: string;
      type: "InstallUiPlugin";
    }
  | {
      plugin_id: string;
      type: "SmokeTestUiPlugin";
    }
  | {
      plugin_id: string;
      scope: string;
      session_id?: string | null;
      type: "EnableUiPlugin";
    }
  | {
      type: "ListUiPlugins";
    }
  | {
      allow_unsigned?: boolean;
      artifact_base64: string;
      manifest_toml: string;
      plugin_id: string;
      type: "UpdateUiPlugin";
    }
  | {
      approval_receipt: string;
      plugin_id: string;
      type: "ApproveUiPluginUpdate";
    }
  | {
      approval_receipt: string;
      plugin_id: string;
      type: "RejectUiPluginUpdate";
    }
  | {
      plugin_id: string;
      type: "RevokeUiPlugin";
    }
  | {
      publisher_id: string;
      type: "RemoveTrustedUiPublisher";
    }
  | {
      /**
       * Hard cap on returned rows (the daemon also caps at 200).
       */
      limit?: number | null;
      type: "ListSessions";
      /**
       * Restrict to one workspace; `None` lists all.
       */
      workspace?: string | null;
    }
  | {
      limit?: number | null;
      query: string;
      repository: string;
      type: "SearchWorkspaceFiles";
    }
  | {
      /**
       * The canonical filesystem root of the repository this session operates on, so the daemon can build its code graph on open (not only on the first run). `#[serde(default)]` keeps older clients (which send none) working.
       */
      repository?: string | null;
      title: string;
      type: "CreateSession";
      workspace: string;
    }
  | {
      session_id: string;
      type: "CloseSession";
    }
  | {
      last_seen_sequence?: number | null;
      /**
       * The canonical filesystem root of the repository this session operates on, so the daemon can build its code graph on open (not only on the first run). `#[serde(default)]` keeps older clients (which send none) working.
       */
      repository?: string | null;
      requested_role: ClientRole;
      session_id: string;
      subscriptions: Subscription[];
      type: "AttachSession";
    }
  | {
      /**
       * The full multimodal input this submission normalizes (voice v1, rubric 8): a typed [`InputEnvelope`] whose blocks may reference artifacts previously stored via [`PutArtifact`](CommandBody::PutArtifact) — e.g. an [`InputBlock::Audio`](crate::input::InputBlock::Audio) carrying a recorded voice note. When the envelope carries audio without a transcript, the daemon transcribes it (through its transcription seam, gated by the [`transcription_allowed`](crate::input::transcription_allowed) classification math) and the transcript text becomes the run input; the original audio stays linked to its transcript (the original-is-never-replaced invariant). Additive (`#[serde(default)]`): an older client omits it and `text` alone drives the run, exactly as before this field existed.
       */
      envelope?: InputEnvelope | null;
      mode: AgentMode;
      /**
       * The model to **pin** this continuation to (mid-conversation model switch). When the operator re-picks a model in the `/model` picker, the very next follow-up in the SAME session carries it here so the switch is instant — no restart, no new session. `Some(id)` runs this continuation on exactly that model AND makes it the session's current pin (a later follow-up that carries none inherits it via [`session_run_provenance`](crate) recovery). `None` is unchanged behavior: the continuation inherits the session's existing model from its originating `StartRun`. Mirrors [`StartRun.model`](CommandBody::StartRun::model): `#[serde(default)]` keeps an older client (which sends none) working — the daemon then resolves the model exactly as before this field existed.
       */
      model?: string | null;
      session_id: string;
      text: string;
      type: "SubmitUserInput";
    }
  | {
      mode: AgentMode;
      /**
       * The model to **pin** this run to (STEP MP2): when the operator picks a model in the `/model` picker, the run executes on exactly that model instead of the router's/resolver's choice. A pin overrides the daemon's *quality* judgment, never its *security* constraint — a pinned hosted model for classified data is refused (fail-closed), never silently run off-device (enforced in the executor). Mirrors [`repository`](CommandBody::StartRun::repository): `#[serde(default)]` keeps an older client (which sends none) working — the daemon then resolves the model exactly as before this field existed.
       */
      model?: string | null;
      objective: string;
      /**
       * The canonical filesystem root of the repository this run operates on. A per-user daemon can serve several checkouts over one socket, so the run — not the daemon's startup working directory — must decide which repository its context map and curated memories are attributed to (issue #6 item 1). `#[serde(default)]` keeps an older client (which sends none) working: the daemon then falls back to its own directory, exactly as before this field existed.
       */
      repository?: string | null;
      session_id: string;
      type: "StartRun";
    }
  | {
      approval_id: string;
      decision: ApprovalDecision;
      scope: ApprovalScope;
      type: "ResolveApproval";
    }
  | {
      outcome: QuestionOutcome;
      question_id: string;
      type: "ResolveQuestion";
    }
  | {
      run_id: string;
      type: "CancelRun";
    }
  | {
      run_id: string;
      type: "PauseRun";
    }
  | {
      run_id: string;
      type: "ResumeRun";
    }
  | {
      run_id: string;
      text: string;
      type: "QueueSteering";
    }
  | {
      session_id: string;
      type: "UpdateIdeContext";
      update: IdeContextUpdate;
    }
  | {
      /**
       * Markdown to seed the document's blocks from (`docs new --from file.md`, the agent's `docs.create`). Imported lossily-but- reasonably at block granularity; absent creates an empty document.
       */
      initial_markdown?: string | null;
      /**
       * The canonical filesystem root of the repository a repository-scoped document belongs to. Mirrors [`StartRun.repository`](CommandBody::StartRun::repository): `#[serde(default)]` keeps an older client working — the daemon then falls back to its own startup root.
       */
      repository?: string | null;
      /**
       * The scope to create the document in: `"repository"` (the default when absent — the document lives with the checkout), `"system"`, or `"organization:<id>"` (organization docs default to suggest-only agent collaboration). An unrecognized value is rejected `document.invalid-scope`, never guessed at.
       */
      scope?: string | null;
      /**
       * The document title (non-empty; the daemon rejects a blank one).
       */
      title: string;
      type: "CreateDocument";
    }
  | {
      /**
       * The canonical filesystem root of the repository whose code graph the links resolve against. Mirrors [`StartRun.repository`](CommandBody::StartRun::repository); absent falls back to the daemon's startup root.
       */
      repository?: string | null;
      /**
       * A session to surface the result into: when set and the sweep found anything stale, the daemon appends a `NoteAppended` to this session's ledger so the finding count reaches the active conversation. Absent, the counts ride only on the reply.
       */
      session_id?: string | null;
      type: "CheckDocuments";
    }
  | {
      document_id: string;
      mutation: DocumentMutation;
      type: "MutateDocument";
    }
  | {
      lease: DocumentEditLease;
      /**
       * How long the lease is valid, in seconds; the daemon applies a default when absent. A re-acquire by the same holder renews the expiry in place.
       */
      ttl_seconds?: number | null;
      type: "AcquireDocumentLease";
    }
  | {
      lease_id: string;
      type: "ReleaseDocumentLease";
    }
  | {
      document_id: string;
      target: PublishTarget;
      type: "PublishDocument";
    }
  | {
      inputs?: JsonValue;
      /**
       * The workflow manifest YAML (the content of a `workflow.yaml`). Empty when [`workflow_id`](CommandBody::StartWorkflow::workflow_id) names a workflow the daemon resolves from its own sources instead.
       */
      manifest: string;
      /**
       * The canonical filesystem root of the repository this workflow's agent nodes operate on. A per-user daemon can serve several checkouts over one socket, so the run — not the daemon's startup working directory — must decide which repository its agent nodes' isolated worktrees are carved from (Phase 5 T5, fixing P5-D1). Mirrors [`StartRun.repository`](CommandBody::StartRun): `#[serde(default)]` keeps an older client (which sends none) working — the daemon then falls back to its own startup repository root, never a wandering `current_dir()` at node-execution time.
       */
      repository?: string | null;
      type: "StartWorkflow";
      /**
       * A named workflow to resolve from the daemon's sources (embedded built-ins, the user config directory, and the run repository's `.codypendent/workflows`) rather than shipping the manifest inline — the path `/fix-ci` takes (`repair-github-check`). Additive (`#[serde(default)]`): an older client omits it and ships `manifest`. When set, `manifest` is ignored and the daemon enforces the workflow registry's version-stability + shadowing rules at resolution.
       */
      workflow_id?: string | null;
    }
  | {
      type: "PauseWorkflow";
      workflow_run_id: string;
    }
  | {
      type: "ResumeWorkflow";
      workflow_run_id: string;
    }
  | {
      /**
       * The node id to re-drive from (its transitive dependents reset with it).
       */
      node_id: string;
      type: "RetryWorkflowNode";
      workflow_run_id: string;
    }
  | {
      type: "CancelWorkflow";
      workflow_run_id: string;
    }
  | {
      type: "ReadWorkflowRun";
      workflow_run_id: string;
    }
  | {
      kind: string;
      name: string;
      requires_permission_review?: boolean;
      type: "ProposePromotion";
      version: number;
    }
  | {
      action: PromotionAction;
      candidate_id: string;
      type: "AdvancePromotion";
    }
  | {
      candidate_id: string;
      type: "ApprovePromotion";
    }
  | {
      candidate_id: string;
      type: "RollbackPromotion";
    }
  | {
      /**
       * Read a **repository task board** instead of a workflow run's board (Phase B kanban). When set, `workflow_run_id` is ignored: the daemon resolves the board to the synthetic run [`board_scope_id`](crate::blackboard::board_scope_id) names (an empty board for a repository never written to — a read creates nothing). Additive (`#[serde(default)]`): an older client omits it and reads a run board exactly as before.
       */
      board_repository?: string | null;
      /**
       * Include superseded revisions too; the default (`false`) returns only the live board (the "live-only" view the TUI shows).
       */
      include_superseded?: boolean;
      /**
       * A blackboard artifact kind to filter by (`finding`, `decision`, …), or all kinds when absent.
       */
      kind?: string | null;
      type: "ReadBlackboard";
      workflow_run_id: string;
    }
  | {
      /**
       * The artifact to store (kind, payload, evidence, board fields).
       */
      item: BlackboardItemDraft;
      /**
       * The board to post onto: a workflow run's, or a repository's task board (created on first write).
       */
      scope: BlackboardScope;
      type: "PostBlackboardItem";
    }
  | {
      /**
       * The new assignee, when re-assigning.
       */
      assignee?: string | null;
      /**
       * The live item to supersede. An already-superseded item is refused (`blackboard.already-superseded`), so concurrent moves never fork.
       */
      item_id: string;
      /**
       * The new within-column position, when re-ordering. When only `status` changes, the daemon appends to the end of the target column.
       */
      ordinal?: number | null;
      payload?: JsonValue;
      /**
       * The board holding the item.
       */
      scope: BlackboardScope;
      /**
       * The new column, when moving.
       */
      status?: string | null;
      type: "UpdateBlackboardItem";
    }
  | {
      /**
       * Return events strictly **after** this sequence (0 = from the start).
       */
      after_sequence?: number;
      /**
       * Maximum events in the page. 0 (or absent) asks for the server default; the server clamps any request to its own page ceiling.
       */
      limit?: number;
      session_id: string;
      type: "ReadSessionEvents";
    }
  | {
      id: string;
      repository: string;
      type: "InspectMemory";
    }
  | {
      confidence: number;
      id: string;
      repository: string;
      statement: string;
      structured_value?: JsonValue;
      type: "CorrectMemory";
    }
  | {
      id: string;
      repository: string;
      type: "ForgetMemory";
    }
  | {
      repository: string;
      tier: MemoryScopeTier;
      type: "ForgetMemoryScope";
    }
  | {
      evidence_index: number;
      id: string;
      repository: string;
      type: "OpenMemoryEvidence";
    }
  | {
      candidate_id: string;
      /**
       * A serialized `codypendent_eval::SuiteReport`. Opaque here: protocol must not depend on `codypendent-eval`.
       */
      report_json: string;
      /**
       * The routing policy the suite ran under, or `daemon-default`.
       */
      routing_policy: string;
      /**
       * The eval suite that ran. The regression gate consumes `core`.
       */
      suite: string;
      type: "SubmitEvalEvidence";
    }
  | {
      /**
       * The raw bytes, base64-encoded (standard alphabet, with padding).
       */
      bytes_base64: string;
      /**
       * IANA media type of the bytes, e.g. `audio/wav`.
       */
      media_type: string;
      /**
       * The stored occurrence's data classification.
       */
      sensitivity: DataClassification;
      type: "PutArtifact";
    }
  | {
      artifact_id: string;
      expected_sha256: string;
      limit: number;
      offset: number;
      type: "ReadArtifact";
    }
  | {
      /**
       * The directory to build from; resolved to its enclosing checkout.
       */
      repository: string;
      type: "BuildCodeGraph";
    }
  | {
      repository: string;
      type: "ReadCodeGraphStatus";
    }
  | {
      query?: CodeGraphQuery;
      repository: string;
      type: "ReadCodeGraph";
    }
  | {
      checkpoint: string;
      run_id: string;
      type: "RestoreCheckpoint";
    }
  | {
      checkpoint: string;
      /**
       * The fork's title; absent derives `"<source title> (fork)"` with an opencode-style `#N` auto-increment on collision.
       */
      name?: string | null;
      session_id: string;
      type: "ForkSession";
    }
  | {
      delivery: PromptDelivery;
      mode: AgentMode;
      session_id: string;
      text: string;
      type: "QueuePrompt";
    }
  | {
      delivery?: PromptDelivery | null;
      prompt_id: string;
      session_id: string;
      text?: string | null;
      type: "UpdateQueuedPrompt";
    }
  | {
      prompt_id: string;
      session_id: string;
      type: "PromoteQueuedPrompt";
    }
  | {
      prompt_id: string;
      session_id: string;
      type: "DeleteQueuedPrompt";
    }
  | {
      command: string;
      session_id: string;
      type: "RunUserShell";
    }
  | {
      session_id: string;
      text: string;
      type: "RememberMemory";
    }
  | {
      type: "Unknown";
    };
/**
 * A client's authority over a session it observes (Chapter 03). Exclusivity is attached to specific resources (leases), not to the whole session.
 */
type ClientRole =
  | {
      type: "Observer";
    }
  | {
      type: "Contributor";
    }
  | {
      type: "Controller";
    }
  | {
      type: "Approver";
    }
  | {
      type: "Unknown";
    };
/**
 * A projection view a client subscribes to, rather than receiving every internal event (Chapter 03). This is the Phase 1 subset; document, workflow, and GitHub views arrive with their features.
 */
type Subscription =
  | {
      type: "SessionSummary";
    }
  | {
      run_id: string;
      type: "RunTrace";
    }
  | {
      type: "AgentActivity";
    }
  | {
      type: "RepositoryStatus";
    }
  | {
      type: "BudgetState";
    }
  | {
      document_id: string;
      type: "Document";
    }
  | {
      type: "Blackboard";
      workflow_run_id: string;
    }
  | {
      type: "Workflow";
      workflow_run_id: string;
    }
  | {
      type: "Unknown";
    };
/**
 * How sensitive an artifact's contents are.
 *
 * Ordered least to most restrictive; higher classifications gate model routing, export, and display. A wire enum, so it is internally tagged and carries an [`DataClassification::Unknown`] fallback for forward compatibility.
 */
type DataClassification =
  | {
      type: "Public";
    }
  | {
      type: "Internal";
    }
  | {
      type: "Confidential";
    }
  | {
      type: "Secret";
    }
  | {
      type: "Unknown";
    };
/**
 * One typed block within an [`InputEnvelope`]. Internally tagged on the wire with a `block` discriminant (`{"block": "...", …fields}`) so the media variants carry structured, artifact-linked payloads inline; an unrecognized block decodes to [`InputBlock::Unknown`] for forward compatibility. The discriminant is `block` (not `kind`) precisely because inner payloads such as [`SymbolRef`]/[`GitHubReference`] carry their own `kind` field.
 */
type InputBlock =
  | {
      block: "text";
      text: string;
    }
  | {
      block: "audio";
      duration_ms?: number | null;
      /**
       * The preserved original audio blob (kept where policy allows).
       */
      original: ArtifactRef;
      sample_rate_hz?: number | null;
      /**
       * The transcript, once produced and (for submission) reviewed.
       */
      transcript?: Transcript | null;
    }
  | {
      block: "image";
      /**
       * (2) Extracted text (OCR), when produced. A separate artifact, not a substitute for the image.
       */
      extracted_text?: ArtifactRef | null;
      height?: number | null;
      /**
       * (3) Model observations about the image.
       */
      observations?: ModelObservation[];
      /**
       * (1) The original image — never replaced by a summary.
       */
      original: ArtifactRef;
      /**
       * (4) Crop / coordinate references into the image.
       */
      regions?: ImageRegion[];
      width?: number | null;
    }
  | {
      block: "file";
      byte_length: number;
      id: string;
      /**
       * IANA media type, e.g. `text/plain` or `application/json`.
       */
      media_type: string;
      sensitivity: DataClassification;
      /**
       * Lowercase hex SHA-256 of the blob's bytes (the content address).
       */
      sha256: string;
    }
  | {
      block: "editor-selection";
      path: string;
      range: Range;
    }
  | {
      block: "code-symbol";
      /**
       * The symbol kind (`function`, `struct`, …), when known.
       */
      kind?: string | null;
      line?: number | null;
      path: string;
      /**
       * The symbol name (e.g. `WorkflowDriver::advance`).
       */
      symbol: string;
    }
  | {
      block: "github-reference";
      kind: GitHubRefKind;
      /**
       * The PR/issue number, or `None` for a commit/repo reference.
       */
      number?: number | null;
      owner: string;
      repo: string;
      url?: string | null;
    }
  | {
      block: "unknown";
    };
/**
 * Where a transcription (or any media interpretation) runs.
 */
type TranscriptionMode =
  | {
      type: "local";
    }
  | {
      type: "remote";
    }
  | {
      type: "unknown";
    };
/**
 * The kind of GitHub entity a [`GitHubReference`] points to.
 */
type GitHubRefKind =
  | {
      type: "pull-request";
    }
  | {
      type: "issue";
    }
  | {
      type: "commit";
    }
  | {
      type: "comment";
    }
  | {
      type: "unknown";
    };
/**
 * The scope hierarchy an input applies at (README: `System → Organisation → User → Workspace → Repository → Branch → Session → Task`).
 */
type ScopeLevel =
  | {
      type: "system";
    }
  | {
      type: "organization";
    }
  | {
      type: "user";
    }
  | {
      type: "workspace";
    }
  | {
      type: "repository";
    }
  | {
      type: "branch";
    }
  | {
      type: "session";
    }
  | {
      type: "task";
    }
  | {
      type: "unknown";
    };
/**
 * Where an input originated.
 */
type InputSource =
  | {
      type: "tui";
    }
  | {
      type: "ide";
    }
  | {
      type: "cli";
    }
  | {
      type: "web";
    }
  | {
      type: "voice";
    }
  | {
      type: "unknown";
    };
/**
 * A mode preset: a bundle of policy and interaction defaults, not merely a prompt (Chapter 20). Modes are enforced by the policy engine — an `Explore` run proposing a write is denied regardless of what the model says.
 */
type AgentMode =
  | {
      type: "Ask";
    }
  | {
      type: "Explore";
    }
  | {
      type: "Plan";
    }
  | {
      type: "Build";
    }
  | {
      type: "Review";
    }
  | {
      type: "Unknown";
    };
/**
 * The decision an approver returns for a proposed action.
 */
type ApprovalDecision =
  | {
      type: "Approve";
    }
  | {
      type: "Reject";
    }
  | {
      type: "Unknown";
    };
/**
 * How widely an approval applies (Chapter 04 / STEP 1.6).
 */
type ApprovalScope =
  | {
      type: "Once";
    }
  | {
      type: "Run";
    }
  | {
      type: "Pattern";
    }
  | {
      type: "Repository";
    }
  | {
      type: "Unknown";
    };
/**
 * How a question was resolved.
 */
type QuestionOutcome =
  | {
      answers: string[][];
      type: "Answered";
    }
  | {
      feedback?: string | null;
      type: "Rejected";
    }
  | {
      type: "Unknown";
    };
/**
 * A semantic mutation on a collaborative document. Internally tagged with an [`DocumentMutation::Unknown`] fallback so a newer client's mutation deserializes and is rejected structurally rather than crashing the peer.
 */
type DocumentMutation =
  | {
      block_id: string;
      content: JsonValue;
      index: number;
      op: "insert";
    }
  | {
      block_id: string;
      op: "delete";
    }
  | {
      block_id: string;
      delete_len?: number;
      insert?: string;
      op: "edit_text";
      position: number;
    }
  | {
      op: "annotate";
      suggestion: SuggestionInput;
    }
  | {
      op: "accept_suggestion";
      suggestion_id: string;
    }
  | {
      op: "reject_suggestion";
      suggestion_id: string;
    }
  | {
      op: "unknown";
    };
/**
 * Where a document publish writes (Phase 4 STEP 4.4). Wire mirror of `codypendent-knowledge`'s `PublishTarget` domain type — the protocol crate cannot name the knowledge crate, so this carries the same three targets across the wire and the `codypendentd` assembly converts one into the other before computing the plan. Internally tagged with a [`PublishTarget::Unknown`] fallback so a target a newer peer added deserializes and is rejected structurally rather than crashing the peer.
 */
type PublishTarget =
  | {
      kind: "repository_file";
      path: string;
    }
  | {
      branch: string;
      kind: "docs_branch_commit";
      path: string;
    }
  | {
      branch: string;
      kind: "documentation_pr";
      path: string;
      title: string;
    }
  | {
      kind: "unknown";
    };
/**
 * One legal state-machine transition to attempt via `AdvancePromotion` (Phase 7 STEP 7.5). Mirrors `codypendent_eval::promote::Candidate`'s methods exactly. Regression and canary verdicts are computed from durable evidence by the daemon, never supplied as client booleans.
 */
export type PromotionAction =
  | {
      type: "RunRegression";
    }
  | {
      type: "ReviewPermissions";
    }
  | {
      type: "StartShadow";
    }
  | {
      type: "StartCanary";
    }
  | {
      metrics: CanaryMetrics;
      type: "ObserveCanary";
    }
  | {
      type: "FinishCanary";
    }
  | {
      type: "Unknown";
    };
/**
 * Which durable board a client-side blackboard write targets: a workflow run's board, or a repository's task board (the kanban surface). A wire enum with an [`Unknown`](BlackboardScope::Unknown) fallback so a scope from a newer client is rejected structurally rather than failing the frame.
 */
type BlackboardScope =
  | {
      type: "WorkflowRun";
      workflow_run_id: string;
    }
  | {
      repository: string;
      type: "RepositoryBoard";
    }
  | {
      type: "Unknown";
    };
/**
 * Which of the caller's *visible* scopes a bulk forget targets. Deliberately not a scope key: see the module doc.
 */
type MemoryScopeTier =
  | {
      type: "System";
    }
  | {
      type: "User";
    }
  | {
      type: "Repository";
    }
  | {
      type: "Unknown";
    };
/**
 * How a pending prompt is delivered (Adoption 06, cline's `PendingPromptDelivery`). `Steer` feeds the live run's steering channel at its next safe point; `Queue` waits for the session to go idle and launches a continuation run.
 */
type PromptDelivery =
  | {
      type: "Queue";
    }
  | {
      type: "Steer";
    }
  | {
      type: "Unknown";
    };

/**
 * An idempotent, optionally revision-guarded request.
 */
export interface Command {
  body: CommandBody;
  command_id: string;
  /**
   * Optimistic-concurrency guard: apply only if the session is still at this revision.
   */
  expected_revision?: number | null;
  /**
   * Client-chosen key; the same key must never apply twice.
   */
  idempotency_key: string;
}
/**
 * A normalized unit of user input: where it came from, the typed blocks it carries, the scope it applies at, and any bulk attachments.
 */
interface InputEnvelope {
  /**
   * Bulk artifacts referenced by the blocks (or attached alongside them).
   */
  attachments?: ArtifactRef[];
  blocks: InputBlock[];
  scope: ScopeLevel;
  source: InputSource;
}
/**
 * A pointer to a stored artifact plus the metadata needed to handle it safely.
 *
 * `id` and `sha256` are deliberately independent: identical bytes dedup to one blob (keyed by `sha256`) but every occurrence is its own `ArtifactRef` with its own id and `sensitivity` (Chapter 14 / STEP 1.4). Classification checks always read the ref in hand, never a row looked up by hash.
 */
interface ArtifactRef {
  byte_length: number;
  id: string;
  /**
   * IANA media type, e.g. `text/plain` or `application/json`.
   */
  media_type: string;
  sensitivity: DataClassification;
  /**
   * Lowercase hex SHA-256 of the blob's bytes (the content address).
   */
  sha256: string;
}
/**
 * A transcript of an [`AudioArtifact`], linked back to its source audio.
 */
interface Transcript {
  /**
   * Where the transcription ran (local vs. off-device).
   */
  mode: TranscriptionMode;
  /**
   * The transcription model, if a hosted/known one produced it.
   */
  model?: string | null;
  /**
   * Whether the user reviewed/edited the transcript before submission (Chapter 10: "transcript review before submission").
   */
  reviewed?: boolean;
  /**
   * The audio artifact this transcript was produced from — the link that keeps the original reachable from the interpretation.
   */
  source_audio: string;
  text: string;
}
/**
 * A model's textual observation about an image.
 */
interface ModelObservation {
  model?: string | null;
  text: string;
}
/**
 * A rectangular region of an image (a crop or a coordinate reference).
 */
interface ImageRegion {
  height: number;
  label?: string | null;
  width: number;
  x: number;
  y: number;
}
/**
 * A half-open range within a single document.
 */
interface Range {
  end: Position;
  start: Position;
}
/**
 * A zero-based position in a text document.
 */
interface Position {
  character: number;
  line: number;
}
/**
 * A debounced snapshot of the IDE's context, pushed client→daemon. Clients debounce these (≥ 300 ms) so a burst of keystrokes collapses to one update.
 */
interface IdeContextUpdate {
  /**
   * The file the user is focused on, if any.
   */
  active_file?: string | null;
  /**
   * A monotonically increasing revision for the diagnostics set, so the daemon can tell whether it holds the latest without transferring them.
   */
  diagnostics_revision?: number;
  /**
   * Digests of every unsaved buffer (contents are never sent unsolicited).
   */
  dirty_buffers?: DirtyBufferDigest[];
  /**
   * Paths of all open documents.
   */
  open_files?: string[];
  /**
   * The current selection, if any.
   */
  selection?: EditorSelection | null;
}
/**
 * A content digest for one unsaved ("dirty") editor buffer. The filesystem is not always the user's current truth; the IDE sends digests so the daemon can detect divergence and request the full contents only when required and authorized (Chapter 10, "Unsaved buffers").
 */
interface DirtyBufferDigest {
  byte_length: number;
  path: string;
  /**
   * Lowercase hex SHA-256 of the buffer's current bytes.
   */
  sha256: string;
}
/**
 * The editor's current selection: a range within one file.
 */
interface EditorSelection {
  path: string;
  range: Range;
}
/**
 * A proposed replacement over `[range_start, range_end)` of a block's text.
 */
interface SuggestionInput {
  block_id: string;
  range_end: number;
  range_start: number;
  rationale?: string | null;
  replacement: string;
}
/**
 * A request to lease a block range for exclusive writing (Chapter 03 / STEP 4.3). One writer per block-range; readers are unlimited. Reuses the Phase-1 lease machinery in the daemon; this is only the wire request shape.
 */
interface DocumentEditLease {
  /**
   * The block the writer intends to edit; `None` leases the whole document structure (block insert/delete/reorder).
   */
  block_id?: string | null;
  document_id: string;
}
/**
 * Objective canary metrics compared by the daemon. Rates are basis points (0..=10,000); latency is milliseconds.
 */
export interface CanaryMetrics {
  baseline_error_rate_bps: number;
  baseline_p95_latency_ms: number;
  error_rate_bps: number;
  p95_latency_ms: number;
  sample_count: number;
}
/**
 * A client-authored blackboard artifact before it is stored (Phase B kanban — the write half `PostBlackboardItem` carries). The author is **not** here: the daemon builds it server-side from the issuing connection, exactly as the workflow executor builds an agent's author — a client never supplies its own attribution.
 */
interface BlackboardItemDraft {
  /**
   * Who the card is assigned to, if anyone.
   */
  assignee?: string | null;
  /**
   * The author's confidence in `[0, 1]`, if given.
   */
  confidence?: number | null;
  /**
   * Evidence references grounding the artifact. Claim-like kinds require at least one (the store enforces it); a `task` needs none.
   */
  evidence?: JsonValue[];
  /**
   * The typed artifact kind (`task` for board cards; any store kind is legal).
   */
  kind: string;
  /**
   * The card's position within its column (lower sorts first). Appended to the end of the column when absent.
   */
  ordinal?: number | null;
  payload: JsonValue;
  /**
   * The board column (`todo` / `doing` / `review` / `done`, or a validated free string). Defaults server-side to `todo` for a `task`.
   */
  status?: string | null;
}
/**
 * The filter `graph show` applies. Every field narrows; absent fields do not.
 *
 * **Scoping is not one of these fields.** The repository is carried by the command, resolved by the daemon from a filesystem path with its own single source of truth, and applied to every branch of this query — including [`node_id`](Self::node_id). There is no way to spell "some other checkout" here.
 */
interface CodeGraphQuery {
  /**
   * Include the edges incident to the selected nodes — **every** node the filter selects, not only the ones this page of nodes happens to show. The two row kinds are paged independently (see [`limit`](Self::limit)), so an edge between two nodes that both fall past the node page is still returned and still counted.
   */
  include_edges?: boolean;
  /**
   * Include the nodes themselves. Both false is treated as both true — a query that selects nothing is a client bug, not a legal request. Both flags default to `false`, so this is exactly what the default `{}` query asks for: everything.
   */
  include_nodes?: boolean;
  /**
   * Exact stored node-kind scalar (`function`, `type`, …).
   */
  kind?: string | null;
  /**
   * Exact stored language scalar (`rust`, `python`, …).
   */
  language?: string | null;
  /**
   * Maximum rows of each kind — nodes and edges get a page each, not one shared budget. 0 asks for the server default; the server clamps any request to its own ceiling.
   */
  limit?: number;
  /**
   * Case-insensitive substring of the qualified name.
   */
  name?: string | null;
  /**
   * Exactly one node, by its stored id.
   *
   * This is the direct-by-id path, and it carries the **same** repository gate the list path does. A node id belonging to another checkout is answered identically to an id that does not exist anywhere (`graph.node-not-found`), so naming an id can never confirm that it exists elsewhere. A filter that is enforced only where a list is built is not a filter; it is an enumeration oracle with extra steps.
   */
  node_id?: string | null;
  /**
   * Repo-relative path prefix (`crates/cli/`), matched against `code_nodes.source_path`.
   */
  path?: string | null;
}

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
