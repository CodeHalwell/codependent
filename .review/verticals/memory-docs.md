# memory-docs vertical

Reviewer scope: outcome **17** (compounding memory) and outcome **18** (docs round-trip).
Pinned commit `535a2f5e3848b256536ddee94883dc0010ecdcb8` (v0.4.5). Read-only pass.

---

## Verdicts

**OUTCOME 17: PARTIAL** — memory genuinely persists across sessions with real
provenance and genuinely reaches the model prompt (proven live, below), but a user
can only *look* at it: there is no delete, no edit, and no protocol command at all;
the only "decay" is a fixed 365-day read-time cut; there is no short→long promotion;
and the one store that *does* have promotion, expiry and conflict review
(`learning_records`) is written on every run and never read into any run's context.

**OUTCOME 18: PARTIAL** — approving a document really does write, commit and record
a publication (verified end-to-end for two of three targets), but the PR half is
reachable only from the CLI, pushes to the remote *before* it checks it can open a
PR, tells the user "still executing" when it has permanently failed, and **nothing
anywhere reads PR merge status back** — that half is ABSENT, down to the schema.

---

## How I exercised it

Isolated environment: `CODYPENDENT_DATA_DIR=/tmp/mdrev/data`,
`CODYPENDENT_SOCKET=/tmp/mdrev/d.sock`, fresh git checkout at `/tmp/mdrev/repo`
(one `src/lib.rs` with `pub fn charge_customer()`), `codypendentd` from
`target/debug`. Prebuilt artifacts reused; nothing rebuilt, nothing cleaned.

### Publish, live

```
$ codypendent docs new "Payments architecture" --from seed.md
Created "Payments architecture" (019ff87e-b652-75d3-b5a9-3bfda20504bd).

$ codypendent docs publish 019ff87e-… --target repo-file --yes
  target: repository file docs/payments-architecture.md
  git action: write docs/payments-architecture.md in the working tree (approval-gated change set)
Parked approval 019ff87e-e322-7db0-adc8-13a62e49fb73.
Published … -> commit d4b26266dff88f3b1b3986e8609cc465dd09110e
```

sqlite before: `documents 0, document_publications 0, document_publish_jobs 0,
approvals 0, runs 0`. After:

| table | row |
|---|---|
| `documents` | `status=published`, `revision=1`, `links_json=[]` |
| `document_publications` | `revision=1, git_commit=d4b26266…, rendered_hash=da92cf94…` |
| `document_publish_jobs` | `state=completed` |
| `approvals` | `action_json = {"type":"PublishDocument","changed_files":["docs/payments-architecture.md"],…}` |
| `runs` | `objective="write docs/payments-architecture.md …", state=Completed, model_policy=docs-publish` |

`git log` gained `d4b2626 docs: publish docs/payments-architecture.md`; the file is
on disk with the deterministic render. `--target docs-branch` likewise produced
`3ccee12` on branch `docs/mybranch` via a scratch worktree, leaving `master`
untouched. **The `PublishPlan` chain is complete and does execute.** The plan is
built at `crates/knowledge/src/docs/render.rs:236`, serialised into
`document_publish_jobs` at `crates/codypendentd/src/publish.rs:409`, awaited at
`:309`, executed at `:497`, and recorded at `crates/knowledge/src/docs/render.rs:265`.
It is not an unexecuted plan.

### Memory across sessions, live

Two headless runs, two *different* sessions, same DB and repo:

- run 1, session `019ff882-4c44-7fb1-8d53-af8de9b31222` → context note contains
  `=== MEMORIES ===\n(none)`. The run failed ("no model configured"), and the
  failure was harvested.
- run 3, session `019ff882-5071-7980-bfed-fd3ee2e6243d` → context note contains
  `=== MEMORIES ===\n- Run failed: no model configured (no models.toml)
  (confidence 0.60, rev seq:00000000000000000006; source: artifact 019ff882-4caa-…)`

The stored row is `scope_tier=repository`, `scope_key=2d865f1a-…`,
`provenance_json=[{"kind":"artifact","artifact":{"id":…,"sha256":"99122efb…"}}]`,
`retention_json={"ttl_days":365}`.

