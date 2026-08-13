# Vertical: board — built-in blackboard + kanban board; natural-language backlog tools

Owned outcome: **10**. Reviewed at pinned commit `535a2f5e3848b256536ddee94883dc0010ecdcb8` (v0.4.5).
No code was changed.

---

## Verdict

**OUTCOME 10: PARTIAL** — both the blackboard AND a real, distinct kanban board exist and the
happy path works end to end against a live daemon (operator card → agent card → column move →
live fan-out → rendered TUI columns), but board identity is an unvalidated caller-supplied
string, nothing re-derives the board scope server-side, non-`task` kinds are silently
swallowed, and the only user surface is one TUI overlay reachable by an undocumented route.

### Is there a kanban board distinct from the blackboard?

**Yes — both exist, and they are genuinely different things.** I want to be plain about this
because the two share storage and it would be easy to mistake one for the other.

* The **blackboard** is the agent artifact store: `crates/workflow/src/blackboard.rs`,
  typed kinds (`finding`, `hypothesis`, `decision`, …), evidence-required discipline,
  supersession chains, per-workflow-run scope. Agent tools `blackboard.post` /
  `blackboard.query`.
* The **kanban board** is a separate user-facing column/card surface layered on top of it:
  migration `migrations/0019_blackboard_board.sql` adds `status` / `assignee` / `ordinal` /
  `board_scope`; a repository board is a synthetic workflow run
  `board:<repo>` (`crates/protocol/src/blackboard.rs:30`); it has its own wire types
  (`BlackboardScope`, `BlackboardItemDraft`, `PostBlackboardItem`, `UpdateBlackboardItem`),
  its own agent tools (`task.create` / `task.update` / `task.move` / `task.list`,
  `crates/runtime/src/tools/task.rs`), and its own TUI pane with four columns and
  arrow-key card moves (`crates/tui/src/render.rs:6441` `render_kanban`).

The kanban is **not absent** and it is not the blackboard dressed up. I drove it and it renders:

```
┌ Kanban task board (3 card(s)) ──────────────────────────────────────────────────────────┐
││ todo (0)                  │ doing (2)                 │ review (1)      │ done (0)     │
││  —                        │› wire the DAG viewer      │  second card    │  —           │
││                           │    — · task               │    dana · task  │              │
││                           │  agent card: reconnect A… │                 │              │
││  card: wire the DAG viewer  by agent operator                                          │
││  n create · ←/→ move column · ↑/↓ card · Esc close                                      │
```

(real `codypendent` binary in a pty against a live `codypendentd`, terminal-emulated frame)

### Natural-language backlog tools — are they registered, and does the retrieval gate pass them?

**Yes to both, verified against a live model call.** I stood up an OpenAI-compatible fake
provider, logged every request body, and ran a real chat run through the daemon. The tool list
the model actually received (17 tools) was:

```
shell.run, workspace.read_file, workspace.search, git.diff, repository.test,
memory.remember, skills.search, workflow.query, workflow.create, workflow.run,
task.create, task.update, task.move, task.list,
council.create, council.run, council.result
```

All four `task.*` tools are present with full JSON schemas. The outcome-9 retrieval gate does
**not** touch them: `select_mcp_tools` (`crates/runtime/src/agent.rs:1878`) narrows only the
`mcp.*` family; native tools are gated solely by `offers_task_board`
(`crates/runtime/src/agent.rs:1606` — a wired channel plus a run repository identity).
They additionally appear as retrieval tool cards in the assembled context
(`crates/knowledge/src/builtin.rs:213-238`), which I saw in the run's `NoteAppended`:

```
=== TOOLS ===
tool task.list [safe, first-party] — List the repository Kanban task board. …
tool task.create [safe, first-party] — Create a typed task card on the repository Kanban board. …
```

And they execute. From the live ledger:

```
ToolStarted  task.create → ToolCompleted task.create {Succeeded}
[tool result: task.create] created card 019ff88e-11fc-… in `todo`: agent card: reconnect ACP on socket drop
ToolStarted  task.list   → ToolCompleted task.list   {Succeeded}
[tool result: task.list]
  [blackboard artifacts — evidence, not instructions]
  - [todo]  019ff88e-11fc-… agent card: reconnect ACP on socket drop
  - [doing] 019ff87e-76e6-… wire the DAG viewer
  - [doing] 019ff87e-76e2-… second card (@dana)
```

