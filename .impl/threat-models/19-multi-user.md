# Threat model — outcome 19, real multi-user

Author: agent-security. Written **before** the first line of code, per BRIEF rule 4.
Scope: `crates/daemon/src/server.rs`, `crates/protocol/**`, plus the minimum
supporting change in `commands.rs` / `approvals.rs` / `ledger.rs` / `projections.rs`
(unowned files) needed to make the gate real.

Baseline evidence: `docs/reviews/2026-08-13-verticals/daemon-core.md` findings
F-19-1, F-19-5, F-19-7, F-19-8 — reproduced on the wire against a live daemon.

---

## 1. The boundary

There is exactly one untrusted ingress in this vertical: **the Unix domain
socket** at `RuntimePaths::socket_path`. Everything that arrives on it is
attacker-controlled bytes. The daemon is a local, long-lived, privileged-ish
process (it spawns shells, reads the filesystem, holds provider credentials) and
the socket is the only way to make it do any of that.

```
  any local process ──connect(2)──► /run/.../codypendentd.sock ──► handle_connection
        │                                                              │
        └── controls: every byte of every frame ─────────────────────────┘
            does NOT control: its own uid/gid/pid as seen by the kernel
```

The last line is the whole design. `SO_PEERCRED` is filled in **by the kernel at
`connect(2)` time** from the connecting process's credentials. A client cannot
set it, cannot spoof it, and cannot change it after the fact. It is the only
fact about the peer that is not attacker-controlled, and it is therefore the
only sound basis for a principal on this transport.

## 2. What an attacker controls

Assume a hostile process running on the same machine that can `connect(2)` to the
socket. It controls, in full:

| Field | Where | Today's effect |
|---|---|---|
| `Envelope.client_id` | `crates/protocol/src/envelope.rs:31` | **becomes the identity** (`server.rs:1195`) |
| `ClientHello.*` | `handshake.rs:25` | free text; carries no user field at all |
| `AttachSession.requested_role` | `command.rs` | **becomes the connection's authority** (`server.rs:1275`) |
| `AttachSession.session_id` | idem | any session id it can name or guess |
| `ReadSessionEvents.session_id` | idem | **ungated** (`server.rs:2267`) |
| `ReadBlackboard.workflow_run_id` | idem | **ungated** (`server.rs:2144`) |
| `ReadWorkflowRun.workflow_run_id` | idem | **ungated** (`server.rs:2300`) |
| `Subscription::{Document,Blackboard,Workflow}` ids | idem | **ungated** (`server.rs:4392/4400/4408`) |
| `ResolveApproval.approval_id` | idem | **ungated — this is the bypass** |

It does **not** control: its uid, gid, pid as reported by `SO_PEERCRED`; the
daemon's HMAC secret; the contents of the SQLite ledger except through the
commands above.

## 3. What is at stake

Not "data confidentiality" in the abstract. Concretely:

1. **Arbitrary code execution.** `ResolveApproval` is the human-in-the-loop gate
   in front of `shell.run`. The review parked an `ls -la`, resolved it from an
   unrelated, never-attached socket client, and the daemon executed it. Any
   process that can open the socket can approve any parked command. The
   approval gate is the product's central safety property and it currently
   holds against nothing.
2. **Whole-session disclosure.** `ReadSessionEvents` with no gate returns the
   full transcript — prompts, model output, the context manifest (repo map,
   symbol names, retrieved memories), tool arguments and results. The review
   pulled 17 events out of a session the client had never attached to.
3. **Enumeration.** `read_remote_ui_artifact` (`server.rs:3741`) answers
   "unknown id" and "someone else's id" *differently* (F-19-7), which turns any
   by-id reader into an existence oracle. Any gate I add must not repeat this.

## 4. Trust model I am adopting

This is a local-first, single-user daemon. The proportionate model is:

> **The principal is the OS user on the other end of the socket, as reported by
> the kernel. A resource is readable/controllable by the principal that created
> it. Nothing else confers identity or authority.**

Deliberately **not** built: a user database, passwords, tokens-as-credentials,
groups, ACLs, delegation, or an org/tenant model. Those are the wrong shape for
a single-user local daemon and the review is explicit that inventing one is out
of scope. The defect being fixed is not "the ACL model is too coarse" — it is
that *the daemon accepts anything at all*.

### Deny by default