**Cross-session memory works and reaches the prompt.** The mechanism is
`crates/codypendentd/src/executor.rs:1745-1785`: candidates are extracted under the
*session* scope (so evidence can name the session) then re-anchored to
`Scope::Repository`, and `emit_context` (`:1367-1372`) queries
`[System, local_user, Repository]` — no session filter anywhere. Provenance is real
(artifact id + sha256, or `session … events N–M`), never a placeholder.

---

## Findings

### F1 — Repository identity is re-derived from the CWD, so every human-facing knowledge list silently shows nothing outside the repo root. Class (c)

`crates/cli/src/commands.rs:760-761`
```rust
let repository =
    codypendent_knowledge::stable_repository_id(&std::env::current_dir()?.canonicalize()?);
```
The daemon writes under `codypendentd::scan::repository_id_for`
(`crates/codypendentd/src/scan.rs:178-182`), which resolves the **git toplevel**
first. `codypendent-knowledge` even exports the correct helper —
`anchor_repository_id` (`crates/knowledge/src/skills.rs:65`), whose own doc comment
says *"a mismatch here would make the installed skill silently invisible"* — and
`commands.rs:14` already imports it. It is simply not used here.

Live, same daemon, same database, one directory apart:
```
$ cd /tmp/mdrev/repo   && codypendent docs list
019ff87e-b652-75d3-b5a9-3bfda20504bd   published     1  Payments architecture

$ cd /tmp/mdrev/repo/src && codypendent docs list
No documents yet. Create one with `codypendent docs new "<title>"`.
```
`stable_repository_id("/tmp/mdrev/repo") = 2d865f1a-4f95-8414-1525-667c3617e2ff`
vs `…/src` → `f1697e63-3894-c81b-39de-943a31c777ce`. A trailing slash also changes it
(`83a0ed05-…`).

The same wrong derivation feeds the **TUI's Memory browser, Docs Studio and Learning
Journey**: `crates/cli/src/tui.rs:5906`, `:2766`, `:2950`, `:6768` all call
`stable_repository_id(repo)` rather than `anchor_repository_id(repo)`.

User types `codypendent docs list` (or opens `M`) from `src/` and is told, flatly,
that they have no documents and no memories. The real answer is "filtered by a key
you computed differently from the writer". This is the brief's SILENT FILTER pattern
in its purest form.

### F2 — The governed learning ledger is written on every run and read by nothing except a browser. Class (b)

`crates/knowledge/src/learning.rs` is the store that actually implements outcome 17's
vocabulary: promotion (`LearningState::Proposed → Active`, `activate()` at `:661`),
decay (`expires_at`, `is_retrievable()` at `:238`), explicit contradiction resolution
(`conflict_key`, `find_conflicts()` at `:1062`, `ActivationOutcome::Conflict` at `:338`),
and typed provenance with a trust tier (`permits_auto_activation()` at `:171`).

It is populated from `crates/codypendentd/src/learning_capture.rs:44`, called by
`crates/codypendentd/src/executor.rs:1630` after every completed run.

