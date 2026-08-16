# Hybrid Codypendent platform program — design

**Date:** 2026-08-16  
**Status:** approved architecture; implementation contract  
**Audience:** Codypendent maintainers and implementation agents  
**Delivery model:** one coordinated program, implemented as dependency-ordered vertical milestones  
**Companion plan:** `docs/superpowers/plans/2026-08-16-hybrid-platform-program.md` *(written after this specification is reviewed)*

## 1. Goal

Turn Codypendent's mature local agent runtime into a complete local-first product and an optional
hybrid team platform without weakening its existing security model.

The program delivers:

1. a stable and honestly documented local product;
2. real desktop and IDE clients;
3. searchable, manageable session history;
4. durable notifications and approvals;
5. scheduled and event-driven automation;
6. measured cost, routing, and quality analytics;
7. a governed signed marketplace and secret broker;
8. access-safe cross-repository architecture intelligence;
9. a hybrid managed/self-hosted team control plane;
10. self-hosted and managed ephemeral execution pools.

Personal local operation remains fully functional without an account, network connection, control
plane, or remote runner.

## 2. Locked decisions

| Topic | Decision |
|---|---|
| Program scope | Prerequisite remediation, all seven reviewed feature areas, remote team control plane, and remote execution pools |
| Delivery | Vertical capability milestones, not platform-first or big-bang subsystem integration |
| Deployment | Hybrid: managed control plane plus self-hosted control-plane option and self-hosted runners |
| Human identity | GitHub login for individuals plus generic OIDC for organizations |
| Workload identity | Short-lived daemon and runner identities |
| Data authority | Split authority between local daemon and control plane |
| Default sharing | Organization-configurable with a metadata-only default |
| Runner architecture | One deployment-neutral protocol; self-hosted containers first, managed microVMs later |
| Compatibility | Local-only operation and existing local clients remain supported |
| Completion standard | A feature is complete only when a shipped client or production execution path drives it end to end |

## 3. Non-goals

- Replacing the local daemon with a hosted execution service.
- Requiring cloud connectivity for personal use.
- Automatically uploading source, raw transcripts, artifacts, memories, or secrets.
- Giving the control plane authority to broaden local security policy.
- Maintaining separate managed and self-hosted product implementations.
- Building a second agent runtime for remote runners.
- Treating marketplace signatures as sufficient authorization to run a package.
- Supporting every identity provider directly; organization identity enters through OIDC.
- Making Windows runners a gate for the initial runner protocol. The protocol must permit them,
  but Linux container and microVM execution validates the first implementation.
- Completing the program as one unreviewable commit. "One program" means one architecture and
  dependency sequence, not one diff.

## 4. Architecture

```text
┌──────────────────────────────── Clients ────────────────────────────────┐
│ TUI · CLI · VS Code · Desktop · Browser                                │
└───────────────┬──────────────────────────────┬──────────────────────────┘
                │ local protocol               │ authenticated HTTPS/WSS
                ▼                              ▼
┌───────────────────────────┐       ┌─────────────────────────────────────┐
│ Local Codypendent daemon  │◀─────▶│ Hybrid control plane                │
│                           │ sync  │                                     │
│ Source and worktrees      │       │ GitHub + OIDC identity              │
│ Raw transcripts/artifacts │       │ Organizations, teams, RBAC          │
│ Local secrets             │       │ Shared metadata and audit ledger    │
│ Local model execution     │       │ Trigger/schedule service            │
│ Private knowledge         │       │ Runner scheduler                    │
└──────────────┬────────────┘       │ Marketplace registry                │
               │                    │ Cross-repository graph metadata      │
               │ runner protocol    └──────────────┬──────────────────────┘
               ▼                                   │
┌───────────────────────────┐       ┌──────────────▼──────────────────────┐
│ Self-hosted runners       │       │ Managed ephemeral microVM runners   │
│ Docker / Kubernetes       │       │ Same runner protocol                │
└───────────────────────────┘       └─────────────────────────────────────┘
```

### 4.1 Authority boundaries

The **local daemon** is authoritative for:

- repository contents and worktrees;
- local credentials and secret references;
- raw private transcripts and unpublished artifacts;
- local model context and local knowledge;
- local effects, approvals, and deny-first policy enforcement.

The **control plane** is authoritative for:

- identity, organization membership, teams, and repository grants;
- shared workflow and session metadata;
- schedules, trigger deduplication, and runner scheduling;
- marketplace metadata, publisher trust, and revocation;
- shared cross-repository graph facts;
- immutable organization audit records.

