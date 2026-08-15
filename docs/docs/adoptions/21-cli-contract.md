# Adoption 21 — One-shot + JSON-stream CLI contract

**Effort:** S · **Depends on:** nothing · **Reference:** reference-repos/cline/apps/cli/src/commands/program.ts (flag surface), src/utils/output.ts (`emitJsonLine`), src/utils/events.ts (`handleEvent`), src/runtime/run-agent.ts (`run_start`/`run_result`), src/tests/headless/messages-contract.live.test.ts + headless.test.ts (the `--json | jq` contract test)
**Ported from:** cline · **Status:** ⬜ not started

## 1. Summary

Codypendent already ships a non-interactive, NDJSON-streaming client — `codypendent run --objective "…" --jsonl` and `codypendent attach <SESSION_ID> --events jsonl`, both emitting one self-describing `Envelope` per stdout line (`crates/cli/src/stream.rs`). This adoption **verifies, stabilizes, and documents** that surface as a promised contract, matching cline's tested `--json | jq` bar, and closes the two ergonomic gaps that stop it from being the "obvious" scriptable entry point:

1. **Ergonomics** — accept the prompt as a **positional argument** (`codypendent run "fix the flaky test"`) so `--objective` is no longer mandatory, and add `--json` as a **visible alias** of `--jsonl` (cline's flag name), so `codypendent run "…" --json | jq` works as a reader would expect.
2. **A contract test** — a new test that pins the NDJSON line schema (the `Envelope` → `Payload::Event(SessionEvent)` shape, its `type` discriminators, and the exit-code mapping) so any wire-shape drift breaks CI. This is cline's `messages-contract` bar ported to Rust.
3. **A docs page** — a user-facing `docs/docs/cli-json-stream.md` documenting the stream contract and its stability guarantee, registered in `docs/MANIFEST.json` + `docs/docs/SUMMARY.md`.

This is deliberately **not a protocol redesign**: the envelope, its forward-compat `#[serde(other)] Unknown` fallbacks, and the exit codes are already stable and tested. The additions are a thin, additive CLI surface plus the contract test and doc that make the guarantee explicit. No migration; `0039` stays free.

## 2. Reference implementation

All paths under `reference-repos/cline/apps/cli/`.

- **Flag surface** (`src/commands/program.ts`): the entire non-interactive surface is the default top-level command plus flags — a positional `[prompt]` (`cline "…"` runs one turn and exits) and `--json` ("Output messages as JSON instead of styled text"), mapping to `outputMode: "json"`. There is **no** `headless` subcommand and **no** `--output-format`; "headless" is a *derived* state (yolo, or `--json`, or piped stdin without `--tui`). JSON mode **requires** a prompt or piped stdin (interactive+JSON is rejected).
- **Framing** (`src/utils/output.ts`, `emitJsonLine`): every line is `JSON.stringify({ ts: nowIso(), ...record }) + "\n"` on stdout — exactly one JSON object per line, no array wrapper, no pretty-printing; each line carries an added `ts` timestamp and a string **`type`** discriminator. `writeln` is a no-op in JSON mode (stdout stays pure NDJSON); errors go to **stderr** as `{ type: "error", message }`. EPIPE is swallowed for `| head`/`| jq`.
- **Top-level line types** (`src/runtime/run-agent.ts`, `src/utils/events.ts`): `run_start` (verbose only — `{ providerId, modelId, catalog, thinking, mode, sessionId }`), `agent_event` (`{ type:"agent_event", event: <AgentEvent> }` — the typed event nested under `event`), `run_result` (`{ finishReason, iterations, usage, durationMs, text, model }`), `run_aborted`, `run_abort_requested`, `team_restored`, `team_event`.
- **The contract test** (`src/tests/headless/messages-contract.live.test.ts`, `headless.test.ts`): runs `cline --json "…"` and asserts (a) stdout contains a line matching `/\{.*"type"/i` and the process exits 0 — i.e. **every line is a JSON object carrying a `type` field**; (b) the unauthenticated path emits `/"type":"error"/i` and exits 1; (c) the persisted message artifact carries `modelInfo.id`/`modelInfo.provider` (strings) and a numeric `metrics` block. The comments state the framing rule verbatim: "one JSON object per line". The stability guarantee is the **`type`-field-per-line invariant plus the model/usage-carrying shapes**.

## 3. Current state in codypendent (verified — what the CLI stream does TODAY)

- **One-shot exists, prompt is flag-only.** `crates/cli/src/main.rs` lines 85-114: the `Run` subcommand takes `--objective <String>` (required, **no positional form**), `--mode` (`ModeArg`: Ask/Explore/Plan/Build/Review, default Build), `--repo`, `--model`, and `--jsonl` (bool). Dispatch (lines 1253-1272) calls `commands::run(...).await` and `std::process::exit(exit_code)`. A bare `codypendent` (no subcommand, `main.rs` lines 1224-1233) opens the **interactive TUI** — so a top-level bare positional prompt is ambiguous with both the TUI and the subcommands and is intentionally *not* added here.
- **`--jsonl` is the streaming flag; `--jsonl` is currently required on `run`.** `commands::run` (`crates/cli/src/commands.rs` lines 365-416) hard-errors if `--jsonl` is absent ("interactive attach lands in a later build step"). Separately, `--json` **already exists on other subcommands** (`daemon status`, `doctor`, `graph`, `council`, `workflow show`, `acp list`) as a one-shot machine-readable *report* toggle — NOT a stream. So `--json` is free to add to `run` as the streaming alias without collision, and doing so aligns the flag name with cline.
- **NDJSON framing is already correct.** `crates/cli/src/stream.rs` `write_line` (lines 87-92): `serde_json::to_string(&envelope)` + `writeln!` + `flush()` per line — one `Envelope` per line, flushed immediately. `stream_until_terminal` (used by `run --jsonl`) writes every event envelope, ends on the owned run's `RunCompleted`, then `drain_trailing_usage` (≤1500 ms) to also emit the trailing `RunUsage`. `stream_forever` (used by `attach`) writes every event until the connection closes. `replay_catchup` emits the attach backlog first (each `Catchup::Events` event re-wrapped into an `Envelope`; a `Catchup::Snapshot` emitted as one snapshot line).
- **Each line is a full `Envelope`, never a bare `SessionEvent`.** `crates/protocol/src/envelope.rs` lines 25-39: `Envelope { protocol_version, message_id, correlation_id?, client_id, workspace_id?, session_id?, sequence?, payload }` — a plain struct with **no top-level `type` tag**; the discriminator lives inside `payload`. `Payload` (lines 71-344) is internally tagged `#[serde(tag = "type")]` with `#[serde(other)] Unknown`; a streamed event is `Payload::Event(SessionEvent)`, which flattens the `SessionEvent`'s fields next to `"type":"Event"`. `SessionEvent` (`crates/protocol/src/events.rs` lines 26-36): `{ sequence, occurred_at, causation_id?, correlation_id?, actor, body }`. `Actor` and `EventBody` are both `#[serde(tag="type")]`, `#[non_exhaustive]`, `#[serde(other)] Unknown`. **Model/provider ride `Actor::Agent { agent_id, run_id, model }` on `RunStarted`/`RunUsage` events; there is no cline-style `run_start` line.**
- **Exit codes are contracted and tested.** `crates/cli/src/stream.rs` `RunExit` (lines 19-58): `0` Completed, `2` Failed, `130` Cancelled, mapped from both `RunState` and `RunDisposition` (future variants → no exit mapping). `main` is the only caller of `std::process::exit`.
- **The drive loop** (`crates/cli/src/commands.rs` `run_over_connection`, lines 423-518): handshake → `CreateSession` → `AttachSession(Controller)` → `replay_catchup` → `StartRun { objective, mode, repository, model }` (the owned `RunId` comes from `Payload::CommandAccepted { created_run, .. }`) → `stream_until_terminal`. No `SubmitUserInput` on this path — the one-shot prompt is the `StartRun.objective`.
- **Forward-compat is already the wire rule.** `#[serde(other)] Unknown` on `Payload`, `Actor`, `EventBody`, and `Catchup` means a newer daemon's unknown tag deserializes to `Unknown` rather than erroring the frame — the basis for a "unknown event types are skipped, not fatal" promise.
- **Must not break**: `--objective` and `--jsonl` stay accepted forever (scripts use them); the `Envelope`/`Payload`/`SessionEvent` wire shapes and their `Unknown` fallbacks; the exit-code mapping; the `attach --events jsonl` surface; the daemon's `forward_events` envelope stamping that `stream.rs` mirrors.

## 4. Design (per-Action subsections)

### 4.1 Action 21 — stabilize + document, with a thin additive surface

There is no genuine gap in the *streaming machinery*; the additions are ergonomic + documentary:

- **Positional prompt on `run`.** Make `objective` accept either a positional value or the `--objective` flag, so `codypendent run "fix the bug"` and `codypendent run --objective "fix the bug"` are equivalent. Supplying neither, or both with different values, is a clap error. (The top-level bare `codypendent "prompt"` stays reserved for the TUI — documented as such; a bare prompt is ambiguous with subcommands and the interactive default.)
- **`--json` as the streaming alias.** Add `--json` as a visible alias of `--jsonl` on `run` (and, for symmetry, accept `--events json` on `attach` as an alias of `jsonl`). Both select the identical NDJSON `Envelope` machinery; `--jsonl` remains for back-compat. Keep the "streaming flag currently required on `run`" behavior unchanged (interactive attach is a later step) — this adoption does not change *when* the stream runs, only how it is named and how the prompt is passed.
- **A documented, tested contract.** The stream's promise, pinned by a contract test:
  1. **Framing**: exactly one JSON object per line, terminated by `\n`, flushed per line; readers split on `\n` only (never on internal punctuation).
  2. **Envelope shape**: every stdout line deserializes to an `Envelope`; a streamed event line has `payload.type == "Event"` carrying a `SessionEvent`; a snapshot line has `payload.type == "Catchup"`.
  3. **Discriminators are stable**: `Payload`, `Actor`, `EventBody`, `Catchup` are internally tagged on `type`, and an unknown future tag deserializes to `Unknown` rather than failing the line (so a reader on an older CLI does not crash on a newer daemon's event).
  4. **Model/provider are discoverable**: the run's model rides `Actor::Agent { model }` on the `RunStarted` event (the codypendent analogue of cline's `run_start.modelId`), and token/cost ride `EventBody::RunUsage`.
  5. **Exit codes**: `0` completed, `2` failed, `130` cancelled; stdout carries only NDJSON, human diagnostics go to stderr.

The contract test asserts 1-5 against representative envelopes so drift in any wire shape or discriminator breaks CI.

## 5. Changes, file by file

### 5.1 `crates/cli/src/main.rs` — positional prompt + `--json` alias

`Run` subcommand (lines 85-114): make `objective` positional-or-flag and add the alias.

```rust
    /// Start a headless run and stream its events — the scriptable twin of
    /// the interactive TUI. The prompt may be positional
    /// (`codypendent run "fix the bug"`) or via `--objective`.
    Run {
        /// What the agent should do. Positional; `--objective` is an alias for
        /// scripts that prefer named args. Exactly one must be given.
        #[arg(value_name = "PROMPT")]
        prompt: Option<String>,
        /// Named alias for the positional prompt.
        #[arg(long, conflicts_with = "prompt")]
        objective: Option<String>,
        #[arg(long, value_enum, default_value = "build")]
        mode: ModeArg,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
        /// Stream every session event to stdout as NDJSON (one `Envelope` per
        /// line) until the run terminates. `--json` is an alias.
        #[arg(long, visible_alias = "json")]
        jsonl: bool,
    },
```

Dispatch (lines 1253-1272): resolve the prompt, erroring if neither/both are supplied.

```rust
        TopCommand::Run { prompt, objective, mode, repo, model, jsonl } => {
            let objective = prompt.or(objective).ok_or_else(|| {
                anyhow::anyhow!("a prompt is required: `codypendent run \"<prompt>\"`")
            })?;
            let repo = match repo { Some(repo) => repo, None => std::env::current_dir()? };
            let exit_code =
                commands::run(&paths, objective, mode.into(), repo, model, jsonl).await?;
            std::process::exit(exit_code);
        }
