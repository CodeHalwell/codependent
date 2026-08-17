# M4 — Durable scheduled and event-driven automation

**Audience:** the implementer of Milestone 4 of
[`plans/2026-08-16-hybrid-platform-program.md`](../plans/2026-08-16-hybrid-platform-program.md)
(§354–409). Read [`00-conventions-and-traps.md`](./00-conventions-and-traps.md) first.
**Status:** verified against the tree on 2026-08-17 (branch `release/v0.9.0`).

The plan already gives you the task list, the **Files:** lines, the failing-test-first ordering and
the commit messages. This document does not repeat them. It supplies the four things the plan
leaves out: what actually exists today, the column-level data model, the exact contract→handler→
authorization wiring, and the security rules that make automation something other than an approval
bypass.

---

## 1. Status: what exists, what does not

### Already shipped and typed — implement, do not design

| Thing | Where | State |
|---|---|---|
| The whole wire contract | `crates/protocol/src/automation.rs` | Complete. `TriggerSource`, `TriggerFilters`, `DeduplicationPolicy`, `ConcurrencyPolicy`, `TriggerRetryPolicy`, `MissedRunPolicy`, `BudgetCeiling`, `AutomationApprovalMode`, `InvocationPolicy`, `AutomationBindingDraft/Patch/Query/Page`, `AutomationBindingRequest`. **Zero implementers.** |
| The command variant | `crates/protocol/src/command.rs:167-170` — `ManageAutomationBinding { request: AutomationBindingRequest }` | Exists |
| The id type | `crates/protocol/src/ids.rs:76-79` — `AutomationBindingId` | Exists |
| The client capability bit | `crates/protocol/src/capabilities.rs:47-49` — `ClientCapabilities.automation` | Advertised, honoured by nothing |
| Golden vectors | `protocol-vectors/automation.json` | Exists (every `AutomationBindingRequest` and `AutomationApprovalMode` variant) |
| GitHub webhook ingest | `crates/integrations/src/webhook/{ingest,verify,normalize,store,server,config}.rs` | Verifies, dedups, normalizes — then throws the event away |
| The durable workflow engine automation must drive | `crates/workflow/`, `crates/codypendentd/src/workflows.rs`, `crates/codypendentd/src/workflow_exec.rs` | Complete: compile → `create_run_idempotent_owned` → drive, with checkpoints, node retry and budgets |

### Missing

- `migrations/0044_automation.sql` — highest committed migration is `0040_session_library.sql`.
- `crates/daemon/src/automation.rs`, `crates/daemon/tests/automation_it.rs`.
- `crates/codypendentd/src/webhook_dispatch.rs`, `crates/codypendentd/src/automation_scheduler.rs`.
- `crates/integrations/src/webhook/generic.rs`.
- Any `role_permits` arm, `named_resources()` entry, or ownership-gate arm for automation.
- The first-party workflow templates under `docs/specs/workflows/`.

### The three lines that make the feature unreachable today

1. `crates/daemon/src/commands.rs:2982` — `ManageAutomationBinding` is listed in
   `is_reserved_unsupported_command`, so both `apply` (`commands.rs:225`) and `validate`
   (`commands.rs:594`) return `protocol.unsupported-payload` before anything else runs.
2. `crates/protocol/src/command.rs:1019` — `ManageAutomationBinding` is in the
   `=> Vec::new()` arm of `named_resources()`. The ownership gate
   (`crates/daemon/src/server.rs:5143`) therefore passes **vacuously**.
3. `crates/daemon/src/commands.rs:3001-3040` — `role_permits` has no arm, so it falls through
   `_ => false` at line 3038 and every role is denied.

### The webhook dead end, precisely

`crates/integrations/src/webhook/ingest.rs` does the hard part correctly and then stops:

- `ingest()` (line 78) enforces the order that matters — reject empty delivery id/event type
  (line 87), **verify the HMAC over raw bytes before parsing** (lines 95-105), normalize only after
  authentication (line 110), then atomically reserve *two* replay identities in one store
  transaction (lines 118-125): the `X-GitHub-Delivery` GUID and
  `body-sha256:<sha256(signature)>`, which binds the authenticated content so the same signed body
  cannot be re-presented under a forged new delivery id.
- It then returns `IngestOutcome::Accepted { event, trigger: self.allow_triggers }` (line 127),
  where `allow_triggers` is a constructor boolean.
- The only production construction is `crates/codypendentd/src/lib.rs:409`:
  `WebhookIngestor::new(store, secret, false)` — commented *"Deliveries never trigger workflows in
  this phase (default-deny policy)."*
- `crates/integrations/src/webhook/server.rs` consumes `IngestOutcome` only to pick an HTTP status.
  Nothing reads `event`.