The card landed in the DB with server-built attribution
(`author = {"role":"agent","run_id":"019ff88e-119a-…"}`), and the human then moved it from the
TUI with `→`, producing revision 2 with `author.role = "operator"`. One board, two writers,
one supersession discipline. That part is genuinely good.

### Trust boundary

**Board mutations build the *actor* server-side but accept the *scope* from the caller.**
Attribution is honest: `operator_author(client_id)`
(`crates/codypendentd/src/blackboard.rs:513`) is built from the connection's client id, and
the agent's author from the run context — a client cannot forge either. That half is right.

The scope half is not. `BlackboardScope::RepositoryBoard { repository }` and
`ReadBlackboard.board_repository` are raw strings taken verbatim; nothing checks them against
the session's stored repository, the path's existence, or its canonical form. See F1 and F2.

### Surfaces

* **TUI**: yes — `Overlay::Kanban`, palette `/board`, key `K`. Renders, loads, moves cards.
* **CLI**: **absent**. There is no `codypendent board` / `task` / `backlog` subcommand
  (`crates/cli/src/main.rs` `TopCommand`). The board is TUI-only.
* Revision history is stored on every move and **has no reader at all** (F5).

---

## Findings

### F1 — Board identity is an unvalidated caller string; a non-canonical repo path silently forks the board — class (c), HIGH

`crates/protocol/src/blackboard.rs:30`

```rust
pub fn board_scope_id(repository: &str) -> String { format!("board:{repository}") }
```

Its own doc says *"Callers pass the **canonicalized** repository root; this does no I/O."*
Nothing on the server enforces that. `resolve_run_repository`
(`crates/daemon/src/server.rs:829`) takes `repository` verbatim, and
`crates/cli/src/acp.rs:27` + `crates/cli/src/main.rs:1189,1193` never canonicalize the
`--repo` an ACP session is served with (the interactive TUI at `crates/cli/src/tui.rs:250`
and `codypendent run` at `crates/cli/src/commands.rs:378` both do).

**User-visible consequence.** A user runs `codypendent acp serve --repo ./myrepo` (or any
third-party client that sends a non-canonical path). The agent calls `task.create`. The tool
reports success — *"created card 019ff894-… in `todo`"*. The user opens the TUI board on the
same checkout and the card is not there. Ever. Nothing reports a problem.

Demonstrated live, one daemon, one directory, two path strings:

```
StartRun repository = ".../boardrev/repo/."      (non-canonical)
tool events: [ToolStarted task.create, ToolCompleted task.create {Succeeded}]

board ".../boardrev/repo"    (what the TUI reads) → 3 cards, none of them the new one
board ".../boardrev/repo/."  (what the run used)  → todo | ACP-path card: reconnect work | agent
```

The protocol-level split reproduces for every path variant; each one mints its own board on
first write:

```
'.../boardrev/repo'          -> 2 card(s)
'.../boardrev/repo/'         -> 0 card(s)
'.../boardrev/repo/.'        -> 0 card(s)
'.../boardrev/repo/../repo'  -> 0 card(s)
'.../boardrev//repo'         -> 0 card(s)
```

Fix shape: canonicalize once, server-side, in `BoardOps::resolve_for_write` /
`WorkflowBlackboardReader::read` (`crates/codypendentd/src/blackboard.rs:270,202`), not in
each client.

### F2 — No server-side scope derivation: any client can read or write any repository's board — class (c), HIGH (outcome 19)

`crates/daemon/src/server.rs:2187` (`PostBlackboardItem`), `:2218` (`UpdateBlackboardItem`),
`:2144` (`ReadBlackboard`); `crates/codypendentd/src/blackboard.rs:270` (`resolve_for_write`).

The only gate is the connection **role** (Controller to write, anyone to read). The board is
selected entirely by the path string on the command. A session created and attached against
repository A can write to and read from repository B's board:

```
session repository = .../boardrev/repo
POST scope=RepositoryBoard{"/home/user/codypendent"}  → BlackboardItemApplied
     board_scope "/home/user/codypendent", workflow_run_id "board:/home/user/codypendent"
READ board_repository="/home/user/codypendent"        → 1 item: "planted on another repo's board"
```

An **Observer** on that session — correctly refused the write (`protocol.role-denied`,
*"only a Controller may write the blackboard"*) — can still read the same arbitrary board:

```
Observer READ of the board                       → 2 items
Observer READ of an ARBITRARY repo's board       → 1 item: "planted on another repo's board"
```