```

`Attach` (lines 115-128): extend `EventsFormat` with a `Json` alias.

```rust
#[derive(Clone, Copy, ValueEnum)]
enum EventsFormat {
    Jsonl,
    /// Alias of `jsonl` — identical NDJSON `Envelope` stream.
    #[value(alias = "json")]
    Json,
}
```

Map both `Jsonl` and `Json` to the same `commands::attach` call (lines ~1268-1272).

### 5.2 `crates/cli/src/commands.rs` — no behavioral change

`commands::run`'s signature is unchanged (`objective: String`); it keeps rejecting a missing stream flag with the same message (now naming `--json`/`--jsonl`). The positional/flag resolution happens entirely in `main.rs`, so `commands.rs` is untouched except the error string:

```rust
        if !jsonl {
            anyhow::bail!(
                "codypendent run currently requires --json (or --jsonl); run \
                 `codypendent` with no subcommand for the interactive view"
            );
        }
```

### 5.3 `crates/cli/src/stream.rs` — the contract test (new `#[cfg(test)] mod contract`)

The contract test constructs representative `Envelope`s covering every stream line kind, writes them through the real `write_line`, and asserts the NDJSON contract. It is the stability gate: any change to the envelope/event wire shape, a discriminator rename, or a framing regression fails it.

