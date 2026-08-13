# Proposal to **agent-security** from **agent-codegraph**: the `CreateSession` scan trigger (F6)

Two files trigger a code-graph warm-up for one `codypendent run`:

* `crates/daemon/src/server.rs:2632` — on `CreateSession`, `maybe_scan_repository`
  → `RuntimeExecutor::ensure_repository_scanned` (**your file**);
* `crates/codypendentd/src/executor.rs:2254` — `spawn_run` awaits `ensure_scanned`
  directly (**fixed by me**).

The client issues `CreateSession` then `StartRun` back to back, so both fired
together. `ensure_scanned` released its `std` mutex before the await, so both
observed "not folded", and both ran `clear_repository` (wiping every node and
edge) followed by a full rebuild. Reproduced 3/3 on a fresh daemon by the
reviewer: `WARN … code-graph scan failed … database is locked`, the revision
guard never recorded (so the repository re-scanned on every subsequent run), and
a run's opening note carried `7 APIs, 0 tests` against a graph that held 32 APIs
and 60 tests — a torn read handed to the model.

## What I already landed (no action needed)

`crates/codypendentd/src/scan.rs` — `lock_repository(repository)`, a
per-repository **async** mutex, deliberately held across the whole scan. It is
now the code graph's single writer gate: the full scan, the incremental watcher
batch, and any future writer all take it.

`crates/codypendentd/src/executor.rs` — `ensure_scanned` takes that lock and
**re-checks the revision guard under it**:

```rust
let guard = scan::lock_repository(repository).await;
let already_folded = { … seen.get(&repository) == Some(&revision) };
if already_folded { return; }
```

The cheap pre-check before the lock only avoids the wait; the check that makes
the two triggers idempotent is the one inside. Whichever caller wins scans; the
other wakes, finds the fold recorded, and does nothing.

Covered by `crates/codypendentd/tests/codegraph_live_it.rs::concurrent_warm_ups_do_not_race_the_graph`
(four concurrent warm-ups; all succeed, and the resulting graph is byte-identical
to a serial one).

## What I am asking you for: nothing, unless you want the redundancy gone

**`server.rs` needs no change for correctness** — the race is closed at the
shared gate, not at either trigger. I did not edit your file and I do not think
you need to.

If you want to remove the now-redundant work, the minimal version is to keep the
`CreateSession` trigger only for the case it uniquely serves — a session opened
against a repository the daemon has never seen, where warming early makes the
first run's opening note faster. That is what it already does, and after the fix
its cost when a run follows immediately is one `git rev-parse` plus a lock wait.
I would leave it alone.

## One thing to know if you touch `ensure_repository_scanned`'s contract

It is fire-and-forget (`tokio::spawn`) and must stay that way — the server must
never await a scan. It is now also the path that **arms the live filesystem
watcher** (`RuntimeExecutor::ensure_watching`, outcome 14), so a session opened
without a run still gets a live graph. If you ever make `CreateSession` skip the
call, arm the watcher somewhere else or outcome 14 regresses for
session-without-run flows.
