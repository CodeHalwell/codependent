# daemon-core — round 4 review report

Reviewer: **daemon-core**. Vertical: `crates/daemon/**` (esp. `server.rs`),
`crates/protocol/**`, `migrations/0031_multi_user.sql` (+ `0033_workflow_run_owner.sql`),
threat model `.impl/threat-models/19-multi-user.md`.
Owned outcome: **19 — Real multi-user**.
Pinned commit `c255bec8b175d62942b3312cff2335b97d43a59a` (v0.5.1). No code changed.

The task was explicit: **re-run the prior round's on-the-wire attacks against the
current build and report what happens now — do not take the fix on trust.** I did.
Everything below was driven against a live `target/debug/codypendentd` over its Unix
socket with a raw length-prefixed-JSON client, with **two genuinely different OS
principals** (root = uid 0, and a second user `attacker` = uid 2000). The daemon's
gate is uid-based (`SO_PEERCRED`), so a same-uid second client is *by design* the
owner; the only sound adversary test is a different uid, which I set up.

---

## Verdict

**OUTCOME 19: PARTIAL.** The trust boundary the prior round found wide open is now
genuinely enforced, server-side, from the transport — I verified cross-uid on the
wire that a foreign principal can read nothing, resolve nothing, cancel nothing,
and that "not allowed" is byte-identical to "does not exist" on every path the
prior review broke. That is the heart of the outcome and it holds. It is **not
WORKING** because (a) one cross-user write surface — `PublishDocument` — still does
**not** re-derive ownership, which both bypasses the gate cross-uid and re-opens the
exact enumeration oracle (F-19-7) the threat model swears never to repeat; and
(b) the outcome's stated "organization and workspace scopes" are not an enforcement
axis at all — the only dimension the daemon gates on is the OS uid; the `workspace`
a client sends on `CreateSession` is silently discarded.

---

## Environment

```
stub OpenAI-compatible model  ->  127.0.0.1:8931  (SSE streaming, scripted tool calls)
daemon    STUB_KEY=sk-stub CODYPENDENT_DATA_DIR=/tmp/review-daemon-core/dc \
          CODYPENDENT_SOCKET=/tmp/review-daemon-core/d.sock \
          target/debug/codypendentd
clients   raw AF_UNIX + length-prefixed JSON (crates/protocol/src/framing.rs)
```

A note on reachability that matters for how you weight everything below. The socket
lives at a path I chose, whose parent dir I created, so the daemon left it `0755`
and the socket file's own mode (`srwxr-xr-x`, umask 022) *by itself* refuses a
non-owner `connect(2)` — the incidental outer wall the threat model calls F-19-6. To
test the **daemon's own gate** (defense-in-depth *behind* that wall — which the
threat model explicitly commits to: *"this work makes the daemon stop being
additionally wide open behind that wall"*), I `chmod 777`'d the socket so uid 2000
could reach it. In a normal deployment the socket sits under `<data_dir>/run/`, which
the daemon chmods `0700`, so a different uid cannot connect at all. **Every cross-uid
finding below is therefore defense-in-depth tier** — it bites only if the outer wall
is bypassed — but it is measured against the vertical's own unconditional Invariant 3.

---

## What I could NOT break (the repairs are real)

Driven as **uid 2000** against **uid 0 (A)**'s live resources
(`session_id=019ffd58-83d0…`, `document_id=019ffd58-83dc…`,
`workflow_run_id=wfrun-93cd4e2c…`, `approval_id=019ffd59-f82c…` pending,
`run_id=019ffd59-f830…`, a real `memory_id`, board `board:/home/user/codypendent`).
Full transcript: `/tmp/review-daemon-core/EVIDENCE-attacker.txt`. Each line pairs the
attack on A's **real** id with the same command on a **random** id; identical replies
mean no oracle.

