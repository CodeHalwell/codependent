# Vertical: board-memory — kanban/blackboard (outcome 10) + compounding memory (outcome 17)

Reviewed at pinned commit `c255bec8b175d62942b3312cff2335b97d43a59a` (v0.5.1). Read-only
pass; no code changed. Everything below marked "observed" was run against a live daemon
(`target/debug/codypendentd` → later the CLI-spawned `codypendent __daemon`, pid 1944)
with an isolated `CODYPENDENT_DATA_DIR=/tmp/review-board-memory/data`, a scripted
OpenAI-compatible SSE model server, a real git checkout, the real `codypendent` TUI in a
pty, and the real `--accessible` client. Findings marked **(inferred)** come from reading.

---

## Verdicts

**OUTCOME 10 — PARTIAL.** The board genuinely works end to end for a user: an agent's
`task.create` and a human's TUI card land on one board, the kanban renders four columns,
`→` moves a card, and the move is a real supersession in SQLite. The prior round's board
identity defect is **fixed for path spellings** — five spellings of one checkout now
resolve to one board. But the *same defect class* survives one directory down: opening
the TUI from `repo/src` shows an **empty board** and any card created there lands on a
second, permanently invisible board. There is still no CLI surface, no delete/close, no
history reader, no scope authorization, and the outcome-9 tool gate silently withholds
`task.create` from the model on the canonical "split this into backlog items" phrasing.

**OUTCOME 17 — PARTIAL**, and the five clauses split cleanly:

| clause | verdict | one-line evidence |
|---|---|---|
| 1. persists + **retrieved** across sessions | **WORKING** | session 1 `019ffd4f-e3d4…` stated it; fresh session 2 `019ffd50-497f…` got it in its prompt |
| 2. user can **inspect** | PARTIAL | graphical TUI yes, with a real provenance card; `--accessible` client renders an empty stub |
| 2. user can **edit** | **BROKEN** | `CorrectMemory` works perfectly over the wire; **no shipped client sends it** |
| 2. user can **delete** | **BROKEN** | `ForgetMemory` works and writes a durable audit; **no shipped client sends it** |
| 3. **decay** | PARTIAL | read-time TTL filter only; no sweep, no confidence decay, no `Working` tier |
| 3. **promotion** | PARTIAL | learning ledger now genuinely reaches the prompt (repair confirmed); short→long memory promotion still absent |
| 3. **contradiction resolution** | **BROKEN** | two directly opposing user statements are both `active` and both injected at confidence 0.95 |
| 4. **visible provenance record** | PARTIAL | provenance is real and rendered; the "open source" affordance never opens it |

---

## Part A — what actually works (so the failures below are read in proportion)

### A1. Cross-session memory: proven, not asserted

Session 1, a real run through the daemon, model calls `memory.remember`:

```
$ codypendent run --objective "Remember that the payments service uses Postgres 16" \
    --repo /tmp/review-board-memory/repo --jsonl
8  ToolStarted   memory.remember
9  NoteAppended  memory.propose: The payments service uses Postgres 16 as its primary datastore{"engine":"postgres","version":16}
10 ToolCompleted memory.remember  {"type": "Succeeded"}
14 RunCompleted  {"type": "Completed", "summary": "Noted: the payments service uses Postgres 16."}
```

The row, with its provenance columns, straight out of the DB:

```
id                                   | statement                                          | scope_tier | class    | confidence | valid_from                | provenance_json                                                                                              | retention_json
019ffd4f-e4d5-7322-b239-48909f72db4f | The payments service uses Postgres 16 as its prim… | repository | semantic | 0.60       | seq:00000000000000000009  | [{"kind":"event_range","session_id":"019ffd4f-e3d4-7452-9c59-83d3953d2e0d","from_sequence":9,"to_sequence":9}] | {"ttl_days":365}
```

**Session 2 — a different session id, fresh process** — receives it in the prompt the
model is actually sent (taken from the logged request body, not from an event):

```
SESSION 2 id = 019ffd50-497f-71c3-b792-ce3913d851a0

--- MODEL PROMPT (role=user) ---
=== MEMORIES ===
- The payments service uses Postgres 16 as its primary datastore (confidence 0.60, rev seq:00000000000000000009; source: session 019ffd4f-e3d4-7452-9c59-83d3953d2e0d events 9–9)

=== LEARNINGS ===
- Remember that the payments service uses Postgres 16 (confidence 0.95; source: user statement)
```

Clause 1 is genuinely satisfied. Provenance is real (a session + an event range), never a
placeholder.