`LearningStore::query` (`crates/knowledge/src/learning.rs:510`) has exactly **one**
production call site: `crates/cli/src/tui.rs:6762` (the TUI's Journey browser).
`LearningRecord::is_retrievable` — the method whose entire purpose is gating
injection into a run — has **zero** production call sites; only
`crates/knowledge/tests/learning_it.rs:104,129,356,385`.

Context assembly (`crates/knowledge/src/context.rs:297`) reads `MemoryStore` and
nothing else. So: a user says "always use cargo nextest instead of cargo test", the
daemon captures it as an **Active** repository-scoped learning with
`UserStatement` provenance (verified by `learning_capture.rs:342` test), the TUI shows
it as active — and no run ever sees it. The engine with decay, promotion and
contradiction review is not attached to the prompt; the engine that *is* attached has
none of those properties.

### F3 — Memories can be read but never edited or deleted, by anyone. Class (b)

`MemoryStore::forget` (`crates/knowledge/src/memory.rs:261`) and `forget_scope`
(`:287`) — documented in the module header as the Chapter-06 right-to-forget with a
content-free `ForgetAudit` — have **zero** production callers. Only
`crates/knowledge/tests/memory_it.rs:427,495`.

There is no `MemoryStore` update/edit method at all.

There is no memory command in the protocol: `crates/protocol/src/command.rs` contains
no memory variant (grep for `Memor` returns one doc-comment at `:162`). The TUI's
memory surface is `Action::OpenMemory` and `Action::RevealSource` only
(`crates/tui/src/action.rs:314-318`) — the whole action set. The browser reads SQLite
directly from the CLI process (`crates/cli/src/tui.rs:5912`).

User sees a wrong or embarrassing memory in the browser, presses every key, and
cannot remove it. The only route is `sqlite3` on `codypendent.db` by hand.

By contrast the learning ledger *is* fully editable from the TUI
(`crates/cli/src/tui.rs:6824-6900`: activate / reject / pin / edit statement / delete).
The split is exact and inverted: **the store you can edit is never read into a run;
the store that is read into every run you cannot edit.**

### F4 — `provenance_cards` — the "every retrieved memory opens its source" projection — is only ever read by a test. Class (b)

`crates/knowledge/src/memory.rs:592`, exported at `crates/knowledge/src/lib.rs:85`.
Its only consumer is `crates/knowledge/tests/memory_it.rs:542`
(`provenance_cards_open_every_source`). Both real surfaces re-implement a lossy
string instead — `crates/knowledge/src/context.rs:452 format_source` and
`crates/cli/src/tui.rs:6924 evidence_source` — producing
`"artifact 019ff882-4caa-7450-99de-34a8a3873b48"`. That is an opaque UUID: the TUI's
`RevealSource` shows the same string in full, and there is no path from it to the
stored artifact bytes. Provenance is *recorded* truthfully and *displayed* as an
unopenable label. Data produced, never consumed.

### F5 — The contradiction gate collapses two unrelated, both-true facts and silently drops one. Class (c)

`crates/knowledge/src/memory.rs:654`:
```rust
fn contradicts(existing: &str, candidate: &str) -> bool {
    let (es, ev) = subject_and_value(existing);   // splits on the FIRST " is " / ": " / " = " / "="
    ...
    ev.is_some() && cv.is_some() && !es.is_empty() && es == cs && ev != cv
}
```
Every harvested failure lesson is `"Run failed: <reason>"`, so `subject_and_value`
yields subject `run failed` for all of them. Two *different* failures therefore
"contradict". Reproduced live: after the second failing run,

```
id 019ff882-4cb0… 'Run failed: no model configured (no models.toml)'
                  valid_until='seq:00000000000000000006'      <- superseded, gone from the live view
id 019ff883-5784… 'Run failed: no model configured: every candidate failed for Build:
                   unreachable-local: connection check to `http://127.0.0.1:59999/v1` failed…'
                  supersedes=["019ff882-4cb0…"]
```

The first lesson is now invisible to every future run. The user is never told a
contradiction was detected or resolved: the executor emits a bare
`"remembered: <new statement>"` note (`crates/codypendentd/src/executor.rs:1806-1815`)
identically for `Curation::Accepted` and `Curation::Superseded`. Outcome 17 asks for
*explicit* contradiction resolution; this is implicit, lossy, and over-eager.

### F6 — Memory revisions are per-session ledger sequences, so "as of revision X" is meaningless across sessions. Class (c)

`valid_from` is `Revision::sequence(seq)` where `seq` is the *event sequence inside one
session's ledger*, which restarts at 1 per session
(`crates/knowledge/src/observer.rs:92`, used from `crates/codypendentd/src/executor.rs`).
Both memories above carry `valid_from = seq:00000000000000000006` from two different
sessions. `MemoryStore::query` (`crates/knowledge/src/memory.rs:148-198`) compares
these as an ordered text range —
`valid_from <= ? AND (valid_until IS NULL OR ? < valid_until)` — and
`MemoryError::NonOrderableRevision` (`:67`) exists specifically to guarantee the
comparison is meaningful. It guarantees the *format* is orderable; it cannot make the
*values* comparable across sessions. A revision-pinned query is a silent lottery.
(No production caller passes `at_revision = Some(..)` today, so this is currently
latent — but it is the mechanism the temporal-validity invariant rests on.)

### F7 — The documentation-PR target pushes to the remote before it checks it can open a PR. Class (c)

`crates/codypendentd/src/publish.rs:523-543`:
```rust
let github = github.ok_or(PublishExecError::NoGitHubClient)?;          // 528 — checked first, good
let sha = commit_on_docs_branch(repository_root, branch, path, &plan.rendered).await?;  // 529
run_git(repository_root, &["push", "origin", &format!("{branch}:{branch}")]).await?;    // 530
let repo = crate::executor::resolve_github_repo(repository_root)
    .await.ok_or(PublishExecError::NoGitHubRemote)?;                   // 535-537 — checked LAST
```
Reproduced live with a non-GitHub `origin` (a local bare repo):

```
$ codypendent docs publish … --target doc-pr --path docs/pr2.md --branch docs/pr2 --yes
Publish approved; the daemon is still executing it in the background. …
$ git --git-dir=/tmp/mdrev/origin.git branch -a
  docs/pr2                       <- pushed
$ sqlite: document_publish_jobs → state='failed'
daemon.log: WARN publish approved but execution failed error=could not resolve a
            GitHub owner/repo from the checkout's `origin` remote (documentation PR target)
```

A branch was created and pushed to the user's remote; no publication row records it;
the document status was not advanced; the user was told it was still running.
Moving `resolve_github_repo` up next to the `NoGitHubClient` check costs nothing and
makes the failure a no-op. (With *no* credentials at all the ordering is correct:
`NoGitHubClient` fires before any commit — verified, no `docs/pr3` branch was created.)

### F8 — The CLI reports "still executing" for a publish that has permanently failed, while holding the row that says so. Class (c)

`crates/cli/src/commands.rs:925-936`:
```rust
let existing = publications(&pool, document_id).await?.len();
match wait_for_new_publication(&pool, document_id, existing).await {
    Some(publication) => println!("Published … -> commit {}", …),
    None => println!("Publish approved; the daemon is still executing it in the background. \
                      Check the daemon log, or re-run `codypendent docs publish` shortly …"),
}
```
`wait_for_new_publication` (`:1101`) polls only `document_publications`. It never reads
`document_publish_jobs.state` — which by then already says `failed`, in the same pool
this function has open. Both failing runs above printed the reassuring message. The
only place the truth exists is `daemon.log`. A user who follows the advice and re-runs
gets the same message forever.

### F9 — Nothing reads PR merge status back into a document. Class (a), ABSENT

Searched `merge`, `merged`, `merge_status`, `pr_state`, `mergeable`, `check_run`
across `crates/`. Three independent confirmations:

1. **No model field.** `crates/integrations/src/github/model.rs:23-47` — `PullRequest`
   has `number, title, body, state, draft, html_url, head, base`. No `merged`, no
   `merged_at`, no `merge_commit_sha`. `state` is only `open`/`closed`.
2. **No stored handle.** `crates/codypendentd/src/publish.rs:671-687
   open_documentation_pr` calls `create_draft_pull_request` and **discards the
   returned `PullRequest`** (`… .await?; Ok(())`). The PR number and URL are never
   persisted, so there is nothing to poll later.
3. **No schema column.** `migrations/0008_phase4_documents.sql:66-75`
   `document_publications` is `(id, document_id, revision, target, git_commit,
   rendered_hash, published_at)`. No PR number, no PR state, no merge state, and no
   later migration adds one.

`GitHubApi::get_pull_request` / `list_check_runs` exist and have exactly one caller
each: the agent tool path (`crates/runtime/src/agent.rs:3615`, `:3626`), driven by a
model deciding to call `github.get_pull_request`. Nothing connects that to a
`DocumentId`. Outcome 18's "merge status reflects back into the Docs Studio" has no
implementation at any layer.

### F10 — The Docs Studio can only publish to a working-tree file; the PR target is CLI-only. Class (b)

`crates/tui/src/reduce.rs:4674` and `:8452` are the only constructors of a publish
intent in the TUI, and both hard-code
`codypendent_protocol::PublishTarget::RepositoryFile { path }`. There is no overlay,
prompt or palette entry for `DocsBranchCommit` or `DocumentationPr`
(`Overlay::DocPublishPath` collects a path only). The engine supports all three;
the studio exposes one. A user working in the Docs Studio — the surface outcome 18
names — can never produce a PR.

### F11 — The publication record is written and then shown to nobody. Class (b)

`document_publications` rows are read by exactly two things:
`crates/cli/src/commands.rs:925,1101` (the `docs publish` wait loop) and tests
(`crates/codypendentd/src/publish.rs:1043,1257,1337,1694`). The Docs Studio's loader
`load_docs` (`crates/cli/src/tui.rs:6573-6620`) reads documents and pending
suggestions only. `DocCard` (`crates/tui/src/state.rs:1186-1206`) has
`status, mode, revision, blocks, suggestions` — no commit, no target, no publish
history. The `(revision ↔ commit)` pairing that STEP 4.4 exists to record is never
displayed.

### F12 — The `published` badge keeps lying after the document is edited. Class (c)

`set_status` (`crates/knowledge/src/docs/store.rs:418`) has one production caller:
`crates/codypendentd/src/publish.rs:393`, setting `Published`. `InReview` and
`Archived` are never set by any production code — the "frozen for review before
publication" lifecycle stage in `crates/knowledge/src/docs/model.rs:22-27` is dead.
And `write_document_tx` (`crates/knowledge/src/docs/store.rs:489`) binds
`doc.status.as_str()` unchanged, so editing a published document leaves it
`published` at revision N+1 while `document_publications.revision` still says N.
Nothing joins the two. The Docs tree shows `published` for a document whose content
no longer matches any commit.

### F13 — The accessible client cannot inspect memories at all. Class (b)

`crates/tui/src/accessible.rs:815` maps `Overlay::Memory { .. }` to the string
`"memory"` and the renderer emits only:
```
Open dialog: memory
Controls: up, down, Enter, Esc, help, quit
```
Driven live (`printf 'esc\nenter\n/\nmemor\nenter\n' | codypendent --accessible`) with
two memories in the database: no statements, no sources, no rows. The graphical TUI
does render them (`crates/tui/src/render.rs:5291-5410`, including the provenance
card), so this is specific to the screen-reader client. For that user, outcome 17's
"user-inspectable" is false outright.

### F14 — Memory does not participate in retrieval (the outcome-9 dependency). Class (a)

`crates/knowledge/src/context.rs:297-310` retrieves tool/skill cards through the
hybrid BM25+vector funnel, then takes memories by **recency truncation**:
```rust
let records = MemoryStore::new().query(pool, scopes, None).await?;
let dropped = records.len().saturating_sub(MAX_CONTEXT_MEMORIES);   // 32, line 318
```
The comment at `:320` says so plainly: *"retrieval-ranked memory selection is Phase 7+
territory — until then recency is the only defensible ordering."* Outcome 17 is
specified as building **on 9**; the memories that reach the prompt are the newest 32,
not the most relevant, and the objective text is never used to select them. Memories
are also never embedded — `KnowledgeIndexEvent::MemoryChanged` is enqueued
(`crates/knowledge/src/memory.rs:100`) but the daemon's indexer reported
`embedded=0` for all 22 drained rows in my run.

### F15 — "Decay" is one fixed 365-day read-time cut; rows are never removed. Class (b)

`RetentionPolicy::default` is `ttl_days: Some(365)`
(`crates/knowledge/src/types.rs:380-386`) and every production candidate passes
`retention: None` (`crates/knowledge/src/observer.rs:211,337,396`,
`crates/codypendentd/src/executor.rs:3375,3531`,
`crates/runtime/src/extractor.rs:166`) — the sole exception is the completed-run
breadcrumb's `BREADCRUMB_TTL_DAYS = 30` (`observer.rs:63`), and those statements are
themselves rejected as low-value at `crates/knowledge/src/memory.rs:461-467`. Expiry
is applied only as a read filter (`memory_is_retained`, `:446`, called from `query` at
`:206`); no sweep ever deletes an expired row, so the table grows monotonically with
invisible content. Confidence never decays — `confidence` is written once at
`OBSERVED_CONFIDENCE = 0.6` (`observer.rs:45`) and never updated by any code path.
`MemoryClass::Working` (`crates/knowledge/src/types.rs:310`) — the natural short-term
tier — is never constructed anywhere; its only mention is a TUI label
(`crates/cli/src/tui.rs:6993`). **There is no short→long-term promotion for memories.**

### F16 — The TUI writes `learning_records` directly into the daemon's live database

`crates/cli/src/tui.rs:6824-6900` (`mutate_learning`) calls
`LearningStore::activate/reject/set_pinned/edit/delete` on a pool the CLI opened
itself (`knowledge_db::open`, `crates/knowledge/src/db.rs:18`, read-write,
`create_if_missing`). A second process mutates governed state while the daemon runs:
no session event, no ledger entry, no `LearningsCaptured`-style projection, and no
participation in the daemon's transaction discipline. WAL makes it mostly survivable;
it is still a write path that bypasses the server entirely. Noting it because F3's
"no protocol command" is the same gap seen from the other side.

---

## What I could not exercise, and why

- **A run with a live model.** No provider credentials and no reachable endpoint in
  this container, so I could not observe the `memory.remember` tool
  (`crates/runtime/src/tools/memory.rs`) or the model-backed `FactExtractor`
  (`crates/runtime/src/extractor.rs`) producing candidates. I verified the harvest
  path instead through the *failure* branch, which exercises the same
  `extract_candidates → curate → insert` chain (`executor.rs:1740-1815`) and proved
  cross-session visibility. `memory.propose:` note parsing
  (`crates/knowledge/src/observer.rs:286-320`) is covered by its own unit tests but I
  did not drive it live.
- **A real GitHub PR.** No usable token (`GITHUB_TOKEN=proxy-injected` is a
  placeholder; `gh` is not installed) and no github.com-resolvable `origin`. I
  exercised both credential branches of `execute_plan` — `NoGitHubClient` (fails
  before any write) and `NoGitHubRemote` (fails *after* commit+push, F7) — but the
  actual `create_draft_pull_request` call and its hidden-marker idempotency
  (`crates/codypendentd/src/publish.rs:681-686`) were not hit against a live API.
  Note that startup logs `github personal-mode client enabled` purely on the presence
  of a non-empty token string (`crates/integrations/src/github/secret.rs:37-42,61-66`)
  — there is no validation, so an invalid token yields an "enabled" client that fails
  only at call time.
- **The graphical TUI's Memory browser and Docs Studio.** No pty; the accessible
  client (F13) renders the memory overlay as an empty stub, so I read
  `crates/tui/src/render.rs:5291-5410` and `crates/tui/src/reduce.rs:4655-4682`
  directly for the behaviour instead.
- **`document_leases` / suggestion concurrency under real multi-client load.** Single
  client only; the lease and mode gates
  (`crates/codypendentd/src/documents.rs:103-113`,
  `crates/knowledge/src/docs/apply.rs`) are covered by their own integration tests
  which I read but did not re-run.
- **Blocked once by an unrelated daemon defect** (not my vertical, reporting for the
  orchestrator): `codypendent run` intermittently fails with
  `CreateSession rejected: attempted to call begin_with at non-zero transaction depth
  (internal.command-apply-failed)` — reproduced twice out of four attempts against a
  freshly started daemon, apparently racing the startup code-graph scan. It cost me
  two attempts before a run went through.

---

## The pattern

Every engine in this vertical was built to spec and wired to *a* caller — but the
caller is almost always the machine, never the person. The write paths are complete
and correct end to end; the read paths stop exactly one layer short of a human.
Memory is curated, deduped, superseded and injected — and has no delete command.
The learning ledger has promotion, expiry and conflict review — and no reader.
`provenance_cards` renders the source projection — for a test. `document_publications`
records the commit — and no view shows it. PR merge status has no consumer, no stored
handle, and no column. And in the one place a human-facing read path *does* exist, it
re-derives the repository identity itself instead of asking the server that wrote the
rows — so it disagrees with the writer one directory away and reports "none" with
complete confidence.