The **organization publication policy** decides which locally owned content may be shared. A cloud
grant can never broaden the local daemon's security decision. The effective authority is the
intersection of remote RBAC, organization policy, repository policy, and local policy.

### 4.2 Component boundaries

- Extend `crates/protocol` for local session/query/inbox/usage contracts.
- Add `crates/control-plane-protocol` for network and team contracts that must not pollute the
  local socket protocol.
- Add `crates/control-plane` as the self-hostable service implementation.
- Add `crates/runner` for self-hosted execution.
- Reuse `crates/sandbox`, `crates/workflow`, `crates/routing`, `crates/eval`, and the existing
  runtime rather than wrapping or duplicating them.
- Extract shared React transport, projection, and semantic Remote UI rendering for desktop and
  browser clients.
- Keep managed infrastructure providers outside the execution core behind narrow traits.

## 5. Identity and authorization

### 5.1 Human identity

- GitHub OAuth is the default individual and small-team sign-in.
- Generic OIDC is the organization identity boundary. An organization that needs SAML connects it
  through an OIDC-capable identity bridge.
- One human may link multiple identities. Linking requires proof of both identities and emits an
  audit record.

### 5.2 Workload identity

- Daemons and runners use short-lived, audience-bound credentials.
- Pairing a daemon requires human confirmation and displays organization, repository, requested
  synchronization classes, approval-delivery authority, runner-dispatch authority, expiry, and
  revocation controls.
- Runner credentials are job-scoped and cannot claim unrelated jobs or repositories.

### 5.3 RBAC

The initial roles are:

- **Observer:** read permitted shared metadata and content.
- **Contributor:** start permitted sessions and workflows.
- **Approver:** resolve actions within an explicit repository/action scope.
- **Maintainer:** manage repository automation, integrations, and publication policy.
- **Organization administrator:** manage organization identity, policy, runners, and marketplace
  allowlists.

Roles are grants on organization/team/repository resources. Every query and mutation checks the
target repository before revealing whether the target exists. Local policy may narrow every role.

## 6. Data model and publication

### 6.1 Control-plane PostgreSQL

The control-plane relational store owns:

- users, linked identities, organizations, teams, and memberships;
- repository registrations and role grants;
- connected daemons and runners;
- shared session/run summaries;
- approval and notification metadata;
- schedules, triggers, bindings, and deduplication receipts;
- runner jobs, attempts, leases, heartbeats, and attestations;
- publishers, packages, versions, compatibility, and revocations;
- cross-repository identities and graph edges;
- immutable audit records;
- usage aggregates and quality measurements.

### 6.2 Object storage

S3-compatible object storage contains only:

- explicitly published artifacts and support bundles;
- signed marketplace packages;
- optional encrypted transcript or patch exports;
- content-addressed runner input and output bundles.

### 6.3 Local SQLite

Local SQLite remains authoritative for private sessions, event history, local artifacts, memories,
credential references, and unpublished graph data. It gains synchronization receipts, remote object
mappings, deletion tombstones, and publication provenance. Reusable control-plane bearer tokens are
not stored in plaintext.

### 6.4 Publication classes

Every shareable object has one class:

1. `private-local`
2. `metadata-shared`
3. `content-shared`
4. `organization-knowledge`
5. `public-marketplace`

The default is `private-local`, except low-risk operational metadata explicitly allowed by
organization policy. Publication records actor, source, policy decision, content hash, encryption
state, retention class, and remote receipt. Search indexes and graph edges inherit the strictest
classification of their sources.

Deletion emits a durable tombstone. Reconnecting daemons consume tombstones before publishing new
deltas, preventing deleted data from being resurrected.

## 7. Protocol contracts

### 7.1 Local client protocol additions

- session search, rename, pin, archive, restore, and retention-aware deletion;
- paginated session history and bounded artifact retrieval;
- durable notification inbox operations;
- usage and quality aggregate queries;
- editor-context actions;
- trigger and schedule management;
- support-bundle export and import.

### 7.2 Control-plane API

- REST resources for administration and bounded queries;
- WebSocket or SSE streams for notifications, run status, approvals, synchronization, and runner
  events;
- explicit protocol version negotiation;
- idempotency keys on every mutation;
- stable opaque pagination cursors;
- generated clients from authoritative schemas.

### 7.3 Daemon synchronization

- The daemon initiates an outbound authenticated connection; the control plane does not require an
  inbound workstation port.
- Synchronization carries metadata deltas, publication receipts, tombstones, remote approvals,
  schedule notifications, and runner-job events.