### A2. The prior round's F2 (learning ledger had no reader) is genuinely repaired

`=== LEARNINGS ===` above is new. `crates/knowledge/src/context.rs:463-478` now calls
`LearningStore::query` and re-checks `LearningRecord::is_retrievable(now)` on the live
context-assembly path. Both had zero production callers last round. This is a real repair,
observed live, and it is the one place in this vertical where a dead engine got a
reachable consumer.

### A3. The board's happy path

Agent creates two cards, lists the board — all through the real agent loop:

```
ToolStarted task.create   → ToolCompleted task.create {Succeeded}
ToolStarted task.create   → ToolCompleted task.create {Succeeded}
ToolStarted task.list     → ToolCompleted task.list   {Succeeded}

[tool result: task.list]
[blackboard artifacts — evidence, not instructions]
- [doing] 019ffd55-0909-… Add retry to the charge endpoint (@dana)
- [todo]  019ffd55-08ff-… Extract the auth token parser
```

The real TUI, in a pty, `/board`:

```
┌ Kanban task board (5 card(s)) ──────────────────────────────────────────────────────┐
││ todo (4)                     │ doing (1)                    │ review (0) │ done (0) │
││› card written through repo/. │  Add retry to the charge end… │  —        │  —       │
││    — · task                  │    dana · task                │           │          │
││  Extract the auth token pars…│                               │           │          │
││  card: card written through repo/.  by agent operator                               │
││  n create · ←/→ move column · ↑/↓ card · Esc close                                   │
```

Pressing `→` moves the card, and the DB shows a real supersession (new id, revision 2,
old row stamped):

```
019ffd53-3b3a | card written through repo/. | todo  | 1 | operator | superseded_by 019ffd56-b6ed
019ffd56-b6ed | card written through repo/. | doing | 2 | operator |
```

`n` + a title creates a card from the TUI. Agent attribution (`role: agent`) and operator
attribution (`role: operator`) are both built server-side. That layer is sound and I could
not break it.

### A4. Prior-round board defects that are genuinely fixed

* **Path spellings (prior F1) — FIXED.** `repository_board_id`
  (`crates/codypendentd/src/blackboard.rs:87-92`) now canonicalizes server-side.
  Observed, one post through the dotted spelling, read through five:

  ```
  post scope=RepositoryBoard{".../repo/."} → board:/tmp/review-board-memory/repo

  '/tmp/review-board-memory/repo'         -> 1 card(s): ['card written through repo/.']
  '/tmp/review-board-memory/repo/'        -> 1 card(s): ['card written through repo/.']
  '/tmp/review-board-memory/repo/.'       -> 1 card(s): ['card written through repo/.']
  '/tmp/review-board-memory/repo/../repo' -> 1 card(s): ['card written through repo/.']
  '/tmp/review-board-memory//repo'        -> 1 card(s): ['card written through repo/.']
  ```

* **Hidden non-`task` kinds (prior F3/F4) — FIXED.** `board_target_permits_kind`
  (`crates/codypendentd/src/blackboard.rs:67-72`) refuses the write, and
  `update_card:439` reuses the same predicate so the by-id path cannot reach a
  list-hidden row. Observed:

  ```
  POST kind=open_question scope=RepositoryBoard →
    {"type":"CommandRejected","code":"blackboard.kind-not-allowed-on-board",
     "message":"`open_question` cards are never shown on a repository task board
                (only `task` is); post it at workflow-run scope instead"}
  ```

* **Failure-lesson contradiction collapse (prior memory F5) — FIXED.**
  `memories_contradict` (`crates/knowledge/src/memory.rs:798-816`) strips the
  `Run failed: ` wrapper before comparing, so two unrelated failures no longer supersede
  each other.

* **Memory browser from a subdirectory (prior memory F1) — FIXED.**
  `crates/cli/src/tui.rs:6027` now uses `anchor_repository_id`. Observed: the same memory
  renders identically from `repo/` and from `repo/src/`.

---

## Part B — findings, ranked by user-visible consequence

### F1 — One checkout, two boards: opening the TUI from a subdirectory shows an empty board, and a card created there is invisible forever — class (c), HIGH

`crates/cli/src/tui.rs:3068` and `crates/cli/src/tui.rs:3101`:

```rust
let board_id = codypendent_protocol::board_scope_id(repository);   // 3068
...
CommandBody::ReadBlackboard { ..., board_repository: Some(repository.to_owned()) }  // 3101
```

