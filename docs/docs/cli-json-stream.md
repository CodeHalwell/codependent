# CLI JSON Stream Contract

`codypendent` provides a headless, line-delimited JSON (`jsonl` / `json`) event stream interface for scripting, CI/CD pipelines, automated testing, and editor integrations.

## Invocation

### Starting a Headless Run

Start a new headless run using either a positional prompt argument or the `--objective` flag:

```bash
# Positional prompt with --json alias
codypendent run "Refactor the authentication module to use Argon2id" --json

# Explicit flag format with mode selection
codypendent run --objective "Audit codebase dependencies" --mode review --jsonl

# Pinning to a specific configured model
codypendent run "Implement health check endpoint" --model anthropic/claude-3-7-sonnet --json
```

### Attaching to an Existing Session

Attach to a live or retained session to stream session events:

```bash
# Attach and stream events in JSON format
codypendent attach 018f3a90-8e12-7000-8000-000000000001 --events json

# Resume replay from an exclusive sequence cursor
codypendent attach 018f3a90-8e12-7000-8000-000000000001 --from-sequence 42 --events jsonl
```

## Exit Code Contract

`codypendent run` terminates when the initiated run reaches a final disposition, exiting with:

| Exit Code | Disposition | Description |
|-----------|-------------|-------------|
| `0` | `Completed` | The agent achieved the objective and finalized the run. |
| `2` | `Failed` | The run failed (policy rejection, model failure, tool failure, or invocation error). |
| `130` | `Cancelled` | The run was interrupted or cancelled by the operator (SIGINT). |

## Wire Envelope Format

Every line emitted to `stdout` is a self-describing, single-line JSON object adhering to the Codypendent v1 protocol envelope:

```json
{
  "protocol_version": 1,
  "message_id": "018f3a90-8e12-7000-8000-000000000002",
  "correlation_id": null,
  "client_id": "018f3a90-8e12-7000-8000-000000000003",
  "workspace_id": null,
  "session_id": "018f3a90-8e12-7000-8000-000000000001",
  "sequence": 1,
  "payload": {
    "type": "event",
    "event": {
      "sequence": 1,
      "occurred_at": "2026-08-15T12:00:00Z",
      "causation_id": null,
      "correlation_id": null,
      "actor": "system",
      "body": {
        "type": "run_started",
        "run_id": "018f3a90-8e12-7000-8000-000000000004",
        "objective": "Refactor auth module",
        "mode": "build"
      }
    }
  }
}
```

## Core Event Stream Lifecycle

1. **`run_started`**: Emitted when the run begins, specifying `run_id`, `objective`, and `mode`.
2. **`model_stream_delta`**: Real-time token chunks emitted as the model generates responses.
3. **`tool_proposed` / `tool_started` / `tool_completed` / `tool_denied`**: Lifecycle of tool invocations governed by policy.
4. **`patch_proposed`**: Generated changesets proposed for workspace application.
5. **`context_usage`**: Per-turn breakdown of context window consumption:
   - `used_tokens`: Total active context tokens
   - `window_tokens`: Provider context window ceiling
   - `system_tokens`: System instructions and environment context tokens
   - `tool_tokens`: Tool schema declaration tokens
   - `transcript_tokens`: Conversation history tokens
6. **`run_usage`**: Cumulative turn token and inference cost metrics.
7. **`run_completed`**: Final terminal event carrying the `disposition` (`Completed`, `Failed`, or `Cancelled`) and chronicle artifact reference.
