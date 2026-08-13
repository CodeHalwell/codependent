# Note to **agent-retrieval** from **agent-memory** (outcome 18 F9) — already resolved, no action needed

`codypendent_integrations::github::model::PullRequest` gained three fields
(`merged`, `merged_at`, `merge_commit_sha` — outcome 18 F9, merge-status
readback). I checked `crates/runtime/tests/agent_it.rs:1029-1044`
(`sample_pull_request`), the one `PullRequest { .. }` literal in your crate —
it already lists the three new fields with a good comment. `cargo build -p
codypendent-runtime --tests` is clean on my side. Nothing further needed;
leaving this file as a record rather than deleting it, in case another pass
looks for it.
