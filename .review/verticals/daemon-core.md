# daemon-core — review report

Vertical: `crates/daemon/{ledger,policy/**,approvals,server,db,commands,instance,subscriptions,recovery,replay,lib,documents,remote_ui*}.rs`,
`crates/protocol/**`, `migrations/**`.
Owned outcomes: **20 (the ledger made visible)**, **19 (real multi-user)**.
Pinned commit `535a2f5e3848b256536ddee94883dc0010ecdcb8` (v0.4.5). No code changed.

---

## Verdicts

**OUTCOME 20: BROKEN** — the ledger records tokens exactly once, in a JSON
artifact that no wire command can fetch and the one artifact reader in the
product explicitly refuses; cost is never computed for a session run, latency is
never written at all, and the client-facing `BudgetState` subscription silently
delivers zero events.

**OUTCOME 19: ABSENT (and worse than absent — the trust boundary is open)** —
there is no principal on a connection. `client_id` and `ClientRole` are both
asserted by the client in plaintext, the connection defaults to `Controller`,
and no read or write path checks session ownership. I connected a fresh,
never-attached client and (a) read another session's entire event history and
(b) approved another client's parked `ls -la`, which then executed.

---

## How this was exercised

Real daemon, real ledger, temp DB:

```
CODYPENDENT_DATA_DIR=<scratch>/dc CODYPENDENT_SOCKET=/home/user/cdpr/d.sock \
  ./target/debug/codypendentd
```

Model: a local OpenAI-compatible stub on `127.0.0.1:8123` that returns real
`usage` blocks (`prompt_tokens`/`completion_tokens`/`total_tokens`) and scripted
`tool_calls`. Three runs driven through `codypendent run --jsonl`:

1. Explore, no tools → 1 model request.
2. Build, scripted `shell.run` (bad args) → `workspace.read_file main.rs` →
   `workspace.read_file /etc/shadow` (policy deny) → finish. 4 model requests.
3. Build, scripted `shell.run ls -la` → parks for approval → resolved by a
   *stranger* client over a raw socket → executes → finish. 2 model requests.

Wire probes were driven by a 60-line Python client speaking the length-prefixed
JSON framing (`crates/protocol/src/framing.rs`).

---

## The ledger table dump (core evidence for outcome 20)

After three completed runs, every table in the DB:

```
approvals                     1      index_outbox                 25
artifact_upload_commands      0      learning_records              0
artifacts                     4      memories                      0
blackboard_items              0      model_profiles                0
code_edges                    2      pending_effects               0
code_nodes                    2      promotion_*                   0 (all 5)
commands                      6      registry_embeddings           0
daemon_instance               1      registry_items               21
document_* (5 tables)         0      runs                          3
documents                     0      sessions                      3
eval_suite_reports            0      ui_plugin_commands            0
events                       43      webhook_deliveries            0
ide_context                   0      workflow_* (3 tables)         0
                                     workspace_leases              1
```

`runs` — every row, after three *completed* runs:

```
{'id':'019ff87a-…','state':'Completed','mode':'Explore','budget_json':'{}','started_at':None,'ended_at':None}
{'id':'019ff87f-…','state':'Completed','mode':'Build',   'budget_json':'{}','started_at':None,'ended_at':None}
{'id':'019ff888-…','state':'Completed','mode':'Build',   'budget_json':'{}','started_at':None,'ended_at':None}
```

`sessions` — every row:

```
{'id':'019ff87a-…','workspace_id':None,'title':'Read main.rs and summarise it','state':'open',…}
```

Event-type histogram over all 43 events:

```
ApprovalRequested 1   ApprovalResolved 1   ClientPresenceChanged 10
ModelStreamDelta 3    NoteAppended 5       RunCompleted 3
RunStarted 3          RunStateChanged 11   SessionCreated 3
ToolCompleted 4       ToolDenied 1         ToolProposed 1   ToolStarted 2
```

The only place any number lands — the chronicle artifact for run 2, read off
disk from `<data_dir>/artifacts/sha256/…`:

```json
{
  "objective": "Inspect the repo",
  "specification": null, "plan_versions": [], "investigations": [],
  "decisions": [],
  "actions": [
    {"tool":"shell.run","outcome":"failed","artifact":null},
    {"tool":"workspace.read_file","outcome":"succeeded","artifact":"019ff87f-90d4-…"},
    {"tool":"workspace.read_file","outcome":"denied","artifact":null}
  ],
  "changes": [], "verification": [],
  "costs": { "model_requests": 4, "tokens": 4212, "cost_micros": null },
  "unresolved": []
}
```