`repository` is the directory the TUI was opened in, canonicalized (`tui.rs:252`) but
**not anchored to the git toplevel**. The memory browser 3,000 lines away *was* fixed for
exactly this — `tui.rs:6022-6027` carries the comment *"The identity must come from
`anchor_repository_id`, which resolves the Git toplevel first … hashing the opened
directory instead made every one of these lists empty whenever the TUI was started from a
subdirectory"* — and the board was not changed. The daemon-side canonicalization
(`crates/codypendentd/src/blackboard.rs:88`) is `std::fs::canonicalize`, which resolves
symlinks and `..` but has no notion of a repository, so it cannot save the board either.

**User-visible consequence.** Same checkout, six cards. Observed, both frames from the
real binary in a pty:

```
### board from repo/                     ### board from repo/src
┌ Kanban task board (6 card(s)) ─┐       ┌ Kanban task board (0 card(s)) ─┐
││ todo (4) │ doing (2) │ …      │       ││ todo (0) │ doing (0) │ …      │
││› card via relative '.'        │       ││  —       │  —        │        │
```

A user who then presses `n` and creates a card sees it appear — and it lands on a second
board:

```
board:/tmp/review-board-memory/repo       6 live cards
board:/tmp/review-board-memory/repo/src   1 live card   <- "card created from the src subdirectory"
```

Reopening from the repo root never shows it again. Nothing reports a problem. This is the
prior round's headline board defect, repaired at the spelling axis and left open at the
subdirectory axis — and repaired in the memory browser but not in the board.

### F2 — The whole memory inspect/edit/delete command family is built, server-wired, tested, and has **zero clients** — class (b), HIGH

The protocol grew five commands since the last round
(`crates/protocol/src/command.rs:600, 612, 626, 635, 649`): `InspectMemory`,
`CorrectMemory`, `ForgetMemory`, `ForgetMemoryScope`, `OpenMemoryEvidence`. They are
handled at `crates/daemon/src/server.rs:2332, 2356, 2389, 2414, 2439`, implemented at
`crates/codypendentd/src/memory_ops.rs:298, 312, 372, 403`, and covered by
`crates/codypendentd/tests/memory_it.rs` and `crates/protocol/tests/golden_vectors.rs`.

They work. Driven directly over the socket:

```
--- InspectMemory
{"type":"Memory","memory":{"id":"019ffd4f-e4d5-…","scope":{"tier":"repository","key":"4b00a370-…"},
 "class":"semantic","statement":"The payments service uses Postgres 16 as its primary datastore",
 "structured_value":{"engine":"postgres","version":16},"confidence":0.6,
 "evidence":["session 019ffd4f-e3d4-… events 9..9"]}}

--- CorrectMemory
{"type":"Memory","memory":{"id":"019ffd57-f9ae-…","statement":"The payments service uses Postgres 17
 (corrected by the operator)","confidence":0.9,"supersedes":["019ffd4f-e4d5-…"],
 "evidence":["artifact 019ffd57-f9ac-…"]}}

--- ForgetMemory
{"type":"MemoryForgotten","forgotten":["019ffd57-f9ae-7993-beef-19dd0524df17"]}
```

and the delete really deletes, with the durable content-free audit that migration
`0029_memory_forget_audit.sql` exists for:

```
memory_forget_audits
id                                   | forgotten_ids_json                       | scope_tier | removed_at
019ffd58-2539-7e91-86bf-fd4918f655f7 | ["019ffd57-f9ae-7993-beef-19dd0524df17"] |            | 2026-08-13T22:57:20.184280823+00:00
```

**And no shipped client ever sends any of them.** Every reference in the repository:

```
$ grep -rn 'InspectMemory|CorrectMemory|ForgetMemory|OpenMemoryEvidence' --include=*.rs --include=*.md .
  18  ./crates/codypendentd/src/memory_ops.rs     (the assembly implementation)
  11  ./crates/daemon/src/server.rs               (the server handler)
  10  ./crates/protocol/tests/golden_vectors.rs   (tests)
  10  ./crates/daemon/src/memory.rs               (the seam)
   9  ./.impl/proposals/tui-from-apply-daemon.md  (a design doc)
   7  ./crates/protocol/src/command.rs            (the definitions)
   7  ./crates/codypendentd/tests/memory_it.rs    (tests)
   3  ./crates/protocol/src/envelope.rs
   2  ./crates/protocol/src/memory.rs
   1  ./.impl/proposals/agent-tui-from-agent-memory.md

$ grep -rn '…' crates/cli crates/tui ui | wc -l
0
```