```rust
#[cfg(test)]
mod contract {
    //! Adoption 21 — the stable NDJSON stream contract. These assertions ARE
    //! the promise `codypendent run --json | jq` scripts depend on; changing a
    //! wire shape or a `type` discriminator must break this test on purpose.
    use super::*;
    use codypendent_protocol::{Actor, EventBody, RunDisposition, RunState, SessionEvent};
    use serde_json::Value;

    fn event_line(body: EventBody, actor: Actor, sequence: u64) -> Value {
        let event = SessionEvent {
            sequence,
            occurred_at: chrono::Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor,
            body,
        };
        let envelope = envelope_for(ClientId::new(), SessionId::new(), event);
        let mut buf = Vec::new();
        write_line(&mut buf, &envelope).unwrap();
        // Framing: exactly one line, terminated by a single '\n', no embedded newline.
        let text = String::from_utf8(buf).unwrap();
        assert!(text.ends_with('\n'));
        assert_eq!(text.matches('\n').count(), 1);
        serde_json::from_str(text.trim_end()).unwrap()
    }

    #[test]
    fn every_stream_line_is_one_json_object_with_a_type() {
        // A representative run: started (carries the model), a token delta, a
        // tool completion, a terminal completion, and trailing usage.
        let run_id = RunId::new();
        let started = event_line(
            EventBody::RunStarted { run_id, /* … */ },
            Actor::Agent { agent_id: AgentId::new(), run_id, model: "openai/gpt-5.4".into() },
            1,
        );
        // Envelope discriminator is the flattened Payload tag.
        assert_eq!(started["type"], "Event");
        // The inner event body carries its own stable `type`.
        assert_eq!(started["body"]["type"], "RunStarted");
        // Model/provider are discoverable on the agent actor (the run_start analogue).
        assert_eq!(started["actor"]["type"], "Agent");
        assert_eq!(started["actor"]["model"], "openai/gpt-5.4");

        for (body, tag) in [
            (EventBody::ModelStreamDelta { run_id, text: "hi".into() }, "ModelStreamDelta"),
            (EventBody::RunUsage { run_id, prompt_tokens: Some(10), completion_tokens: Some(5), cost_micros: Some(42) }, "RunUsage"),
        ] {
            let line = event_line(body, Actor::System, 2);
            assert_eq!(line["type"], "Event");
            assert_eq!(line["body"]["type"], tag);
        }
    }

    #[test]
    fn a_future_event_tag_deserializes_to_unknown_not_error() {
        // The forward-compat promise: an older CLI must not choke on a newer
        // daemon's event. A body with an unrecognized tag round-trips to Unknown.
        let raw = r#"{"sequence":9,"occurred_at":"2026-01-01T00:00:00Z",
            "actor":{"type":"System"},"body":{"type":"SomeFutureEvent","x":1}}"#;
        let event: SessionEvent = serde_json::from_str(raw).unwrap();
        assert!(matches!(event.body, EventBody::Unknown));
    }

    #[test]
    fn exit_codes_are_the_documented_contract() {
        assert_eq!(RunExit::Completed.exit_code(), 0);
        assert_eq!(RunExit::Failed.exit_code(), 2);
        assert_eq!(RunExit::Cancelled.exit_code(), 130);
        assert_eq!(RunExit::from_state(RunState::Completed), Some(RunExit::Completed));
        assert_eq!(
            RunExit::from_disposition(&RunDisposition::Failed { reason: "x".into() }),
            Some(RunExit::Failed)
        );
    }

    #[test]
    fn a_catchup_snapshot_is_one_line_tagged_catchup() {
        let mut buf = Vec::new();
        replay_catchup(&mut buf, ClientId::new(), SessionId::new(),
            Catchup::Snapshot { through: 7, projection: SessionProjection::empty(/* … */) }).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(text.matches('\n').count(), 1);
        let line: Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(line["type"], "Catchup");
    }
}
```