`tokens: 4212` is the true provider-reported sum (1050+1052+1054+1056). So the
measurement is honest and real. `cost_micros` is null. There is no latency
field. And nothing reads this file — see F-20-1.

---

## Producer → consumer table

"Consumer" means *reaches a human*. Test/fixture-only readers do not count.
`R` = `crates/tui/src/reduce.rs`, `X` = `extensions/vscode/src/extension.ts`.

| # | EventBody | Producer (file:line) | Consumer that surfaces it | Verdict |
|---|---|---|---|---|
| 1 | `SessionCreated` | `crates/daemon/src/commands.rs:444` | `R:1502` title; `X:598` | ok |
| 2 | `NoteAppended` | `crates/daemon/src/commands.rs:1131`, `crates/runtime/src/agent.rs:2186` & `:4007`, `crates/codypendentd/src/executor.rs:1332`/`1834`/`3481`/`4987` | `R:1503` (context/memory notes folded into a collapsed "Backstage" entry); `X:595` | ok |
| 3 | `SessionClosed` | **NONE** | `R:1566`, `crates/daemon/src/replay.rs:28`, `crates/daemon/src/commands.rs:2439` | **F-20-6: unreachable** |
| 4 | `RunStarted` | `crates/daemon/src/commands.rs:483`,`:533` | `R:1573`; `X:580` | ok |
| 5 | `RunStateChanged` | `crates/daemon/src/ledger.rs:248`, `commands.rs:611`, `agent.rs:2614`, `recovery.rs:253`/`274`/`358`, `workflow_exec.rs:2202` | `R:1615`; `X:519` | ok |
| 6 | `ModelStreamDelta` | `crates/runtime/src/agent.rs:2383`,`:2656` | `R:1629`; `X:523` | ok |
| 7 | `ToolProposed` | `crates/runtime/src/agent.rs:2822`, `crates/codypendentd/src/executor.rs:1980` | `R:1635`; `X:529` | ok |
| 8 | `ToolDenied` | `crates/runtime/src/agent.rs:2762` | `R:1666`; `X:533` | **F-20-3: dropped by `RunTrace`; reason `code` discarded** |
| 9 | `ToolStarted` | `crates/runtime/src/agent.rs:2876` | `R:1703`; `X:587` | ok |
| 10 | `ToolCompleted` | `crates/runtime/src/agent.rs:2717`/`2777`/`2852`/`2900`, `workflow_exec.rs:2184` | `R:1745`; `X:589` | ok (artifact ref shown, bytes unfetchable — F-20-2) |
| 11 | `PatchProposed` | `crates/runtime/src/agent.rs:4503`, `crates/codypendentd/src/executor.rs:1296` | `R:1774`; `X:562` | ok (inline `preview` only) |
| 12 | `ApprovalRequested` | `crates/daemon/src/approvals.rs:305` | `R:1801`; `X:540` | ok |
| 13 | `ApprovalResolved` | `crates/daemon/src/approvals.rs:323`,`:532`,`:601`,`:660` | `R:1823`; `X:557` | ok |
| 14 | `SteeringQueued` | `crates/daemon/src/commands.rs:576` | `R:1848` | ok (not in `X` — falls to `default:`) |
| 15 | `SteeringApplied` | `crates/runtime/src/agent.rs:2687` | `R:1853` | ok, but carries no text (documented at `session_history.rs:31`) |
| 16 | `BudgetWarning{Tokens}` | `crates/runtime/src/agent.rs:448` | `R:1876` → `run.context_percent`; `X:592` | fires only when `driver.context_window()` is `Some`; value is an **estimate**, not measured |
| 17 | `BudgetWarning{WallClock}` | `crates/runtime/src/agent.rs:2136` | `R:1867` transcript card only — `_ => {}` at `R:1880` drops it from the status bar | partial |
| 18 | `BudgetWarning{Cost}` | **NONE** | `R:1879` sets `run.cost_minor`; `render.rs:436`,`553`,`613` render it | **F-20-4: the cost chip can never render** |
| 19 | `BudgetWarning{ToolCalls}` | **NONE** | `R:1880` `_ => {}` | dead variant |
| 20 | `RunCompleted` | `crates/runtime/src/agent.rs:2001`/`2036`/`2577`, `recovery.rs:293`/`380`, `executor.rs:1222` | `R:1893` reads `{run_id, disposition, ..}` — **`chronicle` discarded**; `X:583` prints `disposition.type` only | **F-20-1** |
| 21 | `LearningsCaptured` | `crates/codypendentd/src/executor.rs:1645` | `R:1916` (a counter; deliberately no card) | ok |
| 22 | `ClientPresenceChanged` | `crates/daemon/src/server.rs:4457` | `R:1933` → 10-tick status notice; `X` `default:` arm → bare label, no detail | **F-19-4** |