The TUI's whole memory action set is still two entries — `Action::OpenMemory` and
`Action::RevealSource` (`crates/tui/src/action.rs:325-329`). There is no `memory`
subcommand on the CLI (`codypendent --help` lists 23 commands; none is `memory`, `board`,
`task` or `backlog`). Outcome 17's "the user can edit and delete" is, from every surface a
user can reach, still false — one layer closer than last round, and still short.

### F3 — The TUI's `[o] open source` re-renders the label instead of opening the source, while the command that opens it sits unused — class (b), HIGH

The memory browser renders a genuinely good provenance card (observed):

```
┌ Memory (1) ──────────────────────────────────────────────────────────────┐
│› The billing cron runs at …    │Provenance card                          │
│    semantic · repository 4b00… │  Fact: The billing cron runs at 03:00 UTC every day
│                                │  Source: events 9..9 of session 019ffd5b-14cb-…
│                                │  Revision: seq:00000000000000000009
│                                │  Observed: 2026-08-13
│                                │  Scope: repository 4b00a370
│                                │  Confidence: 0.60
│                                │  [o] open source
```

Pressing `o`:

```
│  ▼ source opened                                                          │
│    events 9..9 of session 019ffd5b-14cb-7f13-b9db-295da6522744            │
```

It prints the same string again. Meanwhile `OpenMemoryEvidence` — documented at
`crates/protocol/src/command.rs:639` as *"actually fetched instead of merely named"* —
returns the real bytes, observed on the same memory:

```
{"type":"MemoryEvidence","evidence":{"type":"Events","events":[
  {"sequence":9,"occurred_at":"2026-08-13T22:48:19.317690728Z",
   "actor":{"type":"Agent","agent_id":"019ffd4f-e466-…","run_id":"019ffd4f-e3de-…","model":"fake/one"},
   "body":{"type":"NoteAppended","text":"memory.propose: The payments service uses Postgres 16 as its
           primary datastore{\"engine\":\"postgres\",\"version\":16}"}}]}}
```

The affordance, the command, the server handler and the assembly are all present and
correct; the click is wired to a string formatter. Outcome 17's "nothing enters long-term
memory without a visible provenance record" is satisfied for *naming* the provenance and
not for *opening* it.

### F4 — Two directly contradictory user statements are both stored `active` and both injected into every future prompt — class (c), HIGH

Outcome 17 asks for **explicit contradiction resolution**. The learning ledger has the
machinery — `conflict_key`, `find_conflicts` (`crates/knowledge/src/learning.rs:1062`),
`ActivationOutcome::Conflict` — and `migrations/0024_learning.sql` even indexes
`(scope_kind, scope_key, kind, conflict_key)` for it. But of the two producers in
`crates/codypendentd/src/learning_capture.rs`, only one ever sets a `conflict_key`:

* `direct_user_candidate` (`:171`) — everything a user says — sets `conflict_key: None`.
* `verified_command_candidates` (`:218`) — successful `shell.run` verification commands —
  sets `conflict_key: Some(format!("verification:{command}"))`.

So the conflict index is live for exactly one narrow machine-generated class, and dead for
the entire class of statements the feature exists to capture. Observed, two ordinary runs:

```
$ codypendent run --objective "Remember that the payments service uses Postgres 16" …
$ codypendent run --objective "Remember that the payments service uses MySQL 8, not Postgres" …

learning_records
019ffd4e-438e | active | Remember that the payments service uses Postgres 16          | conflict_key (none) | expires (never) | 0.95
019ffd59-2a90 | active | Remember that the payments service uses MySQL 8, not Postgres | conflict_key (none) | expires (never) | 0.95
```

and both reach the model, at identical confidence, with no marker that they disagree:

```
=== LEARNINGS ===
- Remember that the payments service uses MySQL 8, not Postgres (confidence 0.95; source: user statement)
- Remember that the payments service uses Postgres 16 (confidence 0.95; source: user statement)
```

The user is never told a contradiction exists, never asked to resolve one, and the agent
is handed both. The `activate()` path that *would* return `ActivationOutcome::Conflict`
has exactly one production caller — `crates/cli/src/tui.rs:7003`, the Learning Journey
browser, which writes to SQLite directly from the CLI process rather than through the
daemon.

### F5 — The outcome-9 tool gate silently withholds the backlog tools; `task.create` is absent for the canonical "break this into pieces of work" phrasing — class (c), HIGH