- Delivery is at least once. Durable idempotency receipts prevent duplicate effects.
- A disconnected daemon continues local work. Shared operations remain visibly queued or fail with
  a specific connectivity error rather than silently changing scope.

### 7.4 Runner protocol

The runner contract includes:

- capability advertisement;
- job eligibility labels;
- claim, lease renewal, release, and cancellation;
- content-addressed input manifests;
- sandbox and resource specification;
- bounded live logs and content-addressed complete outputs;
- output artifact manifests;
- signed execution attestations.

The contract is deployment-neutral. Docker, Kubernetes, managed microVM, macOS provider, and future
Windows implementations share it.

### 7.5 Compatibility

- New local protocol variants and fields are additive.
- Existing local clients continue working when team features are disabled.
- The control plane supports at least one prior released client protocol version.
- Migrations are immutable and checksummed.
- TypeScript contracts are generated from Rust schemas and golden vectors rather than manually
  mirrored.

## 8. User-facing capabilities

### 8.1 Session Library and global search

The daemon owns a ranked, paginated search service over:

- session titles and transcript text;
- tool observations;
- patches and artifact metadata;
- changed paths and symbols;
- workflow, model, repository, date, and run status.

Every result includes source, scope, stable identity, and a deep-link target. Session lifecycle adds
rename, pin, archive, restore, export, and retention-aware deletion. Council and internal worker
sessions may be automatically archived after their parent run finishes without deleting attribution.

### 8.2 Real desktop, browser, and IDE clients

The desktop application must replace simulated state with:

- daemon discovery and authenticated connection;
- reconnect and paginated catch-up;
- live run and transcript projections;
- approval, cancellation, and question handling;
- bounded artifact retrieval;
- shared semantic Remote UI rendering.

VS Code adds real patch retrieval into `vscode.diff` and editor-native actions:

- Fix selection
- Explain selection
- Review current file
- Generate tests for selection
- Fix diagnostic

Each action creates an ordinary attributable Codypendent run using current editor context. There is
no extension-only agent loop.

The browser client exposes shared sessions, inbox, approvals, analytics, marketplace, automation,
and explicitly published artifacts through the control-plane API. It has no implicit access to local
repository content.

### 8.3 Durable notification and approval inbox

The inbox aggregates approval requests, agent questions, run completion/failure, budget warnings,
workflow blocks, plugin permission changes, and runner failures. Entries are durable, deduplicated,
acknowledgeable, repository-authorized, and deep-linked. Desktop and VS Code may produce native
notifications. Email and chat delivery are optional policy-controlled adapters.

### 8.4 Scheduled and event-driven automation

The generic trigger service supports:

- cron and one-time schedules;
- GitHub and generic signed webhooks;
- CI failures;
- repository and code-graph changes;
- dependency alerts;
- manual and API events.

A trigger binding defines filters, deduplication identity, concurrency policy, retry policy,
missed-run behavior, budget ceiling, target workflow, repository, and approval mode.

Initial first-party templates are failing-CI repair, dependency update, stale-document refresh,
flaky-test investigation, repository health report, and release preparation.

### 8.5 Usage, routing, and quality center

Aggregate measured input/output/cached/reasoning tokens, cost, latency, task class, route, retry,
escalation, completion, grader score, and cost per successful task. Views group by model, provider,
repository, workflow, task class, and time period. Unknown measurements remain absent rather than
zero. Users and organizations may configure budgets and alerts. Exports support JSON and CSV.

### 8.6 Signed marketplace and integration pack

Marketplace operations include discovery, inspection, installation, pinning, update, disabling,
removal, publisher trust, revocation, compatibility, and permission-diff review. Installation never
enables executable code automatically. Organization allowlists may prohibit unverified publishers.

Initial first-party integrations cover GitHub, GitLab, Linear/Jira, Slack/Teams, generic webhooks,
generic OpenAI-compatible model providers, MCP packages, and ACP packages.

### 8.7 Cross-repository architecture intelligence

The shared graph models repositories, packages, symbols, references, APIs, schemas, migrations,
deployments, ownership, tests, and CI workflows. Local daemons publish only policy-approved facts.
Queries enforce grants before traversal and do not infer inaccessible nodes through counts or error
differences.

Initial capabilities are cross-repository blast radius, coordinated API migration plans, dependency
upgrade campaigns, ownership-aware review assignment, and multi-repository execution with separate
repository approvals.

### 8.8 Session and support bundles

A versioned redacted bundle can include selected transcript events, routing metadata, approvals,
artifact manifests, patches, and environment diagnostics. Export requires an explicit inclusion
policy. Import verifies hashes, handles identity collisions, labels imported provenance, and never
restores credentials.

