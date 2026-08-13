# Reviewer brief — shared by all verticals

Repo: /home/user/codypendent
Pinned commit: 535a2f5e3848b256536ddee94883dc0010ecdcb8 (v0.4.5)
Do not change code. This is a review pass only. Do not commit, do not push.
A workspace build is already running/complete; `cargo build --workspace --all-features`
artifacts live in ./target. Reuse them; do not `cargo clean`.

## The 20 target outcomes

SHIPPED — implemented in one large release, never exercised against a live model,
a real terminal, an audio device or a local runtime. Assume nothing works.
 1. Beautiful, well-formatted, easy-to-use TUI — every menu polished
 2. ACP fully working, incl. automatic model discovery from ACP agents
 3. Easy model selection; prefilled model lists for non-ACP providers
 4. Agent skill-writer and doc-writer
 5. Fully functional DAG viewer for code-context management (user + agent)
 6. Fully functional AI council
 7. Rich chat stream
 8. Built-in TTS + STT
 9. Vector top-k tool/skill selection (keyword + embedding), not inject-all
10. Built-in blackboard + kanban board; natural-language backlog tools

NEW — to be built after their base is verified
11. Live measured routing (on 3)      16. Evals as a product loop (on all)
12. Executable skills / WASM (on 4,9) 17. Compounding memory (on 9)
13. Hook engine (on 12)               18. Docs round-trip (on 4)
14. Live code graph (on 5,9)          19. Real multi-user (on 10,18)
15. Delegation (on 5,6,10)            20. Ledger made visible (on all)

## What counts as a finding

For outcomes 1-10 the question is NOT "does the code exist" — it does. It is
"what happens when a user actually does this". RUN IT. Green tests are not
evidence. A defect nobody can observe is not a finding.

Every finding needs:
  - `crates/foo/src/bar.rs:123` file:line
  - the user-visible consequence, concretely ("user types X, sees Y, expects Z")
  - a classification:
      (a) engine missing entirely
      (b) engine built, tested, documented — final wire never attached
      (c) wire attached, wrong behaviour
    (b) is the cheapest and highest-value class. Hunt it deliberately.

## Patterns that hide well — hunt these specifically

  - SILENT FILTERS: status gates, scope filters, capability checks that drop
    items and report nothing. Anything returning "not found" or an empty list
    where the real answer is "filtered".
  - DATA PRODUCED BUT NEVER CONSUMED: assembled, rendered into a trace, and
    dropped. Follow EVERY producer to a real consumer. If a struct is built and
    only ever read by a test, that is a finding.
  - TRUST-BOUNDARY READS: code that acts on metadata supplied by a caller
    instead of re-deriving it from what the server stored.

## Method

Read EVERY file in your vertical. Do not sample. Then run things:
  - `cargo run -p codypendent-cli --bin codypendent -- <args>` for CLI paths
  - the daemon binary is `codypendentd`
  - for TUI, drive it headless / with a pty if you can; check `crates/tui/src/`
    render and reduce functions directly for anything you cannot drive
  - write throwaway probe binaries or `#[test]` harnesses in
    /tmp/claude-0/-home-user-codypendent/5b8351f1-c73f-5de3-981c-c56d73b7a138/scratchpad
    if that is the fastest way to exercise a seam. Do not add files to the repo.

## Output

Write your full report to `.review/verticals/<your-vertical>.md`.
Structure: for each owned outcome, a verdict line
  `OUTCOME N: WORKS | PARTIAL | BROKEN | ABSENT — one sentence`
then findings, then a section "What I could not exercise, and why".

Then RETURN (in your final message) a summary under 1200 words: the verdict
lines, your top 5 findings with file:line and class, and the single structural
pattern you think explains most of what you found. The return value is read by
the orchestrator; the file is the record.
