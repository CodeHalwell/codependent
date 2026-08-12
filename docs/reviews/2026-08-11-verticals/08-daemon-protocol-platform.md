# Agent report: Daemon / protocol / platform

## Platform maturity verdict

**Unusually mature for a v0.3.2 — this platform can carry the product.** Properly event-sourced daemon: append-only per-session ledger `(session_id, sequence)` (`migrations/0001_init.sql:27-37`), six-step crash-consistent command write path with idempotency-key replay (`crates/daemon/src/commands.rs:1-37,130-141`), persist-before-publish fan-out (`subscriptions.rs:1-13`), atomic in-transaction sequence allocation (`ledger.rs:166-195`), genuine startup recovery matrix (`recovery.rs:1-27,89-130`). Single-instance exclusivity via socket-bind-before-recovery (`codypendentd/src/lib.rs:60-67`, `server.rs:243-248,457-474`). Real version negotiation (`server.rs:811-818`; `PROTOCOL_V1 = 1.4`), build-id stale-daemon detection, TOCTOU-closed idle shutdown (`server.rs:778-807`). Forward compat systematic: every wire enum `#[non_exhaustive]` + `#[serde(other)] Unknown`, golden vectors incl. Phase-0 bytes (`events.rs:434-457`, `protocol/tests/golden_vectors.rs`). ~1,900 test functions workspace-wide. **The codypendentd/daemon split is not a fork**: daemon declares trait seams (`RunExecutor`, `WorkflowStarter`, `BlackboardReader`, `DocumentMutator`, `PromotionGateway` — `daemon/src/executor.rs:102-286`), codypendentd is the composition root. Zero drift risk of the duplicated-logic kind.

## Protocol capability check for rubric features

**Voice (rubric 8): protocol plumbing fully built; nothing wired to it.** `InputEnvelope`/`InputBlock::Audio` with original-preserving transcripts, `TranscriptionMode::{Local,Remote}`, classification gate `transcription_allowed` (media defaults `Confidential`) (`protocol/src/input.rs:35-43,114-180,346-368`); `InputSource::Voice` (`input.rs:98-99`), `ClientCapabilities.audio_capture` (`capabilities.rs:22-23`), routing `ModelCapabilities.audio_input` hard filter, golden vectors for every audio shape. **But no command carries an `InputEnvelope`** — `SubmitUserInput` is `{ session_id, text, mode, model }` (`command.rs:118-136`). No capture path, no STT engine, no TTS anywhere, no client→daemon artifact upload. **Protocol work needed for STT:** (1) additive `envelope: Option<InputEnvelope>` on `SubmitUserInput`; (2) a `PutArtifact { media_type, base64, sensitivity } → ArtifactRef` command (16 MiB frame limit ok — ~1 min of 16 kHz WAV ≈ 2 MB); (3) a daemon transcription seam behind `transcription_allowed` (policy math already written+tested, `input.rs:465-520`). **TTS needs zero protocol change** if client-side over `ModelStreamDelta` text.

**Rich chat stream (rubric 7): good, three gaps.** Vocabulary covers run lifecycle, `ModelStreamDelta`, full tool lifecycle w/ labels + args digests (`ToolStarted.label`, `events.rs:117-135`), `ToolDenied`, `PatchProposed` (preview + counts + artifact, `events.rs:144-164`), approvals w/ typed risk, steering, budget warnings, presence. Gaps: no thinking/reasoning channel; no incremental tool output (bulk only at `ToolCompleted`); no per-turn token/cost usage event. All three additive.

**DAG viewer (rubric 5): live substrate excellent, wire carries no edges.** `ReadWorkflowRun` → `WorkflowRunSnapshot` (topo-ordered node views) + `Subscription::Workflow` full-state events with watermark-free idempotent merge (`protocol/src/workflow.rs:1-35`, `server.rs:3546-3571`). But `WorkflowNodeView` has **no `depends_on`/edge list** — a client can render a live node list but cannot draw arrows. One additive field completes the DAG viewer.

**Kanban/blackboard (rubric 10): subscription machinery done; write path agent-only.** `BlackboardItemView` + `ReadBlackboard` + live `Subscription::Blackboard` are a solid live-board substrate. Deliberately no client post command (`command.rs:406-407`); boards keyed to workflow runs only. Needs an additive role-gated client post/update command or session-scoped boards.

**Also:** ACP represented at approval layer (`ProposedAction::AcpToolCall`, v1.4). Model selection first-class (per-run pin + mid-conversation re-pin, verified live `server_it.rs:862`).

## Verified working