## 9. Secret broker

Jobs and integrations receive named secret references, never embedded values. The broker issues
short-lived credentials based on principal, organization, repository, job, requested capability,
and policy.

Initial backends are:

- local OS keychain;
- environment references for compatibility;
- managed encrypted secret storage;
- HashiCorp Vault-compatible storage;
- cloud workload identity where available.

Every lease, use, denial, rotation, and revocation is audited without recording values. A runner
cannot ask for a broader secret after accepting a job; the job must be rejected and resubmitted with
a newly reviewed specification.

## 10. Control-plane deployment

Managed and self-hosted modes run the same service and schema.

**Managed mode** provides hosted PostgreSQL, object storage, identity configuration, scheduler,
marketplace, audit ledger, and runner coordination.

**Self-hosted mode** provides versioned container images, database migrations, health checks,
backup/restore documentation, and deployment manifests for PostgreSQL-compatible storage,
S3-compatible object storage, and standards-compliant OIDC.

Cloud-specific infrastructure is implemented through deployment adapters, not branches in domain
logic.

## 11. Remote execution

### 11.1 Lifecycle

1. A workflow submits an idempotent job specification.
2. Policy, capability, data-residency, budget, and region filters identify eligible runners.
3. A runner claims a time-bounded lease and receives minimum-scope credentials.
4. The runner materializes the content-addressed input in an isolated sandbox.
5. It executes the existing Codypendent runtime or workflow node.
6. It streams bounded logs and uploads complete outputs by hash.
7. It returns a signed attestation.
8. The control plane validates hashes, attestation, lease ownership, and terminal compare-and-set.
9. Accepted outputs continue the workflow; suspicious outputs are quarantined.

### 11.2 Runner properties

Runners advertise OS, architecture, tools, sandbox backend, capacity, and policy labels. They use
one-job credentials and cannot approve their own actions, broaden capabilities, or access unrelated
storage. Job environments are disposable.

### 11.3 Implementation progression

1. Linux container runner.
2. Kubernetes runner controller.
3. Managed Linux microVM pool.
4. macOS runner provider adapter.
5. Warm pools and capability-aware autoscaling.

### 11.4 Failure behavior

- Duplicate submission returns the existing job receipt.
- A missing runner expires its lease; retry follows workflow policy.
- Cancellation racing completion uses a persisted terminal compare-and-set.
- Partial artifact upload leaves the job in `uploading`; missing hashes are retried.
- A disconnected runner may finish within its lease but cannot claim another job.
- A disconnected daemon does not stop remote work; results await synchronization.
- Policy revocation may narrow or terminate an active job, never broaden it.
- Attestation or hash mismatch quarantines outputs and blocks automatic continuation.
- Exactly one attempt's output set may become accepted for a logical job.

## 12. Security, privacy, and reliability

### 12.1 Threat boundaries

Browser/IDE clients, plugins/hooks, API callers, daemons, runners, marketplace publishers,
repository contents, model output, and webhook payloads are independently untrusted. Authentication
does not imply repository, artifact, runner, or approval authority.

### 12.2 Required controls

- short-lived audience-bound credentials;
- repository authorization on queries and mutations;
- local deny-first policy on local effects;
- mTLS or equivalent workload authentication;
- envelope encryption for published sensitive artifacts;
- content hashes and signatures for runner and marketplace packages;
- approval decisions bound to exact action digests and expiry;
- defenses against SSRF, archive traversal, symlink escape, command injection, and oversized
  payloads;
- append-only audit records without credential values;
- revocable runner images and publisher keys.

### 12.3 Organization policy

An organization may configure publication classes, data residency, retention, provider/model
restrictions, runner regions, integration allowlists, export, and deletion. Metadata-only remains the
default.

### 12.4 Reliability objectives

- No acknowledged approval, question response, cancellation, or publication is lost.
- At-least-once delivery never duplicates an external effect.
- Local operation remains available during control-plane outages.
- Lease loss cannot produce two accepted output sets.
- Unauthorized resources are indistinguishable from absent resources.
- Published artifact hashes verify end to end.
- One prior released protocol version remains compatible.

## 13. Observability and continuous quality

OpenTelemetry-compatible traces, metrics, and structured logs correlate client, daemon,
control-plane, workflow, runner, and model activity. They measure synchronization lag, queue depth,
lease health, scheduling latency, model latency/retries/routing/quality, notification delivery, and
marketplace sandbox outcomes. Telemetry obeys the same publication policy as product data.