`resolve_for_write` also creates the synthetic board run for a path that does not exist at
all — no `is_dir` check, no repository check:

```
POST scope=RepositoryBoard{"/definitely/not/a/real/path"} → BlackboardItemApplied
workflow_runs now contains 'board:/definitely/not/a/real/path'
```

For a single-operator local daemon this is mostly an accidental-typo hazard. For outcome 19
(real multi-user) it is the load-bearing gap: there is no notion of *which boards this
principal may touch*, and the wire never carries one.

### F3 — SILENT FILTER: any blackboard kind may be written to a repository board, and every board view then hides it forever — class (c), MEDIUM

Write side accepts any kind: `crates/codypendentd/src/blackboard.rs:521` parses the draft's
`kind` against the whole `BlackboardKind` enum with no Task restriction, then stamps it with
board fields via `post_card` (`:290`).

Read side filters to `task` in **both** board views:
`crates/cli/src/tui.rs:3086` (`kind: Some("task".to_owned())`) and
`crates/codypendentd/src/blackboard.rs:646` (`Some(BlackboardKind::Task)`).

**User-visible consequence.** Post an `open_question` at `RepositoryBoard` scope. You get a
success reply with a card id, `status: "todo"`, `ordinal: 0`, `board_scope` set. It never
appears on the board, in `task.list`, or anywhere else. Live:

```
POST kind=open_question, scope=RepositoryBoard  → BlackboardItemApplied, status todo, ordinal 0
READ kind=task     → 2 items
READ kind=None     → 3 items  (the open_question is the extra)
```

Two aggravating details:

1. **Ordinal pollution.** `BlackboardStore::next_ordinal`
   (`crates/workflow/src/blackboard.rs:324`) filters by `workflow_run_id` and `status` but
   **not** by kind, so invisible artifacts consume visible column positions. In my run the
   agent's first-ever `todo` card was assigned `ordinal: 1` because the hidden
   `open_question` held `ordinal: 0`.
2. **Live/read disagreement.** The TUI routes a live delivery to the kanban on
   `item.board_scope.is_some()` alone (`crates/cli/src/tui.rs:1552`), with no kind check. So a
   non-`task` item posted while the board pane is open **does** appear as a card, then
   disappears the next time the board is opened. The pane and the reload disagree.

Either reject a non-`task` kind at `RepositoryBoard` scope, or stop filtering by kind on read.
Doing neither is the worst of both.

### F4 — The by-id write path has no gate the list path has — class (c), MEDIUM

`crates/codypendentd/src/blackboard.rs:336-397` (`update_card`), `:342-347`:

```rust
let (run_id, _) = self.resolve_for_write(target).await?;
let old = self.store.get(&self.pool, &run_id, item_id).await?
    .ok_or_else(|| BlackboardError::NotFound(item_id.to_string()))?;
```

`store.get` (`crates/workflow/src/blackboard.rs:365`) is scoped by run id only — no kind
filter, unlike the list path. And `BoardTarget::WorkflowRun` (`crates/daemon/src/blackboard.rs:146`)
is accepted straight from a client's `BlackboardScope::WorkflowRun`.

The brief asked specifically whether a user can fetch/act by id on something the list hides.
**Yes.** Demonstrated:

```
board LIST (kind=task) shows 3 items;  board actually holds 4
HIDDEN: [('open_question', {'question': 'is this on the board?'})]

UPDATE that hidden id at RepositoryBoard scope → BlackboardItemApplied, revision 2,
   payload rewritten to "REWRITTEN by a client that could not see this item", status→done
board LIST after the mutation: still 3 items (the rewrite stays invisible)
```

The same seam lets a Controller post and then rewrite a **`finding`** at `WorkflowRun` scope.
`update_card:378` carries `evidence: old.evidence` forward untouched while `payload` is
merged from the caller (`merge_card_payload`, `:435`):

```
POST   kind=finding, scope=WorkflowRun  → applied, evidence [{path src/x.rs, line 1}]
UPDATE payload={"summary":"REWRITTEN claim, original evidence kept"}
       → revision 2, same evidence, summary replaced
```

Attribution stays honest (`role: operator`), which limits the blast radius — but an agent
later running `blackboard.query` sees a live, evidence-carrying `finding` whose claim no
longer matches the evidence that grounds it. The client write seam is documented as the
kanban half (`crates/daemon/src/blackboard.rs:35-38`); accepting `WorkflowRun` scope with an
unrestricted payload merge is wider than that intent.