**What must change (Task 4.2).** Do not flip the boolean to `true`. Delete the
`allow_triggers: bool` field and `should_trigger()` entirely and replace them with an injected
`Option<Arc<dyn WebhookEventSink>>`, invoked **after** the `reserve_if_new` call at line 119
succeeds and only for `IngestOutcome::Accepted`. The trigger decision then comes from an enabled
server-side binding row, never from a constructor flag and never from the payload. Keep every
existing ordering test in `ingest.rs` (lines 157-304) and `crates/integrations/tests/webhook_it.rs`
green — they pin the invariants, and `trigger_defaults_false` (line 290) is the one that must be
rewritten rather than deleted (its replacement: *"an accepted delivery with no matching enabled
binding starts no workflow"*).

Two structural gaps in the current listener you must close for `TriggerSource::GitHubWebhook`/
`SignedWebhook`, which both carry an `endpoint_id`:

- **No path routing.** `handle_connection` (`webhook/server.rs:83`) parses the request line only to
  check the method is `POST` (line 114) and discards the path. `endpoint_id` has to come from the
  URL path (`POST /webhooks/<endpoint_id>`), so this function must extract and validate it.
- **One global plaintext secret.** `WebhooksConfig.secret` (`webhook/config.rs:22-28`) is a single
  `Option<String>` for the whole listener. Per-endpoint keys are required. Store them as
  *references* (`automation_endpoints.signing_key_ref`, §2) resolved through the M5 broker — see
  §6 for what to do before M5 lands.

---

## 2. Data model — `migrations/0044_automation.sql`

Conventions taken from `migrations/0040_session_library.sql` and `migrations/0027_hooks.sql`: TEXT
UUID primary keys, TEXT ISO-8601 timestamps, INTEGER 0/1 booleans with `CHECK (x IN (0,1))`,
enumerations as TEXT with `CHECK (... IN (...))`, partial indexes for sparse columns, `UNIQUE`
constraints that carry a durable invariant rather than a convenience.

> **Numbering.** `0041`–`0043` are claimed by M2.x/M3.x (`0043_execution_observations.sql` is named
> in plan Task 3.3). Do not renumber to fill a gap and do not take `0044` early — assign centrally
> per release (conventions §4). Migrations are append-only and checksum-gated: after adding the
> file run `python3 .github/scripts/check_migration_immutability.py --update` and commit
> `migrations/checksums.json` in the same commit.

```sql
-- Milestone 4: durable trigger and schedule automation.
--
-- Invocation policy lives HERE, not in a `WorkflowDefinition`: the same workflow
-- is bound to several sources with different operational and approval policies
-- (see crates/protocol/src/automation.rs module docs). A binding row is the sole
-- server-side authority for owner, repository, workflow, budget and approval —
-- no webhook payload or schedule occurrence may select any of them.

CREATE TABLE automation_bindings (
    id TEXT PRIMARY KEY,                       -- AutomationBindingId
    -- Kernel-derived at create time from the connection's peer credentials and
    -- never accepted from the wire, exactly as StartWorkflowRequest.owner_uid is
    -- stamped (crates/daemon/src/workflows.rs:46-49). Every later firing runs as
    -- this uid, not as the daemon uid.
    owner_uid INTEGER NOT NULL,
    name TEXT NOT NULL,
    source_type TEXT NOT NULL CHECK (source_type IN (
        'cron', 'one_time', 'github_webhook', 'signed_webhook', 'ci_failure',
        'repository_change', 'code_graph_change', 'dependency_alert',
        'manual', 'api'
    )),
    -- The serialized TriggerSource. Endpoint/key REFERENCES only; the contract
    -- forbids secret material crossing it, and this column is read back into
    -- clients verbatim.
    source_json TEXT NOT NULL,
    -- Denormalized out of source_json so an inbound delivery is an index probe
    -- rather than a table scan with JSON parsing on the hot ingest path.
    endpoint_id TEXT,
    -- Cron is split out of source_json for the same reason: the scheduler must
    -- compute next occurrences without deserializing every binding. `timezone`
    -- is stored separately from `next_fire_at` because next_fire_at is UTC and
    -- a DST transition can only be recomputed from the original zone.
    cron_expression TEXT,
    cron_timezone TEXT,
    one_time_at TEXT,
    CHECK (source_type <> 'cron'
           OR (cron_expression IS NOT NULL AND cron_timezone IS NOT NULL)),
    CHECK (source_type <> 'one_time' OR one_time_at IS NOT NULL),
    -- Precomputed next occurrence in UTC. The scheduler's atomic claim is an
    -- UPDATE on this column, so it must be a real column and not a derived view.
    next_fire_at TEXT,
    -- The last occurrence timestamp actually persisted, so a restart can decide
    -- skip / run-once / bounded-catch-up from durable state rather than from
    -- "now minus an interval", which double-fires after a clock change.
    last_fire_at TEXT,
    workflow_id TEXT NOT NULL,
    workflow_version TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    -- RepositoryId is a ONE-WAY hash of the canonical checkout path
    -- (crates/codypendentd/src/scan.rs:1130 -> knowledge::stable_repository_id)
    -- and no repositories table exists in this schema. The canonical path is
    -- therefore persisted at create time; the dispatcher re-derives the id from
    -- it and refuses on mismatch (a moved/renamed checkout).
    repository_path TEXT NOT NULL,
    filters_json TEXT NOT NULL DEFAULT '{}',
    invocation_json TEXT NOT NULL DEFAULT '{}',
    -- Hot policy fields are projected out of invocation_json because dispatch
    -- reads them on every delivery and the scheduler queries on them.
    dedup_window_seconds INTEGER NOT NULL DEFAULT 86400
        CHECK (dedup_window_seconds >= 0),
    concurrency TEXT NOT NULL DEFAULT 'allow'
        CHECK (concurrency IN ('allow', 'skip', 'queue', 'replace')),
    missed_run TEXT NOT NULL DEFAULT 'skip'
        CHECK (missed_run IN ('skip', 'run_once', 'catch_up')),
    missed_run_max_occurrences INTEGER
        CHECK (missed_run_max_occurrences IS NULL
               OR (missed_run = 'catch_up' AND missed_run_max_occurrences > 0)),
    retry_max_attempts INTEGER NOT NULL DEFAULT 0 CHECK (retry_max_attempts >= 0),
    retry_initial_delay_seconds INTEGER NOT NULL DEFAULT 30
        CHECK (retry_initial_delay_seconds >= 0),
    retry_backoff_multiplier INTEGER NOT NULL DEFAULT 2
        CHECK (retry_backoff_multiplier >= 1),
    retry_max_delay_seconds INTEGER CHECK (retry_max_delay_seconds IS NULL
                                           OR retry_max_delay_seconds >= 0),
    -- Budget ceilings are NULLABLE and stay NULL when unset. Never write 0 for
    -- "unset": 0 means "no budget at all" and would silently kill every firing
    -- (measurement-honesty rule, conventions §8).
    budget_wall_time_seconds INTEGER CHECK (budget_wall_time_seconds IS NULL
                                            OR budget_wall_time_seconds > 0),
    budget_tool_calls INTEGER CHECK (budget_tool_calls IS NULL OR budget_tool_calls > 0),
    budget_tokens INTEGER CHECK (budget_tokens IS NULL OR budget_tokens > 0),
    budget_cost_micros INTEGER CHECK (budget_cost_micros IS NULL OR budget_cost_micros > 0),
    approval_mode TEXT NOT NULL DEFAULT 'inherit'
        CHECK (approval_mode IN ('inherit', 'always_require', 'policy_driven', 'preapproved')),
    -- Only meaningful for 'preapproved'. It is a REFERENCE to a receipt the
    -- approval store already issued; automation never mints one (see §4).
    approval_receipt TEXT
        CHECK (approval_receipt IS NULL OR approval_mode = 'preapproved'),
    CHECK (approval_mode <> 'preapproved' OR approval_receipt IS NOT NULL),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    -- Names are the operator-facing handle in CLI/desktop, so they must be
    -- unique per owner; a second principal may reuse a name freely.
    UNIQUE (owner_uid, name)
);

-- The owner predicate leads every listing index so the ownership filter is part
-- of the index seek, not a post-filter (the session-library rule,
-- migrations/0040_session_library.sql).
CREATE INDEX idx_automation_bindings_owner
    ON automation_bindings (owner_uid, enabled, updated_at DESC, id);
CREATE INDEX idx_automation_bindings_due
    ON automation_bindings (next_fire_at)
    WHERE enabled = 1 AND next_fire_at IS NOT NULL;
CREATE INDEX idx_automation_bindings_endpoint
    ON automation_bindings (endpoint_id, enabled)
    WHERE endpoint_id IS NOT NULL;
CREATE INDEX idx_automation_bindings_repository
    ON automation_bindings (repository_id, enabled);
CREATE INDEX idx_automation_bindings_workflow
    ON automation_bindings (workflow_id, enabled);

-- One row per signed inbound endpoint. Separate from the binding because
-- several bindings share one endpoint (different filters over the same GitHub
-- app installation) and because the signing key is rotated independently of the
-- binding's policy.
CREATE TABLE automation_endpoints (
    endpoint_id TEXT PRIMARY KEY,          -- the path segment: POST /webhooks/<endpoint_id>
    owner_uid INTEGER NOT NULL,
    scheme TEXT NOT NULL CHECK (scheme IN ('hmac_sha256', 'ed25519')),
    -- An opaque reference (`env:X`, `keyring://…`, `secret://…`) resolved at
    -- verification time by the M5 broker. NEVER key material: this table is read
    -- by ordinary queries, dumped in support bundles, and shown in CLI output.
    signing_key_ref TEXT NOT NULL,
    -- Per-endpoint body ceiling, at or below the listener's MAX_BODY_BYTES
    -- (crates/integrations/src/webhook/server.rs:80). A public endpoint gets a
    -- tighter one than a loopback one.
    body_limit_bytes INTEGER NOT NULL DEFAULT 1048576
        CHECK (body_limit_bytes > 0 AND body_limit_bytes <= 8388608),
    -- Generic signed webhooks must carry a signed timestamp; a delivery older
    -- than this is refused before the dedup store is touched, so a captured
    -- delivery cannot be replayed after the dedup rows are pruned.
    replay_window_seconds INTEGER NOT NULL DEFAULT 300
        CHECK (replay_window_seconds > 0),
    -- Rotation without deletion: history must stay resolvable for audit.
    created_at TEXT NOT NULL,
    rotated_at TEXT,
    disabled_at TEXT
);