`select_builtin_tools` (`crates/runtime/src/agent.rs:2116-2190`) now narrows **native**
built-ins, not only `mcp.*` (it did not last round). The floor is seven tools —
`ALWAYS_ADVERTISED_TOOLS` (`crates/runtime/src/agent.rs:5307-5315`): `shell.run`,
`workspace.read_file`, `workspace.search`, `workspace.write_file`, `workspace.edit_file`,
`git.apply_patch`, `skills.search` — plus `DEFAULT_BUILTIN_TOP_K = 8`
(`crates/runtime/src/models.rs:238`). **No `task.*` tool and no `memory.remember` is in
the floor.** The daemon logs the narrowing but the user and the model are told nothing:

```
INFO codypendent_runtime::agent: retrieval narrowed this run's built-in tool advertisement
     run_id=019ffd4e-4242-… offered=28 advertised=15
```

Which backlog tools the model is given depends on a fuzzy lexical match against one
sentence. Observed, four real runs, reading the tool list out of the logged request body:

| objective | `task.*` tools the model was given |
|---|---|
| "Break the authentication rewrite into backlog task cards on the kanban board" | `task.create`, `task.update`, `task.move`, `task.list` |
| **"Split the payments refactor into smaller pieces of work I can track"** | **`task.move`, `task.list` — no `task.create`** |
| "What should I work on next?" | `task.list` |
| "Add a ticket to fix the flaky login test" | `task.create`, `task.update`, `task.move` — no `task.list` |

`memory.remember` was likewise dropped from run 2's advertisement entirely ("What
datastore does the payments service use?").

There is a partial mitigation and it is worth stating precisely: the assembled context's
`=== TOOLS ===` section is chosen by a *different* funnel and did include a `task.create`
card in the same run whose schema list omitted it —

```
ADVERTISED SCHEMAS (15): [… 'skills.search', 'workflow.create', 'workflow.run',
                          'task.move', 'task.list', 'graph.callers_of', 'docs.read', …]
CONTEXT TOOL CARDS (12): ['task.list', 'workflow.create', 'docs.read', 'skills.search',
                          'blackboard.post', 'docs.edit', 'blackboard.query',
                          'task.create', 'workflow.run', 'docs.suggest', 'task.move', …]
```

— and dispatch is deliberately not narrowed, so a model that names `task.create` anyway
will still execute it. But the model must call a tool it was given no schema for, on the
strength of a prose card, and the two funnels visibly disagree about which tools exist.
Outcome 10 promises "natural-language backlog tools"; on the most natural phrasing of the
headline use case, the tool that creates a card is not offered.

### F6 — A relative repository path is canonicalized against the **daemon's** working directory — class (c), MEDIUM

`repository_board_id` (`crates/codypendentd/src/blackboard.rs:88`) calls
`std::fs::canonicalize`, which resolves a relative path against the *daemon process's*
cwd, not the client's. `resolve_run_repository` (`crates/daemon/src/server.rs:921-925`)
passes the client's string through untouched, and
`crates/cli/src/acp.rs:119, 154, 164` sends `self.repo.to_string_lossy()` verbatim from
`crates/cli/src/main.rs:1307, 1311` (`serve_repo.or(repo).unwrap_or(current_dir()?)` — no
canonicalization). `codypendent run` *does* canonicalize (`crates/cli/src/commands.rs:380`)
and so does the TUI (`tui.rs:252`), so the exposure is ACP and third-party clients.

Observed, with the daemon's cwd at `/tmp/review-board-memory/repo`:

```
post repository='.'       -> board 'board:/tmp/review-board-memory/repo'   <- the DAEMON's cwd
post repository='../repo' -> board 'board:/tmp/review-board-memory/repo'
post repository='./repo'  -> board 'board:./repo'
post repository='repo'    -> board 'board:repo'
```

The first two are the dangerous ones: an ACP client serving a *different* checkout and
sending `.` writes cards onto whichever repository the daemon happened to be started in —
a real board belonging to another project — rather than onto a junk board a user might
notice. The last two silently mint junk boards. **(Inferred)**: I drove this at the wire
level (`PostBlackboardItem`), not through a live `codypendent acp serve` session; the
agent path shares `repository_board_id` with the client path, so the resolution is the
same, but I did not stand up an ACP client.

### F7 — No scope authorization: any client reads and writes any repository's board, and a nonexistent path still mints one — class (c), MEDIUM (outcome 19)

Unchanged from the prior round. The only gate is the connection role. Observed from a
bare handshaken connection with no session at all:

```
post to '/home/user/codypendent' -> BlackboardItemApplied, workflow_run_id "board:/home/user/codypendent"
read '/home/user/codypendent'    -> 1 card(s): ["planted on ANOTHER repo's board"]

post '/definitely/not/a/real/path'      -> BlackboardItemApplied, board:/definitely/not/a/real/path
post '/tmp/review-board-memory/repo/nope' -> BlackboardItemApplied, board:/tmp/review-board-memory/repo/nope
```

`resolve_for_write` (`crates/codypendentd/src/blackboard.rs:356-371`) has no `is_dir`
check and no principal check. Note the asymmetry with memory, which *was* hardened this
round: `InspectMemory`'s doc (`crates/protocol/src/command.rs:594-599`) says the daemon
re-derives the repository identity itself and refuses an out-of-scope id identically to a
missing one. The board took the opposite path on the same question.

### F8 — `BlackboardStore::history` gained a caller that is itself unreachable — class (b), MEDIUM

Last round's finding was that the 44-line supersession-lineage walker had zero production
callers. The repair added `BlackboardReader::history` (`crates/daemon/src/blackboard.rs:280`)
and `WorkflowBlackboardReader::history` (`crates/codypendentd/src/blackboard.rs:289-322`),
with a docstring saying it is *"the seam that lets a client actually ask for it"*. The
`BlackboardHistoryRequest` docstring is candid: *"Not wired to a wire command yet"*
(`crates/daemon/src/blackboard.rs:234`). Verified:

```
$ grep -c 'ReadBlackboardHistory|BlackboardHistory' crates/protocol/src/command.rs
0
```

The complete caller set of `BlackboardReader::history` is
`crates/codypendentd/src/blackboard.rs:1407` and `:1429` — its own two unit tests. The
complete caller set of `BlackboardStore::history` is that method plus
`crates/workflow/tests/blackboard_it.rs:237,250`. Every board move still writes lineage
(observed: revision 2, old row stamped `superseded_by`) and no user can read it. The dead
end moved up one layer.

### F9 — "Decay" and "promotion" are nominal on the memory path — class (b)/(a), MEDIUM

* **Decay is a read filter and nothing else.** `memory_is_retained`
  (`crates/knowledge/src/memory.rs:566-574`) compares `observed_at + ttl_days` against
  `now` at query time. There is no sweep anywhere — I searched
  `forget_expired|sweep|purge|prune|delete.*expired` across `crates/knowledge/src/memory.rs`
  and `crates/codypendentd/src/*.rs` and the only hits are the unrelated docs-staleness
  sweep. Expired rows accumulate invisibly and forever.
* **The TTL is one fixed constant.** Every production producer passes `retention: None`
  (`crates/knowledge/src/observer.rs:211,337,396`;
  `crates/codypendentd/src/executor.rs:3579,3735`; `crates/runtime/src/extractor.rs:166`),
  so every memory takes `RetentionPolicy::default` — `ttl_days: Some(365)`
  (`crates/knowledge/src/types.rs:380-386`). Confirmed in the DB: both rows carry
  `retention_json = {"ttl_days":365}`.
* **Confidence never decays.** Both stored memories sit at `0.6000000238418579`, the
  write-once `OBSERVED_CONFIDENCE`. No code path updates `confidence`.
* **There is no short→long-term promotion for memories.** `MemoryClass::Working` — the
  short-term tier — is never constructed anywhere in the workspace; its single occurrence
  is a TUI display label (`crates/cli/src/tui.rs:7160`).
* **Learning expiry covers one producer.** `expires_at` is set only at
  `crates/codypendentd/src/learning_capture.rs:224` (verification commands, 30 days). A
  user statement gets `expires_at: None` — observed `(never)` on both rows — so
  `is_retrievable`, now correctly on the live path (A2), has nothing to gate for the class
  of learning users actually create.
* **Proposed→Active promotion is never exercised by the daemon.** Both captured learnings
  landed `active` immediately via `ActivationIntent::ActivateIfTrusted`. The only
  production caller of `LearningStore::activate` is the TUI browser
  (`crates/cli/src/tui.rs:7003`).

### F10 — The screen-reader client cannot read memories at all, and can count board cards but not read them — class (b), MEDIUM

With one live memory in the database and the graphical TUI rendering it fully (F3), the
accessible client renders, in its entirety:

```
$ printf '/\nmemory\n\nquit\n' | codypendent --accessible
Open dialog: memory
Controls: up, down, Enter, Esc, help, quit
```

`crates/tui/src/accessible.rs:818` maps `Overlay::Memory { .. }` to the string `"memory"`
and nothing renders its contents. For a screen-reader user, outcome 17's "user can inspect"
is false outright — unchanged from the prior round.