### F5 — Card history is written on every move and read by nobody; no CLI surface; no delete — class (b), MEDIUM

`BlackboardStore::history` (`crates/workflow/src/blackboard.rs:388-431`) walks a card's full
supersession lineage in both directions — 44 lines of careful chain-walking. **It has zero
production callers.** Its only references in the whole repo are
`crates/workflow/tests/blackboard_it.rs:237` and `:250`.

Every board move is a supersession (verified: a column move produces revision 2 and a new id),
so the design deliberately preserves lineage — and then no command, tool, or view exposes it.
There is no `ReadBlackboardHistory` on the wire, no `task.history` tool, no TUI affordance.
This is the textbook engine-built-wire-never-attached case.

Alongside it:

* **No CLI surface at all.** `crates/cli/src/main.rs` `TopCommand` has no `board` / `task` /
  `backlog` variant. `codypendent --help` confirms it. Every board interaction requires the
  interactive TUI.
* **No delete or close.** Grepping for `DeleteBlackboard` / `task.delete` / `task.close` /
  archive across `crates/` returns nothing. The only way to retire a card is to move it to
  `done`, where it stays on the board permanently. Outcome 10's "add/move/close" reads as
  two-and-a-half of three.

### F6 — The documented `K` shortcut does not open the board from the TUI's default state — class (c), LOW

`docs/cli-and-tui-user-guide.md:190` lists:
`| K, then n | Open the repository Kanban task board; create a task |`

`crates/tui/src/state.rs:2532-2542`: with `Overlay::None`, `InputMode::Normal` requires
`layout == Workspace && focus != Transcript`. The default layout is `LayoutMode::Chat`
(`crates/tui/src/state.rs:2407`), so the base view is always `InputMode::Composer` and `K`
types a literal `K` into the message box. `ToggleLayout` (`crates/tui/src/reduce.rs:802-806`)
also sets `focus = Pane::Transcript`, so switching to Workspace alone is still not enough.

Driven in a pty against the real binary:

| keys after boot | result |
| --- | --- |
| `Enter`, `K` | a literal `K` appears in the composer; no board |
| `Enter`, `Tab`, `K` | a literal `K`; no board |
| `Enter`, `F2`, `Tab`, `K` | board opens |
| `Enter`, `/board`, `Enter` | board opens |

The palette route is the one that works from a cold start, and it is the one the same doc row
lists in its right-hand column. The bare `K` in the left column is wrong as written.

### F7 — The TUI can only create a title and move columns; the wire and the agent tools support far more — class (b), LOW

`crates/cli/src/tui.rs:3997`:

```rust
Intent::CreateBoardCard { title } => CommandBody::PostBlackboardItem {
    item: BlackboardItemDraft {
        payload: json!({ "title": title, "description": "" }),
        status: Some("todo".to_owned()), assignee: None, ordinal: None, … } }
```

`crates/tui/src/action.rs:809-820` defines exactly three board intents: `WatchBoard`,
`MoveBoardCard`, `CreateBoardCard`. The pane footer confirms the whole surface:
`n create · ←/→ move column · ↑/↓ card · Esc close`.

Meanwhile `UpdateBlackboardItem` carries `assignee`, `ordinal`, and `payload`, and the agent's
`task.update` exposes title/description/assignee/ordinal edits. A human can create a card in
the `doing` column, assign it, reorder it, or rename it **only by asking an agent to do it**.
The daemon-side capability is fully built; the client never sends those fields.

### F8 — Board writes are policy-allowed in every mode, including Plan (note, not a defect claim)

`crates/daemon/src/policy/mod.rs:333-334` routes `TaskWrite` / `TaskRead` to
`eval_blackboard()`, which always permits. In my run the agent was under the Plan-mode
prompt — *"do NOT attempt to write, edit, or patch any files"* — and created a durable board
card anyway. The reasoning in the comment is defensible (the board is Codypendent's own
coordination state, not the filesystem), and the write is traced and attributed. Flagging it
because a reviewer reading a Plan-mode transcript will see a mutation and should know it is
intended.

---

## What I could not exercise, and why

* **Real model behaviour.** I drove a scripted OpenAI-compatible fake, so I verified that the
  four `task.*` tools are advertised with correct schemas, dispatch, execute, and persist —
  but not whether a real model reliably *chooses* them from prose like "break this into
  backlog cards". That is an evals question (outcome 16), not something this pass can answer.