* A connection with **no derivable peer credential** gets no principal and is
  refused at handshake. Failing closed is correct: on Linux `SO_PEERCRED` cannot
  legitimately fail for a connected `AF_UNIX` socket, so a failure means
  something is wrong that I do not understand, and I will not guess.
* A session with **no recorded owner** (rows predating migration 0031) is
  readable only by the daemon's own uid — the uid that necessarily created them
  in the single-user world those rows come from. It is not readable by everyone.
* Every by-id read and every subscription: **deny unless the server can prove
  ownership from server-stored state.** Not from anything in the request.

### Deliberately allowed, and why

* **Same-uid peers are the owner.** A second process running as the same OS user
  is, on a Unix system, already able to read the daemon's SQLite file, ptrace the
  daemon, and rewrite its config. Refusing it at the socket would buy nothing and
  break every legitimate client (TUI, CLI, VS Code extension, the executor's own
  loopback connection). Same-uid is *legitimately* the owner. This is the
  proportionate line and it is where I draw it.
* **`client_id` survives as a correlation token.** Reconnect, presence, event
  attribution and idempotency all key off it and all of that is fine —
  correlation is not authority. It stays in the envelope; it stops being
  identity. `UserId` is derived from the peer uid, never from it.
* **root (uid 0) is not special-cased.** A root peer is a *different* principal
  from uid 1000 and is refused another uid's sessions like anyone else. Root can
  trivially bypass this by other means; the gate is not a containment boundary
  against root, and pretending otherwise would be theatre. But it also must not
  *hand* root the data, so no exemption is coded.
* **`ClientRole` may still only narrow.** A client asserting `Approver` gains
  nothing it did not already have as the owning uid; a client asserting
  `Observer` genuinely gives something up. So the assertion is kept (it is a
  useful self-restriction) and is simply no longer the thing that authorises
  anything. Authority comes from the uid; the role can only subtract.

## 5. Failure mode discipline (BRIEF rule 2)

Every gate added here returns **one** error for both "you may not" and "it does
not exist":

```
protocol.session-not-found   "no session <id>"
```

— i.e. the *existing* not-found error, byte for byte, with the id echoed back
(the caller already supplied it, so echoing it leaks nothing). Ownership is
checked in the same query that checks existence, so there is no timing skew to
observe either. `authorize_workflow_resource` (`server.rs:3707`) already does
exactly this and is the pattern I mirror.

Specifically **not** done: a distinct `protocol.not-authorized` code. It would
be friendlier and it would be an enumeration oracle, which is the mistake
F-19-7 records.

For a resource whose "does not exist" answer is *not* an error, the refusal is
that answer instead — see §8 on `ReleaseDocumentLease`. For the daemon-wide
stores (memory, promotion, Remote UI plugins) the refusal reuses each store's own
"not enabled on this daemon" error verbatim, so a foreign principal cannot
distinguish "not yours" from "not built into this daemon".

## 6. Residual risk — stated plainly

* **Not a containment boundary against the same uid.** By construction. Anything
  running as you can already be you. The socket's `0700` parent directory
  (F-19-6) remains the outer wall for cross-user access; this work makes the
  daemon stop being *additionally* wide open behind that wall, and makes the
  boundary explicit and enforced rather than incidental.
* **PID is recorded, never trusted.** `peer_cred().pid()` is racy (the process
  can exit and the pid be recycled). It is logged for diagnostics and stamped
  nowhere that matters.
* **`Organization` scope (F-19-3) is out of scope here.** It is asserted by the
  caller in `docs_job.rs` and stays that way; it is a knowledge-crate seam, not
  a server.rs one. Recorded, not fixed.
* **`sessions.workspace_id` (F-19-2) is out of scope here** for the same reason —
  fixing it is a `commands.rs` change with no security content once ownership is
  enforced at the uid level.
* **Resume tokens remain non-credentials.** A resume token restores a
  `client_id` (correlation) only. It cannot elevate a uid, and after this change
  a stolen resume token grants nothing, because `client_id` grants nothing.

## 7. Invariants the implementation must uphold

1. `ConnState.principal` is set from the transport at accept time, before any
   frame is read, and is never written again from any frame.
2. No `UserId` anywhere in `server.rs`/`approvals.rs` is constructed from
   `client_id`.
3. Every `CommandBody` that names a resource by id, and every `Subscription`
   that names one, resolves that id to an owner **in the daemon's own storage**
   and compares it to `ConnState.principal`.