**Events with no consumer that surfaces them to a human: none.**
**Events with no producer: `SessionClosed`, `BudgetWarning{Cost}`, `BudgetWarning{ToolCalls}`.**
That is the real shape of the problem: outcome 20 is not "the ledger has no
reader" at the *event* level. Every event type is rendered somewhere. What has
no reader is the **numbers**: they are not in events at all. They are in a JSON
blob nothing can open, and in two DB columns nothing writes.

---

# Findings — outcome 20

### F-20-1 — the run chronicle is the only cost record and it is unreadable. Class (b)
`crates/runtime/src/agent.rs:2577` emits `RunCompleted { chronicle: ArtifactRef }`.
The chronicle carries `costs: {model_requests, tokens, cost_micros}` (proved
above: `tokens: 4212`).

Three independent walls stop it reaching a human:

1. **The TUI throws the field away.** `crates/tui/src/reduce.rs:1893`
   destructures `EventBody::RunCompleted { run_id, disposition, .. }`. The `..`
   is the chronicle.
2. **There is no wire command to fetch artifact bytes.** `CommandBody` has
   `PutArtifact` (`crates/protocol/src/command.rs:586`) and no `GetArtifact`.
   Enumerated the whole enum: `ReadSessionEvents`, `ReadBlackboard`,
   `ReadWorkflowRun`, `PutArtifact` — that is the entire read surface.
3. **The one artifact reader that exists refuses chronicles by construction.**
   `crates/daemon/src/server.rs:3745`:
   ```rust
   let ProvenanceSource::ToolOutput { run_id, .. } = &provenance.source else {
       anyhow::bail!("artifact has no session-bound provenance");
   };
   ```
   Chronicles are written with `Provenance::system("run-chronicle")`
   (`crates/runtime/src/agent.rs:2551`, `crates/daemon/src/recovery.rs:223`) →
   `ProvenanceSource::System` → hard-refused. Confirmed in the DB dump: the
   chronicle rows carry `{"source":{"kind":"system","detail":"run-chronicle"}}`.

The codebase states the consequence itself, at
`crates/codypendentd/src/session_history.rs:18`: *"Why `RunCompleted.chronicle`
is never dereferenced"*.

**User-visible:** a user finishes a run that cost 4,212 tokens. Nowhere in the
TUI, the CLI, or the extension is any token count, cost, or duration shown. The
TUI header shows `cost —`.

### F-20-2 — `RunOutcome.usage` is discarded for every session run. Class (b)
`crates/codypendentd/src/executor.rs:902-904`:
```rust
let result = runtime
    .execute_run(&driver, ctx, token)
    .await
    .map(|_| ())                       // <-- RunOutcome { disposition, usage } dropped
    .map_err(|e| format!("run failed: {e}"));
```
`RunOutcome.usage` is the run's aggregated **measured** `ModelUsage`
(`crates/runtime/src/agent.rs:724-729`) — prompt tokens, completion tokens,
cost. The only consumer of that value anywhere is the *workflow node* path
(`crates/codypendentd/src/workflow_exec.rs:953`, `node_cost_micros` at `:224`),
which writes it to `workflow_nodes.cost_json`. An ordinary `codypendent run`
never touches workflow tables, so this is the sole reason `cost_micros` is
`null` in every chronicle above: **price × tokens is only ever computed inside a
workflow node.**

### F-20-3 — policy denials are dropped from the run trace, and their machine code is discarded. Class (c)
Two separate defects on the same event.

(a) `crates/daemon/src/server.rs:4607-4624` (`event_run_id`) enumerates the
run-scoped event types and **omits `ToolDenied`** (and `NoteAppended`, whose
`run_id` is `Option`). `subscription_matches` (`:4601`) resolves
`Subscription::RunTrace { run_id }` as `event_run_id(event) == Some(run_id)`.
So a client subscribed to *"the detailed trace of one run"* receives **no policy
denials at all**. The same omission is duplicated in
`crates/cli/src/stream.rs:184-197`.