CREATE INDEX idx_automation_endpoints_owner
    ON automation_endpoints (owner_uid, disabled_at);

-- The atomic delivery reservation. This table — not the workflow store and not
-- webhook_deliveries — is the at-least-once/never-twice authority for a
-- binding's firings.
CREATE TABLE automation_receipts (
    id TEXT PRIMARY KEY,
    binding_id TEXT NOT NULL REFERENCES automation_bindings(id) ON DELETE CASCADE,
    -- Derived server-side from the binding id plus the normalized event fields
    -- named by DeduplicationPolicy.identity_fields (or the occurrence instant
    -- for a schedule). Never supplied by the payload.
    dedup_key TEXT NOT NULL,
    delivery_id TEXT,           -- X-GitHub-Delivery GUID where the source has one
    content_fingerprint TEXT,   -- sha256 over the signature, as ingest.rs:118 computes
    occurrence_at TEXT,         -- the scheduled instant, for cron/one_time
    reserved_at TEXT NOT NULL,
    -- reserved_at + dedup_window_seconds. A retention sweep may delete rows past
    -- this, which is why the *replay window* is enforced independently at
    -- verification (see automation_endpoints.replay_window_seconds).
    expires_at TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN
        ('reserved', 'dispatched', 'skipped', 'failed', 'abandoned')),
    -- Written exactly once, on the first successful dispatch, and never
    -- overwritten. A crash after reserving and before dispatch is recovered by
    -- re-driving THIS row; a crash after dispatch finds the run id here and
    -- returns the prior receipt instead of starting a second run.
    workflow_run_id TEXT,
    -- Handed verbatim to WorkflowStore::create_run_idempotent_owned
    -- (crates/workflow/src/store.rs:354), so the workflow store's own
    -- idempotency is the second, independent guard.
    workflow_idempotency_key TEXT NOT NULL,
    last_error TEXT,
    updated_at TEXT NOT NULL,
    -- THE crash-consistency constraint. The INSERT is the reservation.
    UNIQUE (binding_id, dedup_key)
);

