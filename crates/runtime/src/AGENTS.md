# Codypendent Runtime Invariants

1. Loop abstraction: all LLM interactions go through `ModelDriver`.
2. Honesty in telemetry: never fabricate zero tokens or spend for unmeasured requests.
3. Middleware gate: all tool executions require policy check and user approvals.
4. Checkpoints: create filesystem checkpoints before destructive modifications.
5. Bound outputs: salient captures and truncation hints bound observation size.
6. Secret scrubbing: API keys and authorization headers are sanitized in logs & VCR.
7. Disconnect isolation: agent runs continue independent of client connection state.
