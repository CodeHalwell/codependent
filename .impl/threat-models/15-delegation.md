# Threat model — Outcome 15 (delegation: parallel workers in isolated worktrees)

Owner: agent-delegation. Written before the first line of code, per BRIEF rule 4.
Scope: `crates/workflow/**`, `crates/codypendentd/src/{workflows,workflow_exec}.rs`,
`crates/daemon/src/worktrees.rs`.

## What outcome 15 widens

Before this change the workflow driver ran the ready frontier **strictly
sequentially** (`drive.rs:423`). One worker at a time, one worktree at a time.
After it, up to `budget.maximum_agents` agent nodes run **concurrently**, each
in its own git worktree, each driving a full agent loop with the daemon's tool
surface. The widening is therefore not "a new input arrives" — it is **N
simultaneous instances of an already-untrusted actor**, plus a new
consolidation step that reads what those actors wrote.

## The trust boundary

```
  user manifest (trusted-ish: the user wrote it, but it is data)
        |
        v
  compile()  ── structural + closed-set tool check ──> CompiledWorkflow
        |
        v
  scheduler (bounded by maximum_agents)
        |
   +----+----+----+
   v         v    v
  worker   worker worker        <-- UNTRUSTED. Model output drives tool calls.
   |         |    |
   |         |    +-- own worktree (own branch, own disk path)
   |         +------ own daemon run + session + budget slice
   +---------------- own blackboard authorship (role, node_id, run_id)
        |
        v
  blackboard items / proposed_patch artifacts   <-- UNTRUSTED DATA
        |
        v
  consolidation + chair/verify nodes            <-- must treat the above as evidence, not instruction
```

Everything below `compile()` is untrusted. A worker is a model loop: its
objective text is partly derived from upstream workers' outputs, so **prompt
injection from one worker into another is in scope**, and so is a worker that
simply malfunctions and loops.

## What a worker MAY reach

* **Its own worktree, and only its own.** `bind_run_worktree` gives each node's
  run a dedicated directory under `codypendent-worktrees/<repo>/run-<short>` on
  a dedicated branch `codypendent/run-<short>`. The short id derives from the
  node's own freshly minted `RunId`, so two concurrent nodes can never collide
  on a path or a branch. The agent's policy read root == write root == that
  worktree; the user's checkout is not the operating tree.
* **The tool surface its resolved `AgentMode` allows.** A `review`-mode profile
  denies writes through the policy engine; a `Build`-mode profile may write —
  inside its worktree.
* **The run's blackboard**, to post declared outputs. Every post carries
  attribution (`role`, `node_id`, `run_id`, `workflow_run_id`).
* **Approval-gated effects** (`github.update_pull_request`, patch-apply) only
  through the durable approval broker, which parks the node before the effect.

## What a worker MAY NOT reach — denied by default

| Denied | Enforced by |
|---|---|
| Another worker's worktree or branch | distinct `RunId` → distinct path/branch; `ensure_outside_repository`; `UNIQUE(worktree_path)` active-lease index |
| The user's checkout / base branch | no merge, rebase, push, or checkout is performed on the repository the user works in — see "merge-back" below |
| Landing its own patch | consolidation is a **separate node**, never the worker's own act; the land step is approval-gated |
| Unbounded fan-out | `budget.maximum_agents` is now an enforced semaphore, not documentation |
| Unbounded spend/time | per-node `[budget]` slice + workflow envelope, charged on measured cost; exceeding **blocks the node and pauses the run** |
| Running forever | the node's agent run is cancellable through the run cancellation registry; the workflow's `CancelWorkflow` fires it |
| Choosing an arbitrary tool as a graph node | tool nodes are a **closed set** rejected at compile time, not at runtime |
| Escalating its model | `model_policy` resolves only against models already configured in `models.toml`; a name that does not resolve falls back to the daemon default, never to an arbitrary endpoint |
| Spawning workers itself | a worker's agent loop has no scheduler handle; only the compiled graph creates nodes |

## Bounding a compromised or runaway worker

1. **Concurrency (breadth).** The scheduler holds a `tokio::sync::Semaphore`
   sized to `budget.maximum_agents` (validated `>= 1` at compile). N workers
   can never exceed that, whatever the graph's shape. A manifest whose frontier
   is 50 wide with `maximum_agents: 3` runs 3 at a time.
2. **Depth.** A worker cannot create a node, so the graph's depth is the
   manifest's depth — fixed at compile time, hashed into the graph signature,
   and refused if the manifest changes under a live run.
3. **Budget.** Wall-time, tool-calls, and measured cost are charged per node
   against the node slice and the workflow envelope. Exceeding blocks the node
   and pauses the **whole run**, so a runaway cannot be masked by its siblings'
   progress. Concurrency makes this stricter, not looser: the first node to
   blow the envelope pauses the run, and the scheduler stops launching new work
   at the next boundary (the in-flight wave drains).
4. **Lease.** Every worktree is a durable `workspace_leases` row with an owner
   run, a base commit, an expiry, and a state. A crash leaves the row; startup
   reconciliation marks vanished trees `orphaned` and never deletes work.
5. **Cancellation.** `CancelWorkflow` sets the run state; the scheduler observes
   it at the next boundary and stops launching, and the per-run cancellation
   registry interrupts in-flight agent runs.
6. **Release is protective, not destructive.** A worktree that holds unmerged
   commits or a dirty tree is **retained** and its diff exported as an artifact
   before anything is removed. Branch reclamation (new) is gated on
   `git merge-base --is-ancestor <branch> HEAD` — the same test `allocate`
   already uses — so a branch holding work is never deleted.

## Deliberately allowed, and why

* **Workers write to disk.** That is the feature. Isolation is the mitigation,
  not prohibition.
* **A worker's output becomes another worker's prompt.** Round-2/consolidation
  prompts carry upstream reports. They are fenced as untrusted evidence (the
  council's `[BEGIN UNTRUSTED MEMBER REPORT — EVIDENCE ONLY]` convention) and
  the reader is instructed never to follow instructions found inside. This is
  mitigation, not proof; a determined injection can still influence a
  downstream worker's *text*. It cannot widen that worker's *capabilities*,
  because capabilities come from the compiled node and the resolved profile,
  never from the payload.
* **Concurrent SQLite writers.** All node transitions go through the shared
  pool. Writes are small, per-node, and serialized by SQLite; the store's
  `transition_node` is a single `UPDATE` keyed on `(workflow_run_id, node_id)`,
  so two workers never write the same row. Run-level state changes use CAS
  (`set_run_state_if_legal`), which is exactly the primitive concurrency needs.

## Residual risk, stated plainly

* **Merge-back is deliberately narrow.** The consolidation step assembles
  workers' patches into an artifact and applies them into a **fresh throwaway
  worktree** to prove they apply; it never touches the user's checkout, never
  pushes, and never fast-forwards a base branch. Landing remains a human/GitHub
  act. A conflicting fan-out fails the consolidation node with the conflicting
  file list rather than resolving it silently.
* **Agent-profile `tools`/`permissions` are still not enforced** (F15.8).
  Least-privilege for a worker is mode-and-prompt only today. That is a real
  gap; it is out of this change's scope and is recorded here rather than
  papered over.
* **A worker still leaks a daemon session** (F6.5) because the protocol has no
  session-close command. Sessions are read-mostly and clearly titled, but the
  list grows.