CREATE INDEX idx_automation_receipts_binding
    ON automation_receipts (binding_id, reserved_at DESC, id);
CREATE INDEX idx_automation_receipts_pending
    ON automation_receipts (state, updated_at) WHERE state = 'reserved';
CREATE INDEX idx_automation_receipts_expiry ON automation_receipts (expires_at);
CREATE UNIQUE INDEX idx_automation_receipts_delivery
    ON automation_receipts (binding_id, delivery_id) WHERE delivery_id IS NOT NULL;

-- Trigger-level attempts. Deliberately NOT workflow node retry (which lives in
-- workflow_nodes.attempt, migrations/0010_workflow_runs.sql): a trigger retry
-- re-attempts *starting* the run; a node retry re-executes a step of a run that
-- already started. Conflating them makes one failed dispatch spend the
-- workflow's retry budget.
CREATE TABLE automation_attempts (
    id TEXT PRIMARY KEY,
    receipt_id TEXT NOT NULL REFERENCES automation_receipts(id) ON DELETE CASCADE,
    attempt INTEGER NOT NULL CHECK (attempt >= 1),
    started_at TEXT NOT NULL,
    finished_at TEXT,
    outcome TEXT CHECK (outcome IS NULL OR outcome IN (
        'started', 'skipped_concurrency', 'queued', 'replaced',
        'filtered', 'denied', 'error'
    )),
    -- A dotted code only (`policy.denied`, `automation.repository-moved`), never
    -- a rendered message containing payload text.
    error_code TEXT,
    next_retry_at TEXT,
    UNIQUE (receipt_id, attempt)
);

CREATE INDEX idx_automation_attempts_retry
    ON automation_attempts (next_retry_at)
    WHERE next_retry_at IS NOT NULL AND finished_at IS NULL;

-- Durable binding-level lease. The plan forbids using the in-process
-- DriveLockRegistry (crates/codypendentd/src/workflows.rs:438) for this: it is a
-- HashMap in one process and provides no exclusion across a restart or a second
-- daemon instance. `fence` is monotonic so a lease holder that wakes up after
-- its lease expired can detect it was fenced out before writing.
CREATE TABLE automation_leases (
    binding_id TEXT PRIMARY KEY REFERENCES automation_bindings(id) ON DELETE CASCADE,
    holder TEXT NOT NULL,                  -- DaemonInstanceId
    acquired_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    fence INTEGER NOT NULL DEFAULT 1 CHECK (fence >= 1)
);