```
-- READS (by id) --
  ReadSessionEvents(A session)       -> REJECTED protocol.session-not-found  no session 019ffd58-83d0…
  ReadSessionEvents(random session)  -> REJECTED protocol.session-not-found  no session 099ffd58-0000…
  ReadWorkflowRun(A wf)              -> REJECTED workflow.run-not-found       no workflow run wfrun-93cd…
  ReadWorkflowRun(random wf)         -> REJECTED workflow.run-not-found       no workflow run wfrun-0000…
  ReadBlackboard(A wf)               -> REJECTED workflow.run-not-found
  ReadBlackboard(A repo board)       -> REJECTED workflow.run-not-found       no workflow run board:/home/user/codypendent
  ReadBlackboard(random wf)          -> REJECTED workflow.run-not-found
  InspectMemory(A mem)               -> REJECTED memory.transport-unavailable
  InspectMemory(random mem)          -> REJECTED memory.transport-unavailable   (uniform — no oracle)
  OpenMemoryEvidence(A mem)          -> REJECTED memory.transport-unavailable
-- APPROVAL GATE (the prior round's arbitrary-code-exec bypass) --
  ResolveApproval(A approval) APPROVE-> REJECTED approval.not-found          no approval 019ffd59-f82c…
  ResolveApproval(random approval)   -> REJECTED approval.not-found          no approval 099ffd58-0000…
-- WRITES / LIFECYCLE (by id) --
  CancelRun(A run)                   -> REJECTED protocol.run-not-found
  CancelRun(random run)              -> REJECTED protocol.run-not-found
  SubmitUserInput(A session)         -> REJECTED protocol.session-not-found
  StartRun(A session)                -> REJECTED protocol.session-not-found
  QueueSteering(A run)               -> REJECTED protocol.run-not-found
  CancelWorkflow(A wf)               -> REJECTED workflow.run-not-found
  PauseWorkflow(A wf)                -> REJECTED workflow.run-not-found
  PostBlackboardItem(A repo board)   -> REJECTED workflow.run-not-found      no workflow run board:/home/user/codypendent
  PostBlackboardItem(A wf board)     -> REJECTED workflow.run-not-found
  MutateDocument(A doc)              -> REJECTED document.not-found           no document 019ffd58-83dc…
  ForgetMemory(A mem)                -> REJECTED memory.transport-unavailable
  UpdateIdeContext(A session)        -> REJECTED protocol.session-not-found
-- ATTACH (subscribe) --
  AttachSession(A session) Approver  -> ERROR    protocol.session-not-found
  AttachSession(random session)      -> ERROR    protocol.session-not-found
-- ROLE / IDENTITY ASSERTION --
  own CreateSession                  -> ACCEPTED  (own_sid stamped owner_uid=2000, see below)
```

**Every one of the prior round's four demonstrated breaks is closed:**

1. **There is a principal now.** `crates/daemon/src/principal.rs` reads `SO_PEERCRED`
   off the stream at accept time (`server.rs:740`, before any frame is read).
   Confirmed with a DB query: the session uid 2000 created for itself is stamped
   `owner_uid = 2000`, not any client-chosen value:
   ```
   sqlite> select id,title,owner_uid from sessions where title like '%2000%';
     019ffd5a-e20e-…  probe-2000 own  2000
   ```
   `CreateSession` stamps `owner_uid = ctx.principal.uid()` (`commands.rs:468`).

2. **`ReadSessionEvents` is gated** (`server.rs:2725`, `principal_may_use_session`).
   uid 2000 reading A's session → `protocol.session-not-found`, identical to a
   random id. (For contrast, a **same-uid** second root client *does* read it —
   correct: same uid is the owner by design, threat model §4.)

3. **The approval bypass is closed.** `ResolveApproval` resolves
   `approval → run → session → owner_uid` (`authorize_command`, `server.rs:4456-4470`)
   and refuses a mismatch with `approval.not-found`. uid 2000 could not approve A's
   pending `shell.run` gate.

4. **Identity cannot be asserted.** Separately tested
   (`/tmp/review-daemon-core`, final probe):
   * uid 2000 setting its envelope `client_id` **to A's** → still
     `session-not-found` on A's session.
   * uid 2000 presenting a **valid resume token** (restores `client_id`) → still
     `session-not-found`. A stolen resume token grants nothing, exactly as the
     threat model claims (`server.rs:1285-1297`).
   * `ClientRole` still only narrows: B (same uid) attached as `Approver`, then
     `CancelRun` → `protocol.role-denied "role Approver may not issue this command"`.

The plugin/Remote-UI mediated-command path — the prior round's F-19-8, where the
plugin path re-derived ownership and the wire path did not — now derives its
principal from the **stored** `owner_uid` too (`server.rs:4705`
`PeerPrincipal::from_uid(owner_uid)` via `ensure_remote_ui_command_session`), so the
two paths are consistent. (I could not exercise this live — the worker runtime is
unavailable without `bwrap` in this sandbox; this item is **read, not run**.)

The memory store fails **closed and uniform** for a non-daemon uid
(`memory_seam`, `server.rs:5596`): every memory verb returns
`memory.transport-unavailable` regardless of whether the id exists — no oracle, and
`ForgetMemoryScope { tier: User|System }` cannot erase a shared scope from a foreign
uid. Good.

---

## Findings

### F-19-A — `PublishDocument` does not re-derive ownership: cross-uid gate bypass **and** an enumeration oracle. Class (c). TOP FINDING.

`server.rs:1756-1815` handles `PublishDocument`. It checks the `Controller` role and
the publisher transport — and then **builds and runs the publish** with no ownership
check. Every sibling document command guards first:

```
server.rs:1570   MutateDocument        -> reject_unowned_document(...)   ✓
server.rs:1681   AcquireDocumentLease  -> reject_unowned_document(...)   ✓
server.rs:1756   PublishDocument       ->  (none)                        ✗
```

Proved on the wire as **uid 2000 against A's (uid 0) document**:

```
PublishDocument(A real doc 019ffd58-83dc…) -> DocumentPublishRequested
    approval_id=019ffd5b-9aa6…  target="repository file docs/o1.md"
    changed_files=["docs/o1.md"]  git_action="write docs/o1.md in the working tree…"
PublishDocument(random doc 099ffd58-…def)  -> CommandRejected document.not-found
                                              "no document 099ffd58-…def"
```

Two defects on one command:

* **Ownership-gate bypass (Invariant 3 violation).** Invariant 3 of the threat model:
  *"Every `CommandBody` that names a resource by id … resolves that id to an owner in
  the daemon's own storage and compares it to `ConnState.principal`."* `PublishDocument`
  names `document_id` and does not. A foreign principal gets the daemon to compile and
  **park a publish plan for a document it does not own.**
* **Enumeration oracle (repeats F-19-7).** Real doc → `DocumentPublishRequested`;
  absent doc → `document.not-found`. The threat model, §5, promises the opposite:
  *"Specifically not done: a distinct not-authorized code … it would be an enumeration
  oracle, which is the mistake F-19-7 records."* Here the *success reply itself* is the
  oracle — a foreign principal can probe which document ids exist on another user.

**Impact ceiling (measured, so I state it honestly).** The parked approval binds to a
**synthetic** session the publisher mints (`crates/codypendentd/src/publish.rs:924`
`mint_publish_run` → `ledger::create_session`, which does **not** set `owner_uid`), so
its `owner_uid` is NULL → resolved to the **daemon uid** (`server.rs:4197`
`session_owner_uid`). Confirmed in the DB — three pending `"docs publish: Runbook"`
approvals on NULL-owner sessions, injected by the cross-uid probe:
```
sqlite> select a.id,s.owner_uid,s.title from approvals a join runs r on a.run_id=r.id
        join sessions s on r.session_id=s.id where s.owner_uid is null;
  019ffd5e-3655…  NULL  docs publish: Runbook
  019ffd5b-9aa6…  NULL  docs publish: Runbook
```
Because the approval resolves to the daemon uid, the attacker **cannot resolve its own
parked approval** (verified: uid 2000 `ResolveApproval` on it → `approval.not-found`).
So this is not a self-service arbitrary-write. What it **is**: (1) an existence oracle
over another user's documents; (2) a **confused-deputy / approval-queue injection** —
the publish lands in the **owning** user's approval rail, described by a `git_action`
that writes to a path **the attacker chose**, so a human who approves from their queue
executes the attacker's write; (3) an unbounded way to spam synthetic
sessions/runs/approvals into the owner's ledger. The fix is one line: the
`reject_unowned_document(state, conn, writer, &request, *document_id)` its siblings
already call.

### F-19-B — `ReleaseDocumentLease` releases by `lease_id` with no holder check. Class (c). Secondary.

`server.rs:1704` (`ReleaseDocumentLease`) gates on role + transport but, having only a
`lease_id`, calls no `reject_unowned_document`. The seam it delegates to,
`crates/knowledge/src/docs/leases.rs:178`, is:
```rust
pub async fn release(&self, pool, lease_id) -> ... {
    "UPDATE document_leases SET state='released' WHERE id = ? AND state='active'"
```
No holder/uid/`client_id` predicate. **Inferred from reading (not driven):** any peer
that learns a `lease_id` can release another writer's edit lease, breaking the
single-writer guarantee. Practical severity is low — `lease_id` is a random UUID, and
`MutateDocument` is *separately* owner-gated, so a foreign uid still cannot write the
document even after stealing its lease — but it is the same "acts on a caller-supplied
id without re-deriving from storage" shape as F-19-A and inconsistent with the other
lease command (`AcquireDocumentLease`, which *is* owner-gated at `server.rs:1681`).

### F-19-C — Outcome 19's "organization and workspace scopes" are not enforcement axes; only the OS uid is. Class (a) for the org/workspace dimension.

Outcome 19 reads: *"Presence, shared boards, and review rails across users within the
existing **organization and workspace** scopes."* The implemented model is purely
OS-uid ownership. Evidence:

* `CreateSession { workspace: WorkspaceId, … }` — the `workspace` field is
  **destructured and discarded**; the apply path (`commands.rs:455-470`) persists
  `owner_uid` and never reads `workspace`. `sessions` has no `workspace_id` the gate
  consults. So "within the workspace scope" is not a thing the server enforces.