(Field placeholders `/* … */` are filled from the real event constructors — the point is the assertions, which pin `type` values, framing, model discoverability, forward-compat, and exit codes.)

### 5.4 `docs/docs/cli-json-stream.md` (new, user-facing)

A reference page documenting the contract, structured as: the invocation (`codypendent run "<prompt>" --json`, `codypendent attach <id> --events json`); the framing rule (one `Envelope` per line, split on `\n`); the line shapes with a real example line per kind (event, catchup snapshot); the `type` discriminator table (`Payload::Event`/`Catchup`; `EventBody` variants a script cares about — `RunStarted`, `ModelStreamDelta`, `ToolStarted`/`ToolCompleted`, `RunCompleted`, `RunUsage`); where model/provider/usage live; the exit-code table (0/2/130); and the **stability guarantee**: fields are additive-only, discriminators never repurposed, unknown tags deserialize to `Unknown` (readers must ignore unknown `type`s, never fail), and the contract test is the enforcement. Include a worked `jq` recipe mirroring cline's README, e.g. streaming assistant text:

```bash
codypendent run "summarize the auth module" --json \
  | jq -r 'select(.type=="Event" and .body.type=="ModelStreamDelta") | .body.text'
```

Register the page: add its entry to `docs/MANIFEST.json` and a link under the appropriate section of `docs/docs/SUMMARY.md` (the `doc-counts` CI job enforces the manifest; run `check_docs_manifest.py --fix` after adding it).