CREATE INDEX idx_automation_leases_expiry ON automation_leases (expires_at);
```

**Do not extend `webhook_deliveries`** (`migrations/0005_phase3.sql:9`). Its primary key is
`delivery_id` alone, which only GitHub supplies; it is the ingest crate's replay store and stays
that. Generic and internal triggers get their identity from `automation_receipts.dedup_key`.

---

## 3. Contract → implementation mapping

| Reserved protocol type | Daemon module / function | Handler site | `role_permits` |
|---|---|---|---|
| `AutomationBindingRequest::Create { binding }` | `crates/daemon/src/automation.rs::create_binding(pool, principal, draft)` | `crates/daemon/src/server.rs`, new `CommandBody::ManageAutomationBinding` arm, intercepted at the connection level alongside `StartWorkflow` (`server.rs:1972`) | `Controller` |
| `::Update { id, patch }` | `automation.rs::update_binding` | same arm | `Controller` |
| `::Delete { id }` | `automation.rs::delete_binding` | same arm | `Controller` |
| `::Get { id }` | `automation.rs::get_binding` → `AutomationBinding` | same arm | any attached role (read) |
| `::List { query }` | `automation.rs::list_bindings` → `AutomationBindingPage` (keyset cursor, copy `session_library.rs`'s `SearchCursor` shape) | same arm | any attached role (read) |
| `::Unknown` | — | same arm | `false` → `protocol.unsupported-payload` |
| `TriggerSource::GitHubWebhook` / `SignedWebhook` | `crates/codypendentd/src/webhook_dispatch.rs::WebhookEventSink` | injected into `WebhookIngestor` (`crates/codypendentd/src/lib.rs:409`) | n/a — not a client command |
| `TriggerSource::Cron` / `OneTime` | `crates/codypendentd/src/automation_scheduler.rs` | spawned from daemon startup next to `maybe_start_webhook_listener` (`lib.rs:381`) | n/a |
| `InvocationPolicy` → run creation | `WorkflowStarter::start` (`crates/codypendentd/src/workflows.rs:464`) via `StartWorkflowRequest` (`crates/daemon/src/workflows.rs:21-52`) | n/a | n/a |

### The "done" signal

**Delete `CommandBody::ManageAutomationBinding` from `is_reserved_unsupported_command`
(`crates/daemon/src/commands.rs:2982`).** While that line stands, every client — desktop, VS Code,
CLI, TUI — receives `protocol.unsupported-payload` no matter how much of the milestone is built.
M4 removes exactly one entry from that function; the remaining entries belong to M2.x, M3.x and the
bundle work.

### The rest of the new-command checklist (conventions §1)

1. `role_permits` (`crates/daemon/src/commands.rs:3001`) needs an arm. It matches on `&CommandBody`,
   so split read from write on the inner request:

   ```rust
   CommandBody::ManageAutomationBinding { request } => match request {
       AutomationBindingRequest::Get { .. } | AutomationBindingRequest::List { .. } => true,
       AutomationBindingRequest::Create { .. }
       | AutomationBindingRequest::Update { .. }
       | AutomationBindingRequest::Delete { .. } => matches!(role, Controller),
       _ => false,   // Unknown
   }
   ```

   `Controller` is the floor the plan specifies ("controller-gated mutation and observer-gated
   query commands", Task 4.1) and matches the existing floor for `CancelRun`/`PauseRun`/
   `MutateSessionLifecycle` at `commands.rs:3021-3025`.
2. Add a representative `ManageAutomationBinding` body to the guard test
   `every_client_issued_command_has_a_decided_role_floor`
   (`crates/daemon/src/commands.rs:5516`). It only protects what it enumerates.
3. `named_resources()` (`crates/protocol/src/command.rs:985`) must stop returning `Vec::new()` for
   this variant. Add `NamedResource::AutomationBinding(AutomationBindingId)` to the enum
   (`command.rs:925-941`) and return it for `Get`/`Update`/`Delete`. `Create` and `List` name no
   pre-existing binding — but see §4 for what `Create` must still check.
4. Add the ownership arm in `authorize_command` (`crates/daemon/src/server.rs:5143`). Copy the
   `NamedResource::Artifact` arm at `server.rs:5202-5213` verbatim in shape:

   ```rust
   NamedResource::AutomationBinding(id) => {
       let owner: Option<i64> = sqlx::query_scalar(
           "SELECT owner_uid FROM automation_bindings WHERE id = ?")
           .bind(id.to_string()).fetch_optional(&state.pool).await?;
       if owner.map(|u| u as u32).unwrap_or(state.daemon_uid) == principal.uid()
           && owner.is_some() { continue; }
       Refusal::Rejected(CodypendentError::new(
           "automation.binding-not-found", "automation binding is unavailable", false))
   }
   ```

   The refusal text must be identical for *unauthorized* and *absent* — that is the program rule
   (conventions §2), and the `Artifact` arm's generic `"artifact is unavailable"` is the precedent.
5. `protocol-vectors/automation.json` already exists, so no new vector file. If you add vectors,
   every one must be added to the `modeled` or `notModeled` list in
   `extensions/vscode/test/protocol-vectors.test.ts` or `assertPartitionIsComplete` fails, and the
   `doc-count:vitest` markers drift (conventions §5).
6. `ClientCapabilities.automation` (`capabilities.rs:49`) should gate whether the daemon *offers*
   automation surfaces to a client — it must never gate authorization. A client that lies about the
   bit still hits `role_permits` and the ownership gate.

---

## 4. Security requirements

Automation is the milestone that can silently turn the deny-first policy into an advisory. These
are not guidelines.

### 4.1 A remote or scheduled trigger may NARROW but never BROADEN local policy

Design spec §4.1 (`specs/2026-08-16-hybrid-platform-program-design.md:105-107`): *"A cloud grant can
never broaden the local daemon's security decision. The effective authority is the intersection of
remote RBAC, organization policy, repository policy, and local policy."* Concretely:

- **Automation triggers workflows; it never executes them.** The only path from a firing to work is
  `WorkflowStarter::start` (`crates/codypendentd/src/workflows.rs:464`), the identical seam a human
  `StartWorkflow` uses. Every node then runs under the same `PolicyEngine` the human path uses. Do
  not add an "automation" execution path, an "automation" tool allowlist, or an `EvalContext`
  variant. If a workflow node needs approval when a human starts it, it needs approval when a cron
  starts it.
- **`AutomationApprovalMode::Preapproved { approval_receipt }` is a reference, never a grant.** The
  receipt must already exist in the approval store, must be digest-bound to the exact action, and
  must be *verified* at use. Automation must never mint an approval, never mark one satisfied, and
  never widen one. `AlwaysRequire` and `PolicyDriven` may only make the run *stricter* than
  `Inherit`; there is no mode that makes it looser. The precedent is
  `RunPolicyAdapter::authorize` (`crates/daemon/src/policy_gate.rs:152-155`), which turns
  `Decision::RequireApproval` into a **refusal** rather than a wait, because an unattended context
  has nobody to prompt. A scheduled firing is exactly such a context.
- **`BudgetCeiling` is a minimum operation, not a replacement.** The effective limit for each
  dimension is `min(workflow budget, binding ceiling)` — see `BudgetLimits::resolve`
  (`crates/workflow/src/budget.rs:274`). A binding must never raise a workflow's own budget.
- **The binding row is the only authority for owner, repository, workflow, version, budget and
  approval mode.** Resolve all six from `automation_bindings` keyed by `endpoint_id` (+ filters).
  Nothing in the payload selects any of them. The plan's Task 4.4 hostile-payload test is exactly
  this: *"proving payload cannot select owner/repository/workflow/budget/approval."*
- **Repository authorization is re-checked at fire time, not only at create time.** Call
  `principal_owns_repository` (`crates/daemon/src/server.rs:4965`) against the binding's
  `owner_uid` when the binding is created, and re-derive `repository_id` from `repository_path`
  before dispatch. A binding whose checkout moved must fail with
  `automation.repository-moved`, not silently run somewhere else.

### 4.2 Webhook signature verification

- **Verify before parse.** Already correct at `ingest.rs:95-110`, pinned by
  `forged_signature_on_unparseable_body_rejected_before_parse` (`ingest.rs:193`). The generic
  adapter (`crates/integrations/src/webhook/generic.rs`) must reproduce it, not re-derive it.
- **No unsigned mode, ever.** `ingest.rs:95-99` turns an absent/empty secret into
  `WebhookError::Config`, not into an accept. Keep that for every endpoint;
  `missing_secret_fails_closed` (`ingest.rs:158`) pins it.
- **Constant-time comparison.** `verify::verify_signature` already does this; do not hand-roll a
  second comparator in `generic.rs`.
- **Bound the body before reading it.** `MAX_BODY_BYTES` (`webhook/server.rs:80`) refuses an
  oversized `Content-Length` up front. Per-endpoint limits (`automation_endpoints.body_limit_bytes`)
  must be applied at the same point — *before* the body is read, not after.
- **Errors are bounded and uninformative.** A verification failure returns a status and a dotted
  code. Never echo payload content, never distinguish "unknown endpoint" from "bad signature" —
  both are the same refusal, or the endpoint namespace becomes enumerable.

### 4.3 Replay protection

GitHub's HMAC covers the body only. That is why `ingest.rs:118-125` reserves **two** identities in
one store transaction: the delivery GUID and `body-sha256:<sha256(signature)>`. Preserve both, and
add the two the current design cannot express:

- **A signed timestamp.** For `TriggerSource::SignedWebhook`, the signed envelope must include a
  timestamp (and ideally a nonce) so a delivery older than
  `automation_endpoints.replay_window_seconds` is refused *before* the dedup store is consulted.
  Without it, replay protection has the lifetime of the dedup rows, and pruning them reopens the
  window.
- **Endpoint binding.** The signature must cover the endpoint id, or a delivery captured on one
  endpoint can be replayed against another endpoint that happens to share a key.
- **Reservation before effect, always.** The order is verify → normalize → reserve → dispatch.
  Normalizing before reserving is deliberate: a malformed payload must not burn a delivery GUID and
  make GitHub's legitimate retry look like a duplicate (`malformed_payload_does_not_burn_the_delivery_id`,
  `ingest.rs:267`). A crash between reserve and dispatch is recovered from
  `automation_receipts.state = 'reserved'`, which is why `workflow_run_id` is write-once.

### 4.4 Secrets

- `automation_endpoints.signing_key_ref` holds a reference, never material — the protocol contract
  already commits to this (`crates/protocol/src/automation.rs`, `signed_webhook_round_trip_contains_reference_not_secret`).
- Any new config type carrying a secret needs a hand-written `Debug` that redacts it. The precedent
  is `WebhooksConfig` (`crates/integrations/src/webhook/config.rs:31-41`) and `GitHubToken`
  (`crates/integrations/src/github/secret.rs:68-73`). A derived `Debug` on a key-bearing struct is
  a leak into `tracing` the first time anyone writes `{:?}`.
- Filters, normalized payload fields and error text can carry attacker-controlled strings.
  Run anything that reaches a durable note or memory through
  `codypendent_knowledge::detect_secret` (`crates/knowledge/src/memory.rs:943`), as
  `commands.rs:2117` already does.

---

## 5. Acceptance criteria

Each is objectively checkable and names the test that proves it.

1. **The command is reachable.** `ManageAutomationBinding` no longer appears in
   `is_reserved_unsupported_command` (`commands.rs:2971`), and a `Controller` client receives an
   `AutomationBinding` rather than `protocol.unsupported-payload`.
   → `automation_it::create_get_list_update_delete_round_trips_for_a_controller`
2. **Role floors hold.** `Observer` can `Get`/`List` and is refused `Create`/`Update`/`Delete` with
   `protocol.role-denied`; the guard test enumerates the variant.
   → `commands::every_client_issued_command_has_a_decided_role_floor`,
     `automation_it::observer_may_read_but_not_mutate_bindings`
3. **Unauthorized and absent are byte-identical.** A `Get` for another uid's binding id and a `Get`
   for a random unused id return the same `automation.binding-not-found` error body; a `List` from
   a foreign principal returns zero items and no cursor.
   → `automation_it::foreign_binding_and_missing_binding_are_indistinguishable`
4. **Owner is server-derived.** A `Create` whose payload attempts to set an owner is stored under
   the connection's peer uid.
   → `automation_it::binding_owner_is_the_peer_uid_not_the_payload`
5. **A valid enabled binding starts exactly one workflow run.**
   → `webhook_workflow_it::valid_delivery_starts_one_run`
6. **A duplicate delivery GUID returns the prior receipt and starts nothing.**
   → `webhook_workflow_it::duplicate_guid_returns_prior_receipt`
7. **Invalid signature, non-matching filter, unknown endpoint, disabled binding and foreign
   repository each start zero runs, with identical refusal shape for unknown-endpoint and
   bad-signature.**
   → `webhook_workflow_it::rejected_deliveries_start_no_runs`
8. **A crash after reservation and before dispatch re-drives without a duplicate external effect.**
   Kill after the `automation_receipts` INSERT commits; recovery produces one run, and
   `workflow_run_id` is written once.
   → `webhook_workflow_it::crash_after_receipt_retries_without_duplicate_effect`
9. **The payload cannot select owner, repository, workflow, budget or approval mode.** A hostile
   body carrying all five is dispatched under the binding's values.
   → `webhook_workflow_it::hostile_payload_cannot_select_binding_policy`
10. **A binding budget only narrows.** A binding ceiling above the workflow's own budget produces
    the workflow's budget; below it produces the binding's.
    → `automation_it::binding_budget_ceiling_narrows_never_widens`
11. **`Preapproved` does not bypass approval.** A binding with a fabricated/unknown
    `approval_receipt` fails closed with `policy.approval-required`; a run whose node demands an
    unheld approval does not proceed.
    → `automation_it::preapproved_receipt_must_already_exist`
12. **Schedules are paused-time deterministic.** Cron next-fire preview, a DST spring-forward and a
    DST fall-back all produce the documented occurrence set with no duplicates and no skips.
    → `automation_scheduler::cron_occurrences_are_deterministic_across_dst`
13. **Restart recovery honours the missed-run policy.** `skip`, `run_once` and
    `catch_up { max_occurrences }` each produce the exact firing count after a simulated downtime.
    → `automation_scheduler::missed_runs_follow_the_configured_policy`
14. **Concurrency policies behave.** `allow`, `skip`, `queue` and `replace` each produce the
    documented run set when a firing lands while a prior run is active.
    → `automation_scheduler::concurrency_policy_governs_overlapping_firings`
15. **The scheduler claim is durable, not in-process.** Two daemon instances over one database
    produce one firing per occurrence; `DriveLockRegistry` is not referenced by the scheduler.
    → `automation_scheduler::two_instances_claim_each_occurrence_once`
16. **Trigger retry is separate from node retry.** A dispatch that fails three times increments
    `automation_attempts` only; `workflow_nodes.attempt` is untouched.
    → `automation_scheduler::trigger_retry_does_not_consume_node_retry`
17. **Replay outside the signed window is refused before the dedup store is consulted.**
    → `generic_webhook_it::stale_signed_timestamp_is_refused_before_dedup`
18. **No secret reaches a log, an error body or the database.** Grep the test-run logs and the
    SQLite file for the sentinel signing key after the full webhook suite.
    → `generic_webhook_it::signing_key_never_appears_in_logs_or_storage`
19. **Every template compiles and runs end to end.** Each manifest under `docs/specs/workflows/`
    has an immutable id/version, resolves through `crates/workflow/src/source.rs`, uses only
    production-supported node types, and completes against a safe fixture via the trigger path.
    → `spec_it::template_catalogue_is_valid`, `spec_it::each_template_runs_through_the_trigger_service`
20. **The CI gates that `cargo test` does not run are green.**
    `python3 .github/scripts/check_migration_immutability.py`,
    `python3 .github/scripts/check_doc_test_counts.py --skip-vitest`,
    `python3 .github/scripts/check_docs_manifest.py`, and the `extension` job's vector partition.

---

## 6. Gotchas

**`RepositoryId` cannot be turned back into a path.** `scan::repository_id_for`
(`crates/codypendentd/src/scan.rs:1130`) hashes the canonical root via
`codypendent_knowledge::stable_repository_id`. There is no `repositories` table anywhere in
`migrations/` — the only `repository_id` column is the one `0040_session_library.sql:19` added to
`sessions`. But `AutomationBindingDraft.repository_id` is a `RepositoryId` and
`StartWorkflowRequest.repository` is a path string (`crates/daemon/src/workflows.rs:45`). Persist
`repository_path` on the binding (§2) and re-derive the id to detect drift. Do not invent an
id→path lookup by scanning sessions; a binding can outlive every session that mentions its
repository.

**M4 needs per-endpoint secrets and M5 builds the broker.** `automation_endpoints.signing_key_ref`
is designed for M5's resolver. Until then, support only `env:<NAME>` references resolved at
verification time — which is exactly the compatibility form Task 5.2 keeps (*"compatibility
references such as `env:GITHUB_TOKEN`"*). Do **not** add a plaintext `signing_key` column "for
now": migrations are append-only and you will never be able to drop it.

**The HTTP listener discards the path.** `handle_connection` (`webhook/server.rs:83`) reads the
request line only to check the method (line 114). Adding `endpoint_id` routing means editing this
function and its header loop; the existing `MAX_HEADER_BYTES`/`MAX_BODY_BYTES` guards
(lines 76-80) and the 431/400/405 responses must be preserved.

**`webhooks.toml` is loaded once at startup.** `maybe_start_webhook_listener`
(`crates/codypendentd/src/lib.rs:381-427`) reads the config, builds one ingestor and spawns the
server. Bindings, by contrast, are mutable at runtime. Resolve bindings *per delivery* from the
database; never snapshot them into the ingestor at construction, or a newly created binding stays
dead until restart.

**`is_fresh` guards double-driving, not double-creating.** `WorkflowConductorHost::is_fresh`
(`crates/codypendentd/src/workflows.rs:449`) only prevents a second *drive* of an already-advancing
run. The no-duplicate-run guarantee comes from `create_run_idempotent_owned`'s key
(`crates/workflow/src/store.rs:354`) plus your `UNIQUE (binding_id, dedup_key)`. Derive
`workflow_idempotency_key` from binding id + dedup key, exactly as plan Task 4.2 says.

**A reused idempotency key under a different manifest is a client error, not a retry.**
`workflows.rs:544-551` documents this (`P5-D2`). If a binding's `workflow_version` changes, the
dedup key must change with it, or a re-fire after the version bump surfaces a confusing
non-retryable store error.

**`ConcurrencyPolicy::Replace` needs an approval story.** The protocol calls it "approved replace
behavior" (plan Task 4.3). Cancelling an in-flight run destroys work; treat the replace as a
policy-gated action, not a bare `CancelWorkflow`.

**`TriggerSource` and every policy enum are `#[non_exhaustive]` with `#[serde(other)] Unknown`.** A
binding row whose `source_json` deserializes to `Unknown` must be treated as disabled, never as a
default. Same for `ConcurrencyPolicy::Unknown`, `MissedRunPolicy::Unknown`,
`AutomationApprovalMode::Unknown`. Silently defaulting `Unknown` approval mode to `Inherit` is a
downgrade attack via an older daemon reading a newer row.

**`0` is not "unbounded".** `TriggerRetryPolicy::default()` sets `max_attempts: 0`
(`crates/protocol/src/automation.rs`), which means *no retry*. `BudgetCeiling`'s fields are
`Option<u64>` and `None` means *unset*. Persist `NULL`, not `0` (conventions §8).

**Timezone-aware cron is a new workspace dependency.** Plan Task 4.3 requires a pinned tz-aware
cron library. Add it to `[workspace.dependencies]` in the root `Cargo.toml` (line 39 onward) with
an exact version, and confirm the licence — this repo ships a single binary.

**Clippy runs with `--all-targets`.** Conventions §10: a helper used only by the scheduler's
`#[cfg(test)]` module, or a macOS-gated function, is the classic way local green becomes CI red.