* Organization scope for documents/memories is a knowledge-crate `scope_tier` the
  **caller** asserts (`docs_job` parse path); the threat model §6 records this as
  deliberately **out of scope** (F-19-3, F-19-2). That is a defensible reduction — but
  it means the outcome's own words overclaim what exists.

The honest description of what shipped: **per-OS-user isolation on a single local
daemon**, re-derived server-side from `SO_PEERCRED`. Since the socket is single-user
(`0700` dir), in the normal deployment there is exactly one uid, so "multi-user"
collapses to "multi-**client**, one user" (TUI + CLI + VS Code all share everything,
correctly), with cross-uid isolation as defense-in-depth for the abnormal case.

---

## Outcome-19 content: presence, shared boards, review rails

All three exist and are uid-scoped (re-derived server-side), which is the security
property that matters:

* **Presence.** `ClientPresenceChanged` is appended and fanned out to a session's
  subscribers (`server.rs:5341` `publish_presence`). It reaches only clients that
  passed the attach gate, i.e. same-uid owners. Observed: same-uid client B's
  arrival/departure reached A; cross-uid B cannot attach, so no presence leaks.
* **Shared boards.** The repository task board (`board:<canonical repo>`) is
  daemon-uid-owned (`principal_may_read_workflow`, `server.rs:4316-4328`), shared among
  same-uid clients, refused cross-uid (both read and write returned
  `workflow.run-not-found` for uid 2000). Scope re-derived server-side from the id and
  the daemon uid — **not** from anything the caller sent.
* **Review rails.** Approvals (`ResolveApproval`, owner-gated), document
  suggestion accept/reject (`MutateDocument::{Accept,Reject}Suggestion`, role + owner
  gated, `server.rs:1581-1607`), and promotion approve/rollback (daemon-uid gated,
  `server.rs:2198-2324`) are all present and gated.

Migrations backing this: `0031_multi_user.sql` (`sessions.owner_uid`) and
`0033_workflow_run_owner.sql` (`workflow_runs.owner_uid`) — both verified present;
boot-time adoption of NULL owners to the daemon uid is at `server.rs:520` and `:537`.

---

## The pattern

The prior round's holes were closed by adding **one gate helper per resource kind**
(`principal_may_use_session`, `principal_may_read_workflow`, `principal_may_read_document`,
`reject_unowned_*`, `memory_seam`, `authorize_command`) and threading it through the
command dispatch — 51 references to those helpers / `owner_uid` / `daemon_uid` in
`server.rs`. The gate itself is sound and the common paths are covered; the
`SO_PEERCRED` principal is the right primitive and identity genuinely cannot be
asserted from the wire. **The residual defects are the arms the retrofit *missed***:
`PublishDocument` and `ReleaseDocumentLease` were intercepted at the connection level
*before* this round's work and never got the `reject_unowned_*` line their siblings
got. It is the same shape as the original F-19-8 — a per-site gate that has to be
*remembered at every site*, and was forgotten at two of them. The threat model's own
comment on `reject_unowned_workflow` names the risk exactly: *"These helpers exist so
the check is one call at each site rather than a pattern each site is trusted to
remember."* Two sites did not make the call. A structural fix (gate in one dispatch
choke point keyed by "does this body name a pre-existing resource id") would make the
class un-missable; the current per-arm discipline will keep leaking one arm at a time.

## What I did NOT verify (read, not run)

* The **Remote-UI plugin projection/mediation** ownership path (`server.rs:4680-4795`,
  `read_remote_ui_artifact` `:4555`) — I read that it derives its principal from stored
  `owner_uid`, but the worker runtime is unavailable here (`bwrap` missing: daemon logs
  *"Remote UI worker runtime unavailable; component workers fail closed"*), so no plugin
  ran. **Inferred** consistent; not exercised.
* **F-19-B (`ReleaseDocumentLease`)** — the missing holder check is read from
  `leases.rs:178`; I did not drive a stolen-lease-id release on the wire.
* The **same-uid** collaboration surfaces (presence fan-out, shared board writes,
  suggestion resolve) I exercised only enough to confirm they *work* for the owner;
  I did not audit their correctness beyond the ownership gate.
* **Outcome 20** ("the ledger made visible") — the prior round's report covered it, but
  this task assigned me outcome 19 only, so I did not re-audit 20. Its prior findings
  (F-20-*) are neither confirmed nor refuted here.
* I did **not** run `cargo build`/`test` (disk constraint); I used the orchestrator's
  `target/debug/codypendentd` and `codypendent` binaries and drove the wire directly.