After real execution data exists, the evaluation loop adds:

- policy-controlled trace-to-evaluation capture;
- sampled replay;
- task-class quality and cost SLOs;
- drift detection;
- statistically controlled shadow and canary routing;
- human-gated promotion of prompts, skills, policies, and routes.

## 14. Delivery milestones

### Milestone 0 — stabilize the current tree

- Complete or remove simulated desktop behavior.
- Implement actual VS Code patch retrieval.
- Reconcile staged and unstaged v0.9 changes.
- Restore the truncated root megaplan.
- Verify every tracked remediation finding against current code.
- Run every Rust, SDK, extension, desktop, documentation, dependency, and security gate.

### Milestone 1 — shared contracts and generated SDK

- Generate protocol schemas and TypeScript SDK.
- Add versioned artifact retrieval.
- Add session lifecycle, search/history, inbox, usage, and editor-action contracts.
- Enforce migration immutability.

### Milestone 2 — Session Library and real clients

- Build the search index and query service.
- Add session lifecycle and internal-session archival.
- Finish desktop daemon transport and Remote UI rendering.
- Complete VS Code history, patch review, and editor actions.

### Milestone 3 — inbox and analytics

- Build the durable inbox and native notifications.
- Add usage, quality, and model-value aggregation and exports.

### Milestone 4 — automation

- Finish GitHub webhook dispatch.
- Add generic triggers, schedules, policies, and templates.

### Milestone 5 — marketplace and secrets

- Add secret references and short-lived leasing.
- Add marketplace distribution, trust, updates, and the integration pack.

### Milestone 6 — cross-repository intelligence

- Add shared identities, access-safe graph publication and traversal, migration planning, and
  coordinated multi-repository workflows.

### Milestone 7 — hybrid control plane

- Add the service, PostgreSQL/object storage, GitHub/OIDC identity, RBAC, audit ledger, daemon
  pairing/synchronization, browser client, and self-hosted deployment.

### Milestone 8 — self-hosted remote runners

- Add the runner protocol, container runner, Kubernetes controller, scheduler, leases, artifacts,
  attestations, and workflow-node dispatch.

### Milestone 9 — managed execution and continuous quality

- Add managed Linux microVMs, warm pools, optional macOS adapter, real shadow/canary execution,
  trace capture, quality drift, and routing experiments.

## 15. Verification

Each milestone requires:

- unit tests for pure behavior;
- protocol golden vectors and compatibility tests;
- migrations tested from the previous release;
- local-daemon and control-plane integration tests as applicable;
- cross-principal and cross-organization authorization tests;
- reconnect, idempotency, race, and partition tests;
- UI type checking, accessibility, and interaction tests;
- end-to-end tests through real client transports;
- adversarial tests for every new trust boundary;
- the complete existing CI suite without weakening gates.

Mandatory adversarial scenarios include replayed tokens/webhooks/approvals/jobs, compromised runners
requesting broader credentials, artifact tampering, partial uploads, publisher-key revocation,
malicious archives, path traversal, lease/cancellation races, oversized payloads, reconnect after
offline deletion, and network partition at every runner state.

## 16. Program acceptance criteria

The program is complete when all of the following are true:

1. Personal local operation requires no hosted account or service.
2. Desktop, TUI, CLI, and VS Code observe the same durable session and effects.
3. Search finds transcript, tool, patch, artifact, path, and symbol evidence with provenance.
4. Session archive/restore/export works and internal council sessions do not pollute active history.
5. Pending human work is visible in one durable inbox and notifications are deduplicated.
6. Scheduled and event-driven workflows are durable, idempotent, policy-gated, and observable.
7. Usage views contain measured values and preserve unknowns honestly.
8. Marketplace packages are verified, permission-reviewed, sandboxed, revocable, and disabled by
   default.
9. Cross-repository queries cannot reveal inaccessible repositories directly or indirectly.
10. Managed and self-hosted control planes run the same domain implementation and schema.
11. Daemon pairing is outbound, revocable, and metadata-only by default.
12. Self-hosted containers and managed microVMs execute through one runner protocol.
13. Runner lease loss or retries cannot accept two output sets.
14. Published content and runner artifacts verify by hash and provenance.
15. Real execution observations feed human-gated quality and routing experiments.
16. Documentation marks only production-driven behavior as complete.

## 17. Implementation rule

The implementation plan must preserve the milestone order unless it proves a task has no dependency
on earlier contracts and edits disjoint ownership. Every task names its migration, protocol impact,
security boundary, tests, and production caller. An engine with only unit tests does not satisfy a
feature task.