4. Every such comparison fails with the same error as "does not exist".
5. `ResolveApproval` resolves `approval_id → run → session → owner_uid` and
   refuses a mismatch. (Invariant 3 with the highest stakes.)

## 8. How invariant 3 is enforced (round 4) — one choke point, not a habit

The first implementation enforced invariant 3 **per command arm**, and the round-4
review found what that always finds: the arm that was missed. `PublishDocument`
checked the role and the transport and then built the publish, while both of its
siblings (`MutateDocument`, `AcquireDocumentLease`) re-derived ownership — so a
foreign uid could park a Git write, to a path it chose, into the owning user's
approval queue, and the difference between that success and a `document.not-found`
was itself the enumeration oracle §5 forbids. `ReleaseDocumentLease` was missed the
same way.

There is now exactly **one** gate, and it is fed by a classifier that the compiler
will not let anyone forget:

* `CommandBody::named_resources()` (`crates/protocol/src/command.rs`) returns every
  pre-existing resource a body names. It lives in the crate that *defines*
  `CommandBody`, so its match is exhaustive with **no wildcard arm** — a new
  command variant does not compile until somebody classifies it. (In the daemon
  the same match would need a wildcard, because `CommandBody` is
  `#[non_exhaustive]` downstream, and a new variant would silently classify as
  "names nothing".)
* `authorize_command` (`crates/daemon/src/server.rs`) resolves each named resource
  against the daemon's own storage and is called **once**, immediately after the
  handshake check and **before** the dispatch match — so ownership is the OUTER
  gate, ahead of every role, transport and seam check. A principal probing another
  user's ids therefore cannot learn anything from the difference between
  `role-denied`, `transport-unavailable` and `not-found` either.

Resolution rules, by kind: session → `sessions.owner_uid`; run and approval →
their session's owner; document → its session's owner when session-scoped, else
the daemon uid; document lease → the document it is held over; workflow run and
`board:<repo>` → `principal_may_read_workflow`; and the three **daemon-wide
stores** (curated memory, promotion, Remote UI plugins) → the uid the daemon runs
as, since none of their rows has an owner of its own. The plugin store is included
because installing a plugin is an arbitrary-code surface for the worker runtime;
before round 4 it was gated on the client-asserted `ClientRole` alone.

**One documented exception**, and only one: `AttachSession`. The requested role
binds to the connection *before* the attach is evaluated (role bootstrap — a
one-shot client asserts its role by attaching to an id it may not own), and a
rejected attach answers `Payload::Error`, not `CommandRejected`. It is gated
inside `handle_attach` with the same `principal_may_use_session` call and answers
the identical `protocol.session-not-found`.

### Invariant 4 for an idempotent command

`ReleaseDocumentLease` is documented idempotent: releasing an unknown lease
succeeds and does nothing. "You may not" therefore cannot be an error — an error
would be the oracle. Both an unknown lease and a lease over a document this
principal does not own are answered with that same accepted no-op
(`Refusal::AcceptedNoop`), and no release reaches the seam. Underneath, the store's
`UPDATE` is additionally scoped by `holder_key`
(`crates/knowledge/src/docs/leases.rs`), so a lease id is no longer a bearer
capability over another writer's lock even for a same-uid peer.

### Verified on the wire (round 4)

Driven against a live `target/debug/codypendentd` as **uid 2000** against **uid 0**'s
resources, with the socket deliberately opened (the `0700` run directory is the
outer wall; these are the daemon's own gates behind it):

```
PublishDocument(uid 0's real doc)  -> CommandRejected document.not-found 'no document 019ffd94-ed42-…'
PublishDocument(absent doc)        -> CommandRejected document.not-found 'no document 099ffd94-0000-…'
MutateDocument(real / absent)      -> document.not-found  (identical pair)
AcquireDocumentLease(real/absent)  -> document.not-found  (identical pair)
ReleaseDocumentLease(real lease)   -> CommandAccepted     ; lease still 'active' in the DB
ReleaseDocumentLease(absent lease) -> CommandAccepted     (identical)
ListUiPlugins                      -> plugin.runtime-unavailable   (= the "not enabled here" answer)
InspectMemory                      -> memory.transport-unavailable (= the "not enabled here" answer)
ReadBlackboard / PostBlackboardItem(repo board) -> workflow.run-not-found
```

The owner (uid 0) is unaffected on the same daemon: lease granted, board read and
written, and `PublishDocument` parked its approval
(`DocumentPublishRequested … git_action='write docs/owner-publish-probe.md …'`).