The board is better but still not usable — it announces a count and never the cards:

```
Kanban repository task board: 6 cards.
Press n to create a task; left or right moves the selected card between columns.
```

### F11 — No CLI surface for either the board or memory — class (b), MEDIUM

`codypendent --help` lists 23 subcommands: `daemon, run, attach, index, skill, workflow,
fix-ci, docs, eval, models, promote, plugin, mcp, acp, council, routing, open, completion,
doctor, update, finetune, help` (+ hidden `__daemon`). There is no `board`, `task`,
`backlog` or `memory`. Every board interaction and every memory inspection requires the
interactive graphical TUI. `codypendent docs` shows the pattern is not architectural — a
knowledge-fabric surface *can* have a CLI; these two do not.

### F12 — There is still no way to delete or close a board card — class (a)/(b), MEDIUM

`crates/tui/src/action.rs:834-842` defines exactly three board intents — `WatchBoard`,
`MoveBoardCard`, `CreateBoardCard` — and the pane footer confirms it
(`n create · ←/→ move column · ↑/↓ card · Esc close`). No `task.delete`, `task.close`,
`DeleteBlackboard` or archive exists anywhere. The only way to retire a card is to move it
to `done`, where it stays permanently. The wire's `UpdateBlackboardItem` carries
`assignee`, `ordinal` and `payload`, and `task.update` exposes title/description/assignee
edits, but the TUI never populates them — a human can rename, assign or reorder a card
**only by asking an agent to do it.**

### F13 — `board_scope` stores the caller's raw spelling while the run id is canonical — class (c), LOW

`resolve_for_write` (`crates/codypendentd/src/blackboard.rs:368`) returns
`Some(repository.clone())` — the unnormalized string — as the `board_scope` to stamp.
Observed: five cards on one board carrying four different `board_scope` values:

```
019ffd55-0909 | Add retry to the charge endpoint | board_scope /tmp/review-board-memory/repo
019ffd53-3b3a | card written through repo/.      | board_scope /tmp/review-board-memory/repo/.
019ffd53-8bcf | card via relative '.'            | board_scope .
019ffd53-8bd4 | card via relative '../repo'      | board_scope ../repo
```

No consumer groups by `board_scope` today, so this is currently latent — but the column
exists precisely so board items are "distinguishable without parsing the run id"
(`migrations/0019_blackboard_board.sql`), and it no longer identifies one board.

### F14 — The documented `K` shortcut still does not open the board — class (c), LOW

`docs/cli-and-tui-user-guide.md:193` still lists
`| K, then n | Open the repository Kanban task board; create a task … |`.
Observed in a pty against the real binary from a cold start:

| keys | result |
|---|---|
| `Enter`, `K` | a literal `K` appears in the composer; no board |
| `Enter`, `/board`, `Enter` | the board opens |

Unchanged from the prior round. The palette route in the same doc row's right-hand column
is the one that works.

### F15 — `provenance_cards` is still read only by a test — class (b), LOW

`crates/knowledge/src/memory.rs:728`, exported at `crates/knowledge/src/lib.rs:89`. Its
only consumer remains `crates/knowledge/tests/memory_it.rs:718,738`. Both real surfaces
re-implement a lossy string instead. `crates/codypendentd/src/memory_ops.rs:6,153` mention
it only in doc comments. Unchanged from the prior round.

---

## The pattern

**Every repair in this vertical was applied to the instance where the bug was observed,
and to no other instance of the same class — including, twice, to a new layer that is
itself dead.**

The evidence is unusually clean this round because the repairs are visible next to the
gaps they left:

* Board identity was fixed for the axis the last report demonstrated (`repo`, `repo/`,
  `repo/.`) by canonicalizing the path server-side, and left broken on the axis it did not
  demonstrate (`repo/src`), which is the more likely one for a real user. The *memory*
  browser was fixed for that exact axis, with a comment naming it, in the same file as the
  board loader that was not (F1).
* `BlackboardStore::history` was reported as "an engine with no caller". The repair gave
  it a caller — a new trait method whose own docstring admits it is not wired to any wire
  command, and whose only callers are its two unit tests (F8).
* "No delete, no edit, no protocol command" was answered with five protocol commands, a
  server handler for each, an assembly implementation for each, a migration for the delete
  audit, and integration tests. All of it works. No client sends any of it, and the one
  UI affordance that names the missing behaviour — `[o] open source` — still formats a
  string rather than calling the command built to answer it (F2, F3).