## 6. Protocol & persistence

- **No wire changes.** The `Envelope`, `Payload`, `SessionEvent`, `Actor`, `EventBody`, and `Catchup` shapes and their `#[serde(other)] Unknown` fallbacks are unchanged — the contract test *pins* them, it does not alter them.
- **No new commands or events.** The CLI already drives the run with the shipped `CreateSession`/`AttachSession`/`StartRun` and consumes the shipped event stream.
- **No migration** — 0039 stays free.
- **CLI surface is additive**: `--objective` and `--jsonl` keep working; `--json` and the positional prompt are aliases over the identical machinery. `attach --events json` aliases `jsonl`.

## 7. Acceptance criteria

1. RUN `cargo test -p codypendent-cli contract` — EXPECT the §5.3 contract module green: every stream line is one `\n`-terminated JSON object whose `type` is a stable discriminator; `RunStarted` carries the model on `actor`; a future event tag deserializes to `Unknown`; exit codes are 0/2/130; a catchup snapshot is one `type:"Catchup"` line.
2. `codypendent run "hello" --json` is accepted (positional prompt + `--json` alias) and behaves identically to `codypendent run --objective "hello" --jsonl`: EXPECT byte-identical stdout modulo ids/timestamps and the same exit code.
3. `codypendent run` with neither a positional prompt nor `--objective` exits non-zero with the "a prompt is required" message; supplying both is a clap conflict error.
4. RUN `codypendent run "print hi" --json | jq -r 'select(.type=="Event" and .body.type=="ModelStreamDelta") | .body.text'` (against a stub/echo model in the test harness) — EXPECT the assistant text on stdout and nothing non-JSON on stdout.
5. `codypendent attach <id> --events json` streams the identical NDJSON as `--events jsonl`.
6. A completed run exits `0`, a failed run `2`, a cancelled run `130`; the terminal diagnostic (failure/cancel reason) appears on **stderr**, never mixed into the stdout NDJSON.
7. The docs page exists, is registered in `docs/MANIFEST.json` and `SUMMARY.md`, and the `doc-counts` job passes.
8. RUN `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` — EXPECT green.

## 8. Tests

