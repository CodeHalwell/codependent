# Work-in-progress implementation patches — 2026-08-11

**Status: UNVERIFIED. None of these patches was compiled, linted, or tested.**

Eleven implementation agents were run in parallel isolated worktrees against the
[2026-08-11 product review](../2026-08-11-product-review.md), one per review
vertical. All eleven were terminated mid-edit when the account hit its monthly
spend limit — each stopped partway through a file, typically just before adding
tests. Their worktrees are ephemeral, so their work is preserved here as patch
files rather than lost or merged.

**Do not merge these blindly.** They are a head start, not a deliverable: expect
missing test coverage, unfinished match arms, unformatted code, and in several
cases a half-edited function. Treat each as "someone's editor buffer at the
moment the power went out".

## Applying one

```sh
git checkout -b resume/<vertical> df62ef4      # the commit they forked from
git apply docs/reviews/2026-08-11-wip-patches/<vertical>.patch
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Patches are diffs against `df62ef4` (v0.3.2 release commit), **not** against the
current branch head — the branch has since gained the review and the provider
model-catalog prefill (`2a6cc4e`, `dca9074`), which touch
`crates/providers/builtin_catalog.toml` and `docs/`. No patch below edits those
files, so conflicts should be limited to context drift.

## Inventory

| Patch | Vertical / review scope | Last known state when killed |
|---|---|---|
| `runtime-loop.patch` | Parallel tool calls, tool-schema params, retry/backoff, mid-run compaction, `artifact.read`, mode seeds | mid-driver: adding the system prompt + retry wrapper |
| `knowledge-context-skills.patch` | **The critical fix** — feed the assembled context manifest to the model; skill activation + startup scan + `skill add` | updating the `emit_run_opening` test for a new return type |
| `acp-model-discovery.patch` | ACP `config_options` capture, per-model profiles, `set_config_option`, spec-correct serve mode | mid-`PromptCommand` enum / `run_connection` driver |
| `model-selection.patch` | Catalog-powered picker cards, provider-auth header fix, `context_tokens`, discovery cache, `models` CLI | adding round-trip tests to `models.rs` |
| `tui-chat-polish.patch` | Tool/diff expansion, composer cursor, wrap accounting, width truncation, timestamps, `/theme` | re-applying reverted edits via Edit after a failed write |
| `workflow-dag-kanban.patch` | Snapshot edges, ASCII DAG pane, `workflow.query` tool, kanban migration 0019 + commands + `task.*` tools | adding the synthetic board-run method to `store.rs` |
| `docs-studio-writer.patch` | `CreateDocument` end-to-end, agent `docs.*` tools, `/update-docs`, suggestion re-anchoring | adding the `ProposedAction::DocumentEdit` variant |
| `council.patch` | Dossier fairness, report artifact, evidence mode, TUI council browser + run, cost totals | checking lint config before writing code (largest diff of the set) |
| `remote-ui-graph.patch` | TUI Graph edge sourcing + layered DAG paint, VS Code SVG graph, rate-limit alignment, `blackboard` projection | about to build and test the TUI crate |
| `voice-stt-tts.patch` | `SubmitUserInput.envelope`, `PutArtifact`, transcription seam, STT/TTS clients, capture/speak flows | updating `command.rs` test construction sites |
| `embeddings-topk.patch` | Real HTTP embeddings behind the `Embedder` trait, persisted vectors + outbox drain, MCP top-k advertisement | adding extras-parsing tests in `models.rs` |

A twelfth agent (Unsloth local-model integration: HF GGUF discovery,
`models pull` → `ollama pull` + register, fine-tune scaffold) was killed before
it created a worktree — nothing to preserve. Its scope is specified in the
session and can be re-run from scratch.

## Migration numbers reserved

To keep the parallel agents from colliding, migration numbers were pre-assigned
and should be honoured on resume: **0019** kanban/blackboard board columns,
**0020** docs (only if needed), **0021** voice (only if needed), **0022**
registry embeddings.
