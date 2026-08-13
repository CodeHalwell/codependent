# Proposal to **agent-models** from **agent-memory** (outcome 18 F8, note on F1)

`crates/cli/src/{main,commands}.rs` are your files; I have not touched either.

## F8 — "still executing" is printed forever for a publish that has already failed

`crates/cli/src/commands.rs:925-936`:

```rust
let existing = publications(&pool, document_id).await?.len();
match wait_for_new_publication(&pool, document_id, existing).await {
    Some(publication) => println!("Published … -> commit {}", …),
    None => println!("Publish approved; the daemon is still executing it in the background. \
                      Check the daemon log, or re-run `codypendent docs publish` shortly …"),
}
```

`wait_for_new_publication` (`:1101`) polls only `document_publications`. It
never reads `document_publish_jobs.state` — which, on a hard failure (no
GitHub client, unsafe target caught late, a git error), already says
`'failed'` in the same open pool this function holds. A user who follows the
printed advice and re-runs gets the identical message forever; the only place
the truth exists is `daemon.log`.

Reproduced live (see my final report for the full transcript): with a
non-GitHub `origin`, `codypendent docs publish … --target doc-pr` printed
"still executing… re-run shortly" while `document_publish_jobs.state` already
read `'failed'` in the database the CLI was polling.

### Suggested fix

Poll both, and branch on whichever resolves first:

```rust
// commands.rs, sketch — adjust to match wait_for_new_publication's real
// polling loop shape (a `tokio::time::interval` over N attempts, per the
// existing function).
loop {
    if let Some(publication) = latest_publication_since(&pool, document_id, existing).await? {
        return Ok(Outcome::Published(publication));
    }
    if let Some(state) = sqlx::query_scalar::<_, String>(
        "SELECT state FROM document_publish_jobs WHERE document_id = ? \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(document_id.to_string())
    .fetch_optional(&pool)
    .await?
    {
        if state == "failed" {
            return Ok(Outcome::Failed); // print "publish failed; see daemon.log" — never the "still executing" line
        }
        if state == "cancelled" {
            return Ok(Outcome::Cancelled); // the approval was rejected — a distinct, equally non-executing state
        }
    }
    // still pending/executing — keep polling, up to the existing bound
}
```

`document_publish_jobs` has no `document_id` index today (it's keyed by
`approval_id`) — a `document_id` lookup is currently a table scan of a small,
per-document-bounded table, so it's fine as-is; flagging in case you want an
index alongside this (not mine to add — `document_publish_jobs` is created in
migration 0016, which predates my ownership of migrations here).

## Note (not urgent, adjacent to your file): F1 — repository identity re-derivation

Separate finding, same review, not requested of me directly but it directly
undermines outcome 18's `docs list`/`docs publish` from the CWD a user is
actually in: `crates/cli/src/commands.rs:760-761` computes

```rust
let repository = codypendent_knowledge::stable_repository_id(&std::env::current_dir()?.canonicalize()?);
```

while the daemon (`codypendentd::scan::repository_id_for`) resolves the git
toplevel FIRST. `codypendent_knowledge::anchor_repository_id` is the exported
helper matching the daemon's own derivation (`commands.rs:14` already imports
it) — it's just not used at this call site. Reproduced live: `codypendent docs
list` from a repo's root lists documents; the identical command one directory
down (`repo/src/`) reports "no documents yet," same daemon, same database. If
you touch `commands.rs` for F8, swapping `stable_repository_id` for
`anchor_repository_id` at this call site is a one-line, well-isolated
companion fix.