- `crates/cli/src/stream.rs` — the `contract` module (§5.3): `every_stream_line_is_one_json_object_with_a_type`, `a_future_event_tag_deserializes_to_unknown_not_error`, `exit_codes_are_the_documented_contract`, `a_catchup_snapshot_is_one_line_tagged_catchup`. These extend the existing `stream.rs` test conventions (the module already round-trips `RunExit` mappings).
- `crates/cli` argument tests (clap `Cli::try_parse_from`, following the existing main-arg tests): `run_accepts_a_positional_prompt`, `run_accepts_objective_flag`, `run_rejects_neither_prompt`, `run_rejects_both_prompt_and_objective`, `json_is_a_visible_alias_of_jsonl`, `attach_events_json_aliases_jsonl`.
- An end-to-end streaming test in the CLI's existing daemon-backed harness (or `commands.rs` tests) with a deterministic echo model: `run_json_stream_is_pure_ndjson_with_stable_types` — drives a full `run --json`, splits stdout on `\n`, asserts every line parses to an `Envelope`, the first `RunStarted` carries the model, the sequence is monotonic, and the last line is a `RunCompleted`/`RunUsage`; asserts the process exit code matches the disposition.
- `crates/protocol` — the round-trip coverage for `Envelope`/`Payload::Event`/`EventBody`/`Catchup` already exists (`events.rs` `every_phase1_event_body_round_trips`, `envelope.rs` tests); add a single assertion that a `Payload` with an unknown `type` deserializes to `Payload::Unknown` if not already present, so the "reader survives a newer daemon" promise is pinned at the protocol layer too.

## 9. Gotchas

1. **NDJSON framing must split on `\n` only** — the doc and any example reader must never split on internal punctuation or attempt to parse the concatenated stream as a JSON array. Each line is an independent `Envelope`; the contract test asserts exactly one `\n` per `write_line`.
2. **Every line is a full `Envelope`, not a bare `SessionEvent`** — the discriminator a reader keys on is `payload.type` (`"Event"`/`"Catchup"`), *then* `body.type` for the event kind. A reader that expects the event fields at the top level will miss the envelope wrapper. Document the two-level shape explicitly (the `Payload` tag flattens next to the `SessionEvent` fields).
3. **There is no `run_start` line** — unlike cline, model/provider are not a dedicated first line; they ride `Actor::Agent { model }` on the `RunStarted` event. The doc must point readers there (and to `RunUsage` for tokens/cost), or they will look for a nonexistent `run_start`.
4. **stdout stays pure NDJSON; diagnostics go to stderr** — the failure/cancel reason is echoed to stderr only (`stream.rs` lines 214-216). A `| jq` consumer must not see a non-JSON line on stdout; the e2e test asserts this.
5. **`--json` must not collide with the report-mode `--json` on other subcommands** — it is only added to `run`/`attach`, where no `--json` exists today; the report-toggle `--json` on `daemon status`/`doctor`/`graph`/… is a different, one-shot surface and is untouched.
6. **Keep `--objective` and `--jsonl` forever** — scripts in the wild use them; they become aliases, never removed. The positional/`--json` forms are additive sugar.
7. **Trailing usage is part of the contract** — `run --jsonl` deliberately drains up to 1500 ms after `RunCompleted` to emit the trailing `RunUsage` line; a reader that stops at `RunCompleted` misses cost/tokens. The doc should note that `RunUsage` may arrive *after* `RunCompleted`.
8. **Forward-compat is the reader's contract too** — the promise is "unknown `type` values deserialize to `Unknown`, never error the line"; the doc must instruct readers to *ignore* unknown `type`s rather than treat them as failures, matching the daemon's own `#[serde(other)] Unknown` discipline.
9. **The exit code maps disposition, not stream health** — a stream that ends because the connection dropped is distinct from a `RunCompleted`; `RunExit` is derived only from a terminal run state/disposition, and any future non-exhaustive disposition variant maps to *no* exit (the caller must default). Keep that mapping the single source of truth in `RunExit`.

## 10. Out of scope

- A top-level bare `codypendent "<prompt>"` (no subcommand) — ambiguous with the interactive TUI default and the subcommand set; the prompt goes on `run`.
- Interactive attach for `run` without a stream flag ("lands in a later build step" per `commands.rs`); this adoption does not change *when* the stream is required.
- A cline-style `run_start`/`run_result` summary line — codypendent's `RunStarted`/`RunCompleted`/`RunUsage` events already carry that information; synthesizing extra envelope kinds would be net-new wire surface, which this adoption avoids.
- `SubmitUserInput`/multi-turn over the one-shot path (the one-shot prompt is the `StartRun.objective`; multi-turn is the TUI/attach surface).
- A JSON Schema / OpenAPI artifact for the stream — the Rust types plus the contract test are the schema; a generated schema export is a separate (spec 12/A6) concern.
- Changing the envelope, adding a top-level `type` tag, or adding a `ts` field to match cline — the codypendent envelope already carries `occurred_at` inside the event and `sequence` on the envelope; no reshaping.