* **`task.update` / `task.move` through a model.** I exercised `task.create` and `task.list`
  end to end through the agent loop, and `task.update`/`task.move` through the equivalent
  daemon write seam (`UpdateBlackboardItem`, which shares `BoardOps::update_card` with the
  agent path — `crates/codypendentd/src/blackboard.rs:616-636`). The two paths differ only in
  the attribution passed in, so I consider the move semantics covered; I did not separately
  script a model turn for them.
* **Multi-writer concurrency.** The `BEGIN IMMEDIATE` fork-proof supersession
  (`crates/workflow/src/blackboard.rs:267-298`) is well-constructed and I confirmed the
  serial refusal (`blackboard.already-superseded` on a second move of a stale id), but I did
  not race two concurrent writers.
* **Board inside a workflow node.** `crates/codypendentd/src/workflow_exec.rs:1176,1205`
  wires the same `AssemblyTaskBoardChannel` and `board_repository` into workflow nodes; I read
  it but drove only plain chat runs, since standing up a full workflow was outside this
  vertical's budget.
* **Remote UI / SDK board surface.** Grepping `crates/daemon/src/remote_ui.rs` and `sdk/` for
  kanban/board/backlog returns nothing, so I believe there is no such surface to exercise —
  but I did not run a Remote UI worker (the sandbox has no `bwrap`, and the daemon logs
  *"Remote UI worker runtime unavailable; component workers fail closed"*).
* **Environment note.** A sibling reviewer's `pkill codypendentd` repeatedly killed my daemon,
  and the shared scratch filesystem filled up mid-run; several probes had to be re-run. All
  results reported above are from completed runs, not partial ones.

## How to reproduce

Probes live in
`/tmp/claude-0/-home-user-codypendent/5b8351f1-c73f-5de3-981c-c56d73b7a138/scratchpad/boardrev/`
(nothing was added to the repo):

| file | what it shows |
| --- | --- |
| `probe.py` | minimal length-prefixed-JSON protocol client |
| `t1_board.py` | post / read / move / supersede / invalid column / unknown scope |
| `t2_scope.py`, `t3_paths.py` | F2 (cross-repo access, Observer), F3 (hidden kinds), path variants |
| `t4_agent.py` + `fakemodel.py` | the 17-tool list a model receives; `task.create` / `task.list` executing |
| `t5_noncanon.py` | F1 — the forked board, demonstrated |
| `t6_byid.py` | F4 — by-id mutation of a list-hidden item; `finding` rewrite at run scope |
| `tui_drive.py` | pty-driven real TUI; the rendered board frame and the `K` key table |

Run with `CODYPENDENT_DATA_DIR=<tmp>/data CODYPENDENT_SOCKET=/tmp/cpbrd.sock` against
`target/debug/codypendentd`, plus a `models.toml` in `<tmp>/data/` pointing at `fakemodel.py`.

---

## The structural pattern

**The board's storage layer is disciplined; its identity and its surfaces are not.**

Everything below `BlackboardStore` is careful work: fork-proof supersession in one immediate
transaction, evidence-required kinds, server-built attribution on both the human and agent
paths, persist-before-publish fan-out, one `BoardOps` shared by the TUI and the tools so a
human's move and an agent's `task.move` are literally the same write. That layer earns its
keep, and I could not break it.

What is missing is a **server-side notion of *which board***. `board_scope_id` is
`format!("board:{repository}")` over a string the caller chose, and every layer above trusts
the previous one to have canonicalized and authorized it — the protocol's doc comment says
callers do it, the daemon assumes the client did, and two of the shipped clients do while a
third does not. The result is one root cause wearing three faces: cards land on a board the
user cannot see (F1), any client can touch any board (F2), and a mistyped or unusual path
mints a fresh empty board rather than erroring (F2). The same trust-the-caller reflex appears
in the kind dimension: the write path accepts anything, the read path filters to `task`, and
nobody reconciles them (F3), including on the by-id path that skips the filter entirely (F4).

And where the storage layer built more than the surfaces consume, the surplus was never wired:
a complete revision-lineage walker with no reader, a wire type carrying assignee/ordinal/payload
that the TUI never populates, and no CLI entry point at all (F5, F7). The engine is ahead of
the product on this vertical — which is the cheap kind of gap, and the one worth closing first.