- Framing/handshake/liveness: length-prefixed JSON, clean-EOF vs mid-frame-drop distinction; heartbeat in own task (`server.rs:515-556,629-662`).
- Catch-up: subscribe-before-read + `catchup_through` drop-watermark = exactly-once attach; windowed SQL gap reads; unknown session ids rejected.
- Idempotency & crash consistency: duplicate commands replay recorded results; `received` rows resume; one executor launch per run.
- Approvals: durable park/wake broker, waiter re-registration on restart, run-scoped auto-approval by canonical digest, expiry sweep.
- Policy: deny-first everywhere — network default-deny, writes confined to `$WORKTREE` (canonicalize-before-compare, symlink-safe `policy/scope.rs:80-146`), exact-string program allowlist, git force-push dispositions, trusted-vs-untrusted overlay ordering, malformed policy = hard error; `fs_write` never widenable.
- Sandbox mechanism (plugins/UI workers only): macOS Seatbelt `(deny default)` via sandbox-exec (integration-tested); Linux bubblewrap `--unshare-*`, `--unshare-net` unless allowlisted, `--clearenv`, ro-bind granted paths, prlimit caps; **no seccomp** (explicitly deferred); non-Linux/macOS fails closed; degradation surfaced via typed `CapabilityReport`. Signature chain: sha256 artifact→manifest binding, ed25519 over whole-manifest domain-separated digest, unsigned default-deny, validated trust store; capability-expanding updates blocked; sealed pinned Node runtime with per-file digest manifest.
- GitHub: typed trait client (PR get/create-draft/update, check-runs, review comments, job logs); idempotent creates via hidden markers under create-lock; token never leaks; path-param injection refused; non-POST-only retries.
- Webhooks: raw-body HMAC before parse, constant-time; delivery-GUID replay protection; missing secret fails closed; slowloris-proofed hand-rolled server, loopback default, deliveries never trigger workflows.
- IDE: transport-agnostic bridge trait, deterministic debounce, dirty-buffer-digest provenance, latest-wins projection, Observer write-denial.
- Eval/promotion: objective assertions, execution-grounded signals, growing regression suite, structurally-unforgeable promotion (only `Actor::Human` reaches `Promoted`; daemon maps only Controller connections to human approvers with no wire field for actor).
- CI/release/install: fmt + clippy -D warnings all-targets/all-features, workspace tests with hang bound, cargo-deny with dated exceptions, eval smoke, Node SDK gates + npm audit; per-commit rolling prereleases, --locked builds, sha256-pinned Node runtime; install.sh set -euo pipefail, atomic swap.

## Bugs & broken wiring & drift risks (severity)

1. **[High — rubric-blocking] Multimodal input envelope is dead protocol.** Nothing constructs/sends/accepts/stores `InputEnvelope`. Voice is one additive command + upload command + transcriber away, but today rubric 8 is 0% wired.
2. **[Med] Catch-up snapshot cannot rebuild the chat.** >500 events behind → `SessionProjection` = title + active runs + pending approvals only (`catchup.rs:40-53`, `server.rs:3343-3349`) — no transcript, and no paged history-read command. Needs `ReadSessionEvents` or richer projection.
3. **[Med] Agent shell commands are not OS-sandboxed.** `shell.run` enforces allowlist/cwd/env-clear/pgroup-kill/caps but runs unconfined as the user; bwrap/Seatbelt machinery exists but applies only to plugins/UI workers. Documented posture, worth closing.
4. **[Low] Dead subscription kinds** `RepositoryStatus`/`BudgetState` match no events (`server.rs:3573-3586`).
5. **[Low] Stale doc comment** `daemon/src/executor.rs:62-65` ("StartRun carries no repository") — it has since v1.2 (`command.rs:141-149`).
6. **[Low] `resolve_run_repository` falls back to daemon `current_dir()`** for legacy clients (`server.rs:701-705`); consider failing instead.
7. **[Low] Roles are honor-system locally** (any local client self-asserts Controller; binds even on rejected attach, `server.rs:481-489,915-919`). Fine for 0700-socket single-user; must change for any remote transport.
8. **[Low] install.sh verifies nothing about the tarball** (no checksum/signature on release assets); clears macOS quarantine wholesale.

## Security posture notes

Strong local-first: 0700 data/run dirs, 0600 HMAC secret, HMAC-SHA256 resume tokens (constant-time, 24h TTL), secrets non-Debug/non-Serialize, env carried on `ExecuteCommand` actions so approvers see LD_PRELOAD-class smuggling, untrusted plugin/MCP output sanitized at one chokepoint, classification gates rank `Unknown` above `Secret`, routing fails closed to local-only under undeclared classification. Residual risks: unconfined approved shell (item 3), installer trust (8), honor-system roles the moment a non-loopback transport lands.

## Prioritized opportunities (S/M/L, impact)

1. **(S, high)** Add edges to `WorkflowRunSnapshot` — one additive field unlocks the DAG viewer (rubric 5).
2. **(M, high)** Wire voice input: additive `SubmitUserInput.envelope`, `PutArtifact` upload, local-whisper transcription seam behind `transcription_allowed`; store audio in CAS (rubric 8).
3. **(S, high)** `ReadSessionEvents { after, limit }` paged history command — fixes snapshot-transcript gap (rubric 7).
4. **(S, med)** Additive chat events: `ThinkingDelta`, `ToolOutputDelta`, per-turn `UsageReported { tokens, cost }`.
5. **(M, med)** Client blackboard write: role-gated `PostBlackboard`/`UpdateBlackboard` + optional session-scoped boards → kanban over existing hub (rubric 10).
6. **(M, med)** Run approved agent shell commands through existing bwrap/Seatbelt executor with profile from the minted `CapabilityGrant` — composition, not new machinery.
7. **(S, med)** Release-asset sha256 + verification in install.sh; optionally ed25519-sign assets.
8. **(S, low)** Fix stale RunLaunch comment; implement or remove dead subscription kinds.

## Extra ideas

- TTS cheaply client-side over `ModelStreamDelta` + `RunCompleted.summary`; add `speech_output` capability flag mirroring `audio_capture`.
- Voice approvals: push-to-talk "approve once" mapped to `ResolveApproval` (constrained enum, not free text) — natural safe first voice feature.
- Kanban from approvals: pending approvals are already a durable, subscribable queue — a "needs-human" column exists for free.
- Protocol vectors as SDK contract: generate the TypeScript SDK types from `protocol-vectors/` + golden tests so web/VS Code clients can't drift.
- Reuse promotion pipeline for voice models: bench local STT with `models bench`, route transcriptions with the same classification gate.