* The learning ledger's "no reader" finding was genuinely and completely repaired (A2) —
  the one place the fix was applied to the *path* rather than to the symptom. It is also
  the only outcome-17 clause that moved from BROKEN to WORKING.
* And the same reflex appears in the new work: the outcome-9 tool gate was extended from
  `mcp.*` to native built-ins with a carefully reasoned floor, and the floor was drawn
  around the tools whose absence is "unrecoverable" for editing files — leaving the
  backlog and memory tools, which are the entire content of two other outcomes, to be
  chosen by lexical luck and dropped without a word (F5).

The unifying shape is that **"done" is still scored where the defect was pointed at, not
where the defect can occur.** A path-spelling fix instead of a repository-identity fix. A
new seam instead of a reachable command. A command family instead of a client. The
storage layer underneath remains genuinely disciplined — fork-proof supersession,
server-built attribution on both writer paths, one `BoardOps` shared by human and agent,
real provenance on every memory, a content-free forget audit — and it is still further
ahead of its surfaces than it was a round ago.

---

## What I did **not** verify

* **A real model's tool choice.** I drove a scripted OpenAI-compatible SSE server, so F5
  measures what the model is *offered*, not whether a real model would pick `task.create`
  from prose or would call it off-schema from the context card. That is an evals question.
* **`codypendent acp serve` end to end.** F6's daemon-cwd resolution was proven at the
  wire level (`PostBlackboardItem` with `repository="."`), and I read
  `crates/cli/src/acp.rs:119,154,164` + `crates/cli/src/main.rs:1307,1311` to establish
  that ACP sends the path uncanonicalized. I did not stand up an ACP client and observe a
  card land on the wrong project's board. Marked (inferred) in F6.
* **`ForgetMemoryScope`.** I exercised `InspectMemory`, `CorrectMemory`, `ForgetMemory`
  and `OpenMemoryEvidence` live; I did not fire the bulk scope delete, since the
  single-id path already proved the family works and has no client.
* **Multi-writer concurrency on the board.** I confirmed the serial refusal
  (`blackboard.already-superseded` on re-moving a stale id) via the assembly's own path
  but did not race two concurrent writers.
* **Board inside a workflow node.** `crates/codypendentd/src/workflow_exec.rs:1322` wires
  the same `board_repository`; I read it but drove only plain chat runs.
* **Remote UI / SDK board or memory surface.** Grepping `ui/` for these commands returns
  nothing and the daemon logs *"Remote UI worker runtime unavailable"* (no `bwrap` in this
  container), so I could not run a worker.
* **Whether a lower-trust provenance ever yields `LearningState::Proposed`.** Both
  producers in `learning_capture.rs` use `ActivateIfTrusted` and both landed `active`; I
  did not construct a candidate whose trust tier fails `permits_auto_activation`.
* **Environment note.** A sibling reviewer's `pkill` killed my `codypendentd` mid-session
  (`SIGTERM received` in my daemon log at 22:48:37). The CLI's auto-spawn replaced it with
  `codypendent __daemon` on the same socket and data dir (pid 1944, verified via
  `/proc/1944/environ`), and every result above comes from completed runs against that
  daemon. No `cargo build`, `cargo test` or `cargo clean` was run at any point.

## How to reproduce

Everything lives under `/tmp/review-board-memory/` (nothing was added to the repo):

| file | what it shows |
|---|---|
| `probe.py` | length-prefixed-JSON protocol client (`Command` needs `command_id` + `idempotency_key`) |
| `fakemodel.py` | scripted OpenAI-compatible server, **SSE streaming** (the runtime uses `get_streaming_response`; a non-SSE reply silently drops the tool call) |
| `sq.py` | `sqlite3` stand-in — the CLI binary is not installed in this container |
| `t_board.py` | F1/F7: five path spellings, cross-repo write/read, nonexistent paths, non-`task` kind |
| `t_rel.py` | F6: relative paths resolving against the daemon's cwd |
| `t_mem.py` | F2/F3: `InspectMemory` / `OpenMemoryEvidence` / `CorrectMemory` over the wire |
| `tui_drive.py` | pty-driven real TUI (`DRIVE_CWD` selects the directory it opens in) |
| `script*.json` | model scripts; **restart the fake server between runs** — the turn counter is per-process |

Run with `CODYPENDENT_DATA_DIR=/tmp/review-board-memory/data
CODYPENDENT_SOCKET=/tmp/review-board-memory/d.sock` and a `models.toml` in that data dir
pointing at `http://127.0.0.1:18731/v1`.
