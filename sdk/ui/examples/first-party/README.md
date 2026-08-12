# Codypendent first-party semantic UI catalogue

The first-party library is a React component layer over Codypendent's semantic
UI primitives. It does not render DOM, own authoritative application state, or
execute commands. Every component is controlled: the caller supplies the
current projection and namespaced action identifiers, and the trusted host
decides whether an emitted intent is valid and authorized.

## Catalogue

| Product area | Components |
| --- | --- |
| Foundation | `Surface`, `SurfaceFrame`, `IntentButton`, `StatusBadge`, `VirtualizedCollection` |
| Application shell | `ApplicationShell`, `ApplicationStatusLine`, `NavigationRail`, `CommandPalette` |
| Conversation | `ConversationTranscript`, `ConversationComposer`, `ModelAgentControls` |
| Execution | `RunProgress`, `ToolCallLifecycle` |
| Trusted decisions | `ApprovalReview`, `CoreDecisionPrompt` |
| Artifacts | `ArtifactBrowser`, `DocumentViewer`, `CodeViewer`, `DiffReview`, `TestResultsViewer`, `MediaViewer`, `StructuredArtifactViewer` |
| Workflows | `WorkflowGraphView`, `WorkflowTimeline`, `WorkflowNodeInspector` |
| Boards | `KanbanBoard` |
| Extensions | `AgentManagement`, `SkillManagement`, `PluginManagement`, `IntegrationManagement` |
| Git | `WorktreeDashboard`, `GitStatusReview`, `CommitComposer`, `CodeReviewPanel` |
| Knowledge and models | `MemoryKnowledgeSearch`, `ModelRoutingView`, `CostQuotaView` |
| Observability | `TraceExplorer`, `LogsExplorer`, `MetricsDashboard` |
| System lifecycle | `OnboardingFlow`, `DoctorReport`, `UpdateCenter`, `RecoveryCenter` |
| Notifications | `NotificationCenter` |

## Surface contract

Every product surface accepts the shared `SurfaceOptions`:

- `density`: `compact`, `comfortable`, or `spacious`.
- `width`: `narrow`, `standard`, `wide`, or `full`.
- `state`: a discriminated `ready`, `loading`, `empty`, `error`, or `streaming`
  state. `Surface.Loading`, `Surface.Empty`, `Surface.Error`, and
  `Surface.Streaming` provide explicit variants.
- `actions`: semantic `SemanticIntent` descriptors. An intent contains an
  action ID, label, optional JSON payload, keyboard shortcut, and presentation
  tone. It is not an authorization or callback.

Use projections for values and host commands for changes. Do not mirror
projection state into local React state. Components deliberately avoid DOM
events, CSS, filesystem access, process access, credentials, and network APIs.

## Approval, permission, secret, and policy safety

`ApprovalReview` and `CoreDecisionPrompt` serialize the following governance
metadata into their semantic domain card:

```json
{
  "governance": "core-only",
  "authority": "intent-only"
}
```

The controlled confirmation must be `confirmed` before the allow/approve
button becomes enabled. Even then, pressing it only emits an intent. The trusted
core must bind the action to the current document revision, authenticate the
actor, re-evaluate policy and scope, and record the final decision. Third-party
components must never mount these contribution points as a substitute for the
trusted core surface.

## Large collections and streaming

Transcript, artifact, workflow, extension, Git, knowledge, diagnostic,
recovery, and notification collections use `VirtualList` or another virtualized
semantic collection. The complete serializable data set is retained in the
collection's `items`; detailed child rows are capped to keep React reconciliation
bounded. Renderers can window the canonical items for the viewport.

For active operations, provide `{ phase: "streaming", ... }`. Streaming surfaces
keep their partial content mounted below an accessible determinate or
indeterminate progress description.

## Boards and card movement

`KanbanBoard` is a Row of virtualized columns holding cards with status,
assignee, and kind. There is deliberately no pointer drag-and-drop: the event
vocabulary carries no `drop`, and a dragged card would be unreachable from the
keyboard. Movement is an `ActionMenu` intent instead — one entry per
destination column, carrying `cardId`, `fromColumnId`, and `toColumnId`, which
the host authorizes like any other command. A column `limit` is advisory: an
over-limit column is called out in text and tone, never blocked. Cards naming a
column the board does not define are collected into an explicit `Unplaced`
column rather than dropped.

## Terminal and accessibility behavior

Rich diff, image, audio, graph, chart, and Markdown-oriented views carry plain
semantic fallbacks. Terminal renderers can select these when a negotiated
capability is missing. Interactive nodes have accessible labels; important
state is expressed in text as well as tone; media includes alt text and audio
transcripts/fallbacks; keyboard shortcuts appear in intent metadata and control
descriptions.

The example in [catalogue.tsx](./catalogue.tsx) demonstrates controlled
composition. Applications should import these components from the package's
first-party or React export once exposed by the package manifest.