(b) `crates/runtime/src/agent.rs:2765-2770` maps `decision.reasons` to
`reason.message` only. `PolicyReason.code` — documented at
`crates/daemon/src/policy/mod.rs:59-63` as *"a stable dotted identifier"*, i.e.
the machine contract — never reaches the ledger, and neither does
`decision.policy_version` (`crates/daemon/src/policy/mod.rs:102`). The stored
event for my forced denial:
```json
{"type":"ToolDenied","run_id":"…","action":{"type":"ReadFiles","paths":["/etc/shadow"]},
 "reasons":["read outside the allowed roots: /etc/shadow"]}
```
No `policy.path-out-of-scope` code, no policy version. A denial audit cannot be
grouped, counted, or attributed to a policy revision — only substring-matched on
English prose the codebase itself mutates at `agent.rs:2755` (it appends a hint
to the message for one specific code).

### F-20-4 — the cost chip in the TUI status bar can never render. Class (b)
`crates/tui/src/reduce.rs:1879` is the sole writer of `run.cost_minor`, gated on
`BudgetDimension::Cost`. Grepping every emitter of `EventBody::BudgetWarning`:
`crates/runtime/src/agent.rs:448` (`Tokens`) and `:2136` (`WallClock`). Nothing
emits `Cost`. (`crates/workflow/src/budget.rs` has its own, *different*,
`BudgetDimension` that never becomes a `SessionEvent`; `crates/cli/src/eval.rs:596`
constructs one only for a test double.)

So `crates/tui/src/render.rs:553` `show_cost = status.cost_minor.is_some()` is
permanently false, and the verbose telemetry row (`render.rs:436`) permanently
prints `cost —`. Engine (`format_cost` at `render.rs:9504`, the packing logic,
the reducer arm) fully built and unit-tested at `render.rs:10534`; the producer
was never written.

### F-20-5 — `runs.started_at` / `runs.ended_at` are dead columns; per-run latency does not exist. Class (a)
Declared in `migrations/0002_phase1.sql:48-49`. The only INSERT is
`crates/daemon/src/projections.rs:104-107`, which lists seven columns and omits
both. There is no `UPDATE runs SET started_at`/`ended_at` anywhere in the
workspace (`crates/daemon/src/projections.rs` has exactly three UPDATEs: state,
state-if-legal, workspace_lease_id). Proved in the dump: three completed runs,
all `started_at: None, ended_at: None`.

They **are** read: `crates/daemon/src/server.rs:3537-3545` selects them for the
Remote-UI `run` projection and maps them to `UiRunProjection.started_at` /
`.completed_at` (`crates/protocol/src/remote_ui.rs:952-954`). The same struct
has `cost: Option<f64>`, hardcoded `None` at `crates/daemon/src/server.rs:3568`,
and `progress: None` at `:3567`. So the *one* by-id run reader that plugins get
returns a struct whose cost, progress, start time and end time are structurally
always null. That is outcome 20's target shape already wired — with nothing
behind it.

`runs.budget_json` is likewise always `"{}"` — `DEFAULT_BUDGET_JSON` at
`crates/daemon/src/commands.rs:57`, bound at `:976`, never overwritten.

### F-20-6 — `EventBody::SessionClosed` has no producer. Class (a)
Zero write sites in the workspace. There is no `CloseSession` command in
`CommandBody`. Three consumers exist: `crates/tui/src/reduce.rs:1566` (which
sets the notice *"Session closed · transcript remains available"* and idles
every run), `crates/daemon/src/replay.rs:28`, and
`crates/daemon/src/commands.rs:2439`. `sessions.state` is written `'open'` at
`crates/daemon/src/ledger.rs:20` and never changed; `session_projection`
(`crates/daemon/src/projections.rs:234`) computes `closed = state == "closed"`,
permanently false. Confirmed in the dump: `'state': 'open'` on a session whose
run finished and whose client disconnected.

### F-20-7 — `codypendent run --jsonl` always terminates one event early. Class (c)
`crates/cli/src/stream.rs:154-157` returns as soon as *either*
`RunCompleted` *or* `RunStateChanged{Completed}` is seen. The daemon emits
`RunStateChanged{Completed}` **before** `RunCompleted`
(`crates/runtime/src/agent.rs:2572` `transition_if_needed(… Completed)` then `:2574` `emit(RunCompleted)`). Reproduced twice: my JSONL
capture ends at sequence 8 while the ledger holds 10; and ends at 13 while the
ledger holds 17. The headless consumer therefore never receives the
`RunCompleted` disposition summary, the chronicle ref, or the trailing
`LearningsCaptured` — the three things a CI/eval integration would want most.

### F-20-8 — `Subscription::BudgetState` (and `RepositoryStatus`, `Document`, `Blackboard`, `Workflow`) silently deliver nothing. Class (c)
`crates/daemon/src/server.rs:4599-4603`:
```rust
subscriptions.iter().any(|subscription| match subscription {
    Subscription::SessionSummary | Subscription::AgentActivity => true,
    Subscription::RunTrace { run_id } => event_run_id(event) == Some(*run_id),
    _ => false,                       // BudgetState, RepositoryStatus, Document, …
})
```
**Reproduced on the wire.** A client attached with
`subscriptions: [{"type":"BudgetState"}]` received `Catchup` and then, over the
following seconds while another client attached and presence fired, **nothing at
all** — not even its own presence event. The peer attached with
`SessionSummary+AgentActivity` received everything. A client that asks for the
budget view is put into a permanent, unreported blackout. This is the textbook
SILENT FILTER: no error, no warning, an empty stream that looks like an idle
session.

### F-20-9 — `approvals.capabilities_json` is written and never shown. Class (b)
`crates/daemon/src/approvals.rs:264`/`:290` persist the minted
`Vec<Capability>` for every approval. The only reader is
`reload_pending` (`:716`/`:899`), used by restart recovery. No client-facing
path ever renders which capability an approval actually grants — the approval
card shows only `ProposedAction` + `Risk`. `resolved_by` (a bare client UUID) is
written at `:509-515` and read by nothing.

---

# Findings — outcome 19

## The scopes that exist

Enumerated from the protocol and the schema — three parallel, non-interoperating
hierarchies:

1. `codypendent_knowledge::Scope` (`crates/knowledge/src/types.rs:34`) —
   `System | Organization(OrganizationId) | User(UserId) | Workspace(WorkspaceId) |
   Repository(RepositoryId) | Branch(BranchId) | Session(SessionId) | Task(TaskId)`.
   Flattened to `scope_tier`/`scope_key` in `registry_items` (0003),
   `memories` (0003), `documents` (0008).
2. `codypendent_protocol::ScopeLevel` (`crates/protocol/src/input.rs:277`) — the
   same eight names as a wire enum, plus `Unknown`. Used only to *label* input.
3. `LearningScope` (`migrations/0024_learning.sql`, `scope_kind`) —
   `user | repository | provider | council`. A fourth, different taxonomy.

What is actually populated:

| Tier | Reachable today? | Evidence |
|---|---|---|
| `System` | yes | built-in tool registration |
| `Repository` | yes | `stable_repository_id(path)` — a hash of a canonical path |
| `User` | **one constant** | `crates/knowledge/src/skills.rs:46` `pub const LOCAL_USER_KEY: &str = "local"` — *"The daemon serves exactly one OS user"* |
| `Workspace` | **client-side fiction** | see F-19-2 |
| `Organization` | **client-asserted, unvalidated** | see F-19-3 |
| `Branch`, `Task` | parse-only | `crates/knowledge/src/registry.rs:378`,`:380` — no construction site outside deserialization |

### F-19-1 — there is no principal. The actor is whatever the client types. Class (a) — THE trust-boundary finding
Three lines establish "who you are", and none of them authenticate anything.

1. **Identity.** `crates/daemon/src/server.rs:1195-1201`:
   ```rust
   let client_id = hello.resume_token.as_ref()
       .and_then(|token| resume::verify_resume_token(&state.secret, &token.0))
       .map(|claims| claims.client_id)
       .unwrap_or(request.client_id);       // <-- the client's own envelope field
   conn.client_id = Some(client_id);
   ```
   `Envelope.client_id` (`crates/protocol/src/envelope.rs:31`) is a plain
   client-supplied UUID. A resume token is HMAC'd against `daemon.secret`, but
   presenting **no** token is the fallback path, not a rejection — so the token
   is a convenience, never a credential.
2. **Authority (default).** `crates/daemon/src/server.rs:630`:
   `role: ClientRole::Controller`. Every handshaken connection starts as a
   Controller.
3. **Authority (declared).** `crates/daemon/src/server.rs:1275`:
   `conn.role = *requested_role;` — verbatim from the client's `AttachSession`,
   with the comment *"role is a connection-level assertion under the Phase 1
   local trust model"*. Nothing narrows it; `Approver` is granted on request.

`ClientHello` carries no user field (`crates/protocol/src/handshake.rs:25-34`);
`ServerHello` deliberately omits `authentication` (`:38-41`). `UserId` is then
**manufactured from the client's own UUID** at
`crates/daemon/src/server.rs:2065` and `:2118`
(`UserId(client_id.to_string())`) and at
`crates/daemon/src/approvals.rs:530` (`Actor::Human { user_id: UserId(resolved_by) }`).

**Proved on the wire.** A fresh Python client, one `ClientHello`, no attach:

```
A) ResolveApproval with NO attach:
   {"type":"CommandAccepted","command_id":"cb0c71bf-…","sequence":10}
```

and the ledger then recorded, and the daemon then executed:

```
10 {'type':'Human','user_id':'e270ca05-b107-47c7-b7c4-24612fdf94c8'}
       {"type":"ApprovalResolved","decision":{"type":"Approve"}}
11 {'type':'System'}  RunStateChanged -> Running
12 {'type':'Agent',…} ToolStarted shell.run  label:"ls -la"
13 {'type':'Agent',…} ToolCompleted shell.run Succeeded
```

`approvals.resolved_by = 'e270ca05-b107-47c7-b7c4-24612fdf94c8'` — a UUID the
attacker chose. The human-approval gate, the product's central safety property,
is satisfied by any process that can open the socket. Containment today is
entirely `run_dir` mode `0700` (`crates/protocol/src/discovery.rs:190`) — which
F-19-6 shows is itself fragile.

### F-19-2 — `sessions.workspace_id` is never written; the workspace scope is a client-side fiction. Class (a)
`migrations/0001_init.sql:17` declares it. `CommandBody::CreateSession` carries
`workspace: WorkspaceId` (`crates/protocol/src/command.rs:98`) and the CLI sends
a real one from its own local store (`crates/cli/src/tui.rs:5764-5786`). The
daemon's handler `apply_create_session`
(`crates/daemon/src/commands.rs:427-466`) never reads it, and the INSERT at
`crates/daemon/src/ledger.rs:19-22` lists six columns without it. Confirmed in
the dump: `'workspace_id': None`.

Meanwhile the TUI builds its knowledge queries as
`[Scope::System, Scope::Workspace(workspace_id), Scope::Repository(repo)]`
(`crates/cli/src/tui.rs:5906-5910`, `:2766-2770`) using a `WorkspaceId` that
lives only in the *client's* `StoredSession` file. Two clients on the same repo
mint different workspace ids and see different, mutually invisible
workspace-scoped memories/documents — with no error. For outcome 19, "the
existing workspace scope" does not exist server-side.

### F-19-3 — the organization scope is asserted by the caller, not derived. Class (a) / TRUST-BOUNDARY READ
`crates/codypendentd/src/docs_job.rs:134-152` parses `CreateDocument`'s wire
`scope` string; `"organization:<uuid>"` becomes `Scope::Organization(id)`
directly. There is no membership table, no organization table, no check. The
document is then stored with `scope_tier='organization'`, `scope_key=<uuid>`,
and `crates/knowledge/src/docs/collab.rs:56` grants it Suggest-mode
collaboration semantics on that basis. Any caller can place a document in any
organization by typing a UUID. The same applies to
`crates/daemon/src/blackboard.rs` `BoardTarget::Repository(String)` — the
repository is a caller-supplied path string.

### F-19-4 — presence is published and consumed, but there is no roster and it does not survive a snapshot catch-up. Class (c)
Presence **works** at the transport layer — this is the healthiest part of the
multi-user surface. `crates/daemon/src/server.rs:4449-4466` (`publish_presence`)
appends a durable `ClientPresenceChanged` and fans it out; called on attach
(`:4439`) and on disconnect (`:751`). **Reproduced:** client A, attached, saw
`{"type":"ClientPresenceChanged","client_id":"5ff1c5ed-…","role":{"type":"Approver"},"present":true}`
the moment client B attached.

What is missing for outcome 19:

* **No roster.** The only consumer, `crates/tui/src/reduce.rs:1933-1951`, sets a
  transient status notice for 10 ticks (`"client 5ff1c5ed joined (approver)"`)
  and — note — *skips it entirely if `state.notice` is already occupied*. There
  is no "who is here" list in `AppState`.
* **Lost on snapshot catch-up.** `SessionProjection`
  (`crates/protocol/src/catchup.rs:41-53`) has `session_id, title,
  last_sequence, active_runs, pending_approvals, closed`. No presence field. A
  client >500 events behind gets `Catchup::Snapshot`
  (`crates/daemon/src/server.rs:4362`) and has no way to learn who else is
  attached. It also gets `active_runs: Vec<RunId>` — bare ids, no objective, no
  state, no model.
* **The extension shows nothing useful.** `extensions/vscode/src/extension.ts`
  has no `case "ClientPresenceChanged"`; it falls to `default:` at `:606` and
  posts `{label: "ClientPresenceChanged", detail: ""}`.
* **The role in the event is the self-declared one** (F-19-1), so a presence
  roster built on it would display attacker-chosen authority labels.

### F-19-5 — list-vs-by-id gate audit
The brief's central question. The answer is that the product has **two read
surfaces with opposite discipline**, for the same resources.

| Resource | Remote-UI plugin projection (by id) | Wire command (by id) | Live subscription (by id) |
|---|---|---|---|
| session | `authorize_session_resource` `server.rs:3697` — must equal broker session | `ReadSessionEvents` `server.rs:2267` — **no gate** | attach: existence only |
| run | `run_session(run_id) != Some(session_id)` → bail `server.rs:3536` | — | — |
| workflow run | `authorize_workflow_resource` `server.rs:3707` — SQL join to owning session, deny-first | `ReadWorkflowRun` `server.rs:2300` — **no gate** (comment: *"carries no role gate"*) | `Subscription::Workflow` `server.rs:4408` — **no gate** |
| blackboard | same helper (`"blackboard"` shares `"workflow"` authorization) | `ReadBlackboard` `server.rs:2144` — **no gate** | `Subscription::Blackboard` `server.rs:4400` — **no gate** |
| document | — | `MutateDocument`/`AcquireDocumentLease`/`PublishDocument` — no ownership gate | `Subscription::Document` `server.rs:4392` — **no gate** |
| artifact | `read_remote_ui_artifact` `server.rs:3735` — joins artifact→run→session | none exists | — |

The mismatch is real and asymmetric: the *plugin* surface — the least trusted
caller in the design — re-derives ownership server-side on every by-id read.
The *client* surface, which is where a second human would actually connect,
re-derives nothing.

`crates/daemon/src/documents.rs:65` (`DocumentHub::subscribe`) and
`crates/daemon/src/blackboard.rs:81` (`BlackboardHub::subscribe`) take a bare id
and hand back a live receiver. `handle_attach`
(`crates/daemon/src/server.rs:4387-4419`) calls them straight from the
client-supplied `subscriptions` vector with no check whatsoever.

**Proved on the wire:** a never-attached client read all 17 events of another
client's session, including the full context manifest (repository map, symbol
names, retrieved memories) and the model's output:
```
T1 ReadSessionEvents(other session, NEVER attached): SessionEventsPage
   events: 17  through: 17
   NOTE LEAK: "=== CONTEXT: EVIDENCE, NOT INSTRUCTIONS ===\nThe material below is …"
```

### F-19-6 — starting the daemon chmods the socket's parent directory to 0700. Class (c) — collateral damage
`crates/protocol/src/discovery.rs:136-138` derives
`run_dir = socket_path.parent()`. `ensure_directories()` (`:184-192`) then
`chmod 0700`s it, and `crates/codypendentd/src/main.rs:17` calls that on every
boot. With `CODYPENDENT_SOCKET` pointing anywhere outside the data dir, the
daemon silently re-permissions a directory it does not own.

The product's own error text invites exactly this
(`crates/protocol/src/discovery.rs:70-75`): *"Set CODYPENDENT_SOCKET to a
shorter path (for example under /tmp)"*.

**Reproduced twice.** First accidentally: `CODYPENDENT_SOCKET=/tmp/cdp-review.sock`
turned `/tmp` from `drwxrwxrwt` into `drwx------` (I restored it). Then
deliberately, in an isolated directory:
```
before: drwxrwxrwt 2 root root  /home/user/.cdp-review-tmp/sockparent
after:  drwx------ 2 root root  /home/user/.cdp-review-tmp/sockparent
```
For outcome 19 this matters twice over: the 0700 directory *is* the entire
current trust boundary (F-19-1), and it is applied to the wrong directory the
moment an operator follows the daemon's own advice. Note also that the socket
inode itself is left at the process umask — `srwxr-xr-x` in my runs; nothing
chmods it to 0600.

### F-19-7 — "not allowed" and "does not exist" are distinguishable on the artifact path. Class (c)
`role_permits` is correctly checked before existence
(`crates/daemon/src/commands.rs:364-373`, with an explicit comment), and
`authorize_workflow_resource` is deny-first for both the missing-row and
wrong-owner cases (`server.rs:3718-3721`). Good.

The artifact reader is not. `crates/daemon/src/server.rs:3741-3750`:
* unknown artifact id → `Ok((true, Null))` — a clean "removed" projection;
* known artifact owned by another session → `bail!("artifact resource does not
  belong to the broker session")`.

A plugin can therefore enumerate which artifact UUIDs exist across the whole
daemon by observing which of the two responses it gets. Narrow (UUIDv7 ids are
not guessable in bulk) but it is a real oracle, and it is the pattern that must
not be copied when the other by-id paths get their gates.

### F-19-8 — role coverage is incomplete and inconsistent between the two dispatch paths
`role_permits` (`crates/daemon/src/commands.rs:1152-1167`) covers seven command
bodies and returns `false` for everything else — but the read/connection-level
commands (`AttachSession`, `ReadSessionEvents`, `ReadBlackboard`,
`ReadWorkflowRun`, `PutArtifact`, `UpdateIdeContext`, all UI-plugin lifecycle
commands, all document commands, all workflow commands, all promotion commands)
never reach it: they are intercepted in `handle_payload`
(`crates/daemon/src/server.rs:1240-2400`) with hand-rolled, per-arm `if
conn.role == ClientRole::Observer` / `!= Controller` checks — 14 distinct call
sites I counted. Two authorization tables that must agree and have no shared
source. `ReadSessionEvents` has neither.

---

## What I could not exercise, and why

* **A second OS user.** The container runs everything as root, and the design
  serves exactly one user (`LOCAL_USER_KEY = "local"`). I substituted a second
  *socket client* with an independent, self-chosen `client_id`, which is the
  same trust boundary the daemon actually enforces at — arguably a stronger
  test, since it needs no OS-level privilege at all.
* **`BudgetWarning{Tokens}` end-to-end.** My stub model does not advertise a
  context window, and `token_budget_event` is only called when
  `driver.context_window()` is `Some` (`crates/runtime/src/agent.rs:2094` `let context_window = driver.context_window();`, contract at `:415-421`).
  I verified the emitter/consumer wiring by reading; I did not see the ctx%
  chip render. The `Cost` half (F-20-4) needs no run to prove — it has no
  emitter at all.
* **The Remote-UI plugin projection path at runtime.** `bwrap` is unavailable in
  this container (`sandbox tool 'bwrap' is unavailable … refusing to run
  unconfined`), so component workers fail closed and I could not drive a real
  plugin through `read_remote_ui_projection`. The authorization helpers and the
  always-null `UiRunProjection` fields were verified by reading, and the DB dump
  independently proves the columns they read are never written.
* **Documents / CRDT sync.** `documents` is empty in every run — the doc
  commands need the assembly's document seams and a `CreateDocument` flow I did
  not drive. The subscription gap (F-19-5) is a code fact at
  `crates/daemon/src/server.rs:4392`, not a behaviour I reproduced.
* **Migrations 0020 and 0021** do not exist — the sequence jumps 0019 → 0022.
  Harmless (`sqlx` orders by the numeric prefix and all 22 applied cleanly:
  `_sqlx_migrations` has 22 rows), but worth knowing before anyone assumes
  contiguity.

---

## The one structural pattern

**Every number and every identity in this system is carried across a seam as an
opaque reference, and the reference is where the trail ends.**

The chronicle is an `ArtifactRef` and no command fetches artifacts. `RunOutcome`
is a return value and the caller writes `.map(|_| ())`. `started_at`/`ended_at`
are columns that only a `SELECT` knows about. `workspace_id` is a wire field the
handler never destructures. `client_id` is an envelope field nothing verifies.
`requested_role` is a command field assigned straight to connection state.
`PolicyReason.code` is a struct field that never survives the `.map()` to the
event.

In each case the *shape* is right — the field exists, is named well, is
documented, and is usually unit-tested against a hand-built value. What is
missing is the single line that carries the value across the last seam. That is
why outcome 20 reads as "the ledger has no reader" and outcome 19 reads as "no
multi-user": both are the same defect, repeated, at the final hop. It is also
why both are cheap: F-20-2 is one `.map(|_| ())`; F-20-5 is two columns in one
INSERT; F-20-3(a) is one missing match arm; F-19-2 is one unused function
parameter. The expensive one is F-19-1, which is genuinely absent — but it is
absent *cleanly*, at exactly three lines, and every downstream `UserId` is
already derived from that one value.
