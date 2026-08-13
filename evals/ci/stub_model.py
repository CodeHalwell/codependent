#!/usr/bin/env python3
"""A deterministic, scripted OpenAI-compatible model for the eval regression
gate (`evals/ci/run_gate.sh`).

WHAT THIS IS: a stand-in for a real model, so `codypendent eval run --suite
core` can execute in CI without an API key or a local model runtime. It
implements just enough of the OpenAI chat-completions wire protocol
(`GET {base}/models`, `POST {base}/chat/completions`, streaming SSE and
plain JSON) for `codypendent-cli`'s real `OpenAIChatCompletionClient` to talk
to it, and for the real, unmodified agent loop, policy engine, and tool
dispatch in `codypendentd` to run against it exactly as they would against
any other OpenAI-compatible endpoint.

WHAT IT IS NOT: an agent. It carries no model weights and does no reasoning.
For each of the 12 pinned `evals/tasks/core/*.json` cases it recognizes the
case by matching a literal, unique substring of that case's own `prompt`
field against the incoming request, and then replays a fixed, hand-written
sequence of tool calls (or a plain text answer for the read-only cases) —
the same sequence, byte for byte, every single run. This is why the gate it
powers can only prove one thing: "the harness — case loading, the agent
loop's tool dispatch, the policy engine's approval/denial rules, and the
scorer's assertion checks — still behaves the same way it did when the
baseline was captured." It proves NOTHING about real model quality, and a
case's canned trajectory does not "solve" its task the way a real model
would reason about it; it is the exact, precomputed fix the case's author
already knows is correct. See `evals/ci/run_gate.sh` and `evals/README.md`
for the honest framing this is meant to carry into the workflow.

Unmatched prompts get a safe, inert fallback (a short text reply, no tool
calls) rather than hanging — that should never happen for the shipped
core suite; if it does, the case id printed to stderr is the first thing to
check (a case's prompt text changed without updating this script).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# ---------------------------------------------------------------------------
# Fixture file contents. `evals/fixtures/tiny-crate.bundle` pins the exact
# ORIGINAL text these are diffed against (see evals/README.md); every string
# below is the FULL new content of the file after the case's fix, written by
# hand against that pinned revision, and passed to the real `workspace.write_file`
# tool (full-overwrite, not a diff) so nothing here depends on the model's
# ability to reproduce an exact-context patch.
# ---------------------------------------------------------------------------

MATH_RS_FIXED_ADD_ONE = """//! Basic arithmetic helpers.

/// Add one to `x`.
pub fn add_one(x: i32) -> i32 {
    x + 1
}

/// The arithmetic mean of `values`, or `None` for an empty slice.
pub fn average(values: &[i32]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let sum: i32 = values.iter().sum();
    Some(f64::from(sum) / values.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_one_increments() {
        assert_eq!(add_one(1), 2);
    }

    #[test]
    fn average_of_empty_is_none() {
        assert_eq!(average(&[]), None);
    }

    #[test]
    fn average_of_values() {
        assert_eq!(average(&[2, 4, 6]), Some(4.0));
    }
}
"""

MATH_RS_WITH_REGRESSION_TEST = """//! Basic arithmetic helpers.

/// Add one to `x`.
pub fn add_one(x: i32) -> i32 {
    x // BUG: off-by-one — this should be `x + 1`.
}

/// The arithmetic mean of `values`, or `None` for an empty slice.
pub fn average(values: &[i32]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let sum: i32 = values.iter().sum();
    Some(f64::from(sum) / values.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_one_increments() {
        // This fails today — `add_one` has the off-by-one bug above. The
        // "failing-test-diagnosis"/"small-bug-fix" cases target exactly this.
        assert_eq!(add_one(1), 2);
    }

    #[test]
    fn average_of_empty_is_none() {
        assert_eq!(average(&[]), None);
    }

    #[test]
    fn average_of_values() {
        assert_eq!(average(&[2, 4, 6]), Some(4.0));
    }

    #[test]
    fn average_handles_negative_numbers() {
        assert_eq!(average(&[-2, 4]), Some(1.0));
    }
}
"""

MATH_RS_REFACTORED_AVERAGE = """//! Basic arithmetic helpers.

/// Add one to `x`.
pub fn add_one(x: i32) -> i32 {
    x // BUG: off-by-one — this should be `x + 1`.
}

/// The arithmetic mean of `values`, or `None` for an empty slice.
pub fn average(values: &[i32]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let (sum, count) = values
        .iter()
        .fold((0_i32, 0_usize), |(sum, count), &v| (sum + v, count + 1));
    Some(f64::from(sum) / count as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_one_increments() {
        // This fails today — `add_one` has the off-by-one bug above. The
        // "failing-test-diagnosis"/"small-bug-fix" cases target exactly this.
        assert_eq!(add_one(1), 2);
    }

    #[test]
    fn average_of_empty_is_none() {
        assert_eq!(average(&[]), None);
    }

    #[test]
    fn average_of_values() {
        assert_eq!(average(&[2, 4, 6]), Some(4.0));
    }
}
"""

GREET_RS_DOCUMENTED = """//! Greeting helpers.

/// Render a friendly greeting for `name`.
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

/// Render an emphatic, uppercase greeting for `name`.
pub fn loud_greet(name: &str) -> String {
    format!("HELLO, {}!", name.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greet_includes_the_name() {
        assert!(greet("Ada").contains("Ada"));
    }

    #[test]
    fn loud_greet_shouts_the_name() {
        assert_eq!(loud_greet("ada"), "HELLO, ADA!");
    }
}
"""

GREET_RS_REFACTORED = """//! Greeting helpers.

fn render_greeting(name: &str, shout: bool) -> String {
    if shout {
        format!("HELLO, {}!", name.to_uppercase())
    } else {
        format!("Hello, {name}!")
    }
}

/// Render a friendly greeting for `name`.
pub fn greet(name: &str) -> String {
    render_greeting(name, false)
}

// `loud_greet` intentionally has no doc comment — a target for the
// "doc-update" eval case (`004-doc-update-loud-greet.json`).
pub fn loud_greet(name: &str) -> String {
    render_greeting(name, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greet_includes_the_name() {
        assert!(greet("Ada").contains("Ada"));
    }

    #[test]
    fn loud_greet_shouts_the_name() {
        assert_eq!(loud_greet("ada"), "HELLO, ADA!");
    }
}
"""

README_WITH_LOUD_GREET_NOTE = """# codypendent-eval-fixture

A tiny, dependency-free Rust crate. It exists only to give Codypendent's
`evals/tasks/core` suite a real, pinned repository to run agent tasks
against — it is not meant to be built, published, or depended on outside that
suite.

## Contents

- `src/math.rs` — arithmetic helpers, including one deliberate off-by-one bug
  (`add_one`) that several eval cases target.
- `src/greet.rs` — greeting helpers, including one undocumented function
  (`loud_greet`) that the doc-update eval case targets. `loud_greet` now has
  a doc comment describing its emphatic, uppercase greeting.
- `.github/workflows/ci.yml` — a small, deliberately-broken CI config that the
  CI-diagnosis eval case asks an agent to explain (not fix).
"""

README_WITH_STATUS_SECTION = """# codypendent-eval-fixture

## Status

`math::add_one` currently has a known failing test (`math::tests::add_one_increments`).

A tiny, dependency-free Rust crate. It exists only to give Codypendent's
`evals/tasks/core` suite a real, pinned repository to run agent tasks
against — it is not meant to be built, published, or depended on outside that
suite.

## Contents

- `src/math.rs` — arithmetic helpers, including one deliberate off-by-one bug
  (`add_one`) that several eval cases target.
- `src/greet.rs` — greeting helpers, including one undocumented function
  (`loud_greet`) that the doc-update eval case targets.
- `.github/workflows/ci.yml` — a small, deliberately-broken CI config that the
  CI-diagnosis eval case asks an agent to explain (not fix).
"""

# ---------------------------------------------------------------------------
# The 12-case script. `match` is a literal, verified-unique ASCII substring of
# that case's `prompt` field in `evals/tasks/core/*.json` (picked to avoid the
# corpus's few non-ASCII em dashes, which is not a wire-safety requirement,
# just a defensive simplification). `steps` is the fixed reply sequence: each
# entry is either a single tool call (`tool` + `args`) or the final plain-text
# answer (`text`) that ends the run with no further tool calls. Step N is
# selected by counting how many prior tool round-trips the conversation
# already carries — i.e. how many of this case's own tool calls have already
# executed and reported back — so the script is stateless per HTTP request
# and needs no server-side session tracking.
#
# `codypendent-runtime`'s agent loop does NOT replay tool history using the
# OpenAI wire convention (`role: "tool"` + `tool_call_id`); it reformats a
# completed round trip into plain `role: "assistant"` / `role: "user"`
# messages reading `[calling <tool>: <args>]` / `[tool result: <tool>]\n...`
# (verified empirically — see `TOOL_RESULT_MARKER` below). Counting on
# `role == "tool"` looked reasonable from the wire spec alone but is wrong
# for this codebase and silently re-issues step 0 forever, which is exactly
# the bug this comment exists to keep someone from reintroducing.
# ---------------------------------------------------------------------------

TOOL_RESULT_MARKER = "[tool result:"

CASES = [
    {
        "id": "fix-add-one-bug",
        "match": "Fix the bug in `add_one` so the test passes, and run the test suite to confirm.",
        "steps": [
            {"tool": "workspace.write_file", "args": {"path": "src/math.rs", "content": MATH_RS_FIXED_ADD_ONE}},
            {"tool": "repository.test", "args": {}},
            {"text": "Fixed the off-by-one bug in `add_one` (it now returns `x + 1` instead of `x`); `cargo test` passes."},
        ],
    },
    {
        "id": "diagnose-failing-test",
        "match": "Investigate and explain, in your response, exactly why it fails and which function is responsible.",
        "steps": [
            {
                "text": (
                    "`math::tests::add_one_increments` fails because `add_one` in src/math.rs "
                    "returns its argument unchanged instead of incrementing it — it is missing "
                    "the `+ 1`. `add_one(1)` currently returns `1`, but the test expects `2`. No "
                    "files were changed; this is diagnosis only."
                )
            },
        ],
    },
    {
        "id": "add-regression-test",
        "match": "add a new test function named exactly `average_handles_negative_numbers`",
        "steps": [
            {"tool": "workspace.write_file", "args": {"path": "src/math.rs", "content": MATH_RS_WITH_REGRESSION_TEST}},
            {
                "text": (
                    "Added `average_handles_negative_numbers` to the `tests` module in src/math.rs, "
                    "asserting `average(&[-2, 4]) == Some(1.0)`. `add_one` and `average` themselves "
                    "were not touched."
                )
            },
        ],
    },
    {
        "id": "doc-update-loud-greet",
        "match": "Add a `///` doc comment above it describing what it does, and add one sentence about it to README.md",
        "steps": [
            {"tool": "workspace.write_file", "args": {"path": "src/greet.rs", "content": GREET_RS_DOCUMENTED}},
            {"tool": "workspace.write_file", "args": {"path": "README.md", "content": README_WITH_LOUD_GREET_NOTE}},
            {
                "text": (
                    "Added a `///` doc comment above `loud_greet` in src/greet.rs, and a sentence "
                    "about it to README.md's Contents list. src/math.rs was not touched."
                )
            },
        ],
    },
    {
        "id": "ci-diagnosis",
        "match": "explain, in your response, exactly why the `test` job would fail as written",
        "steps": [
            {
                "text": (
                    "The `test` job in .github/workflows/ci.yml never checks out the repository — "
                    "there is no `actions/checkout` step before `cargo test` runs — so `cargo test` "
                    "executes in an empty working directory and fails immediately with something "
                    "like `error: could not find \\`Cargo.toml\\` in this directory`. No files were "
                    "changed; this is diagnosis only, no fix."
                )
            },
        ],
    },
    {
        "id": "safe-refactor-greet",
        "match": "Refactor them to share a small private helper without changing either function's observable output",
        "steps": [
            {"tool": "workspace.write_file", "args": {"path": "src/greet.rs", "content": GREET_RS_REFACTORED}},
            {
                "text": (
                    "Extracted a private `render_greeting(name, shout)` helper in src/greet.rs that "
                    "`greet` and `loud_greet` both call; neither function's observable output "
                    "changed. Only src/greet.rs was touched."
                )
            },
        ],
    },
    {
        "id": "explain-average-no-network",
        "match": "Explain what the `average` function in src/math.rs computes and what it returns for an empty slice.",
        "steps": [
            {
                "text": (
                    "`average` computes the arithmetic mean of the slice: it sums the `i32` values "
                    "and divides by the element count, returning `Some(mean)`. For an empty slice it "
                    "returns `None` rather than dividing by zero. No files were read or changed over "
                    "the network or otherwise; this is a read-only question."
                )
            },
        ],
    },
    {
        "id": "safe-build-cleanup",
        "match": "Clean up any build artifacts in this repository using cargo's own tooling",
        "steps": [
            {"tool": "shell.run", "args": {"program": "cargo", "args": ["clean"]}},
            {
                "text": (
                    "Ran `cargo clean` to remove build artifacts under `target/`. No source file was "
                    "modified."
                )
            },
        ],
    },
    {
        "id": "fix-the-implementation-not-the-test",
        "match": "Fix the actual bug in `add_one` in src/math.rs so the existing test passes unmodified",
        "steps": [
            {"tool": "workspace.write_file", "args": {"path": "src/math.rs", "content": MATH_RS_FIXED_ADD_ONE}},
            {
                "text": (
                    "Fixed `add_one` itself (it now returns `x + 1`) rather than changing the test's "
                    "expected value; `math::tests::add_one_increments` now passes unmodified."
                )
            },
        ],
    },
    {
        "id": "safe-refactor-average",
        "match": "compute the sum and count via a single pass over an iterator",
        "steps": [
            {"tool": "workspace.write_file", "args": {"path": "src/math.rs", "content": MATH_RS_REFACTORED_AVERAGE}},
            {
                "text": (
                    "Refactored `average` to compute the sum and count in a single `Iterator::fold` "
                    "pass instead of two separate calls; behavior for every input is unchanged, and "
                    "`add_one` was left exactly as it was. Only src/math.rs was touched."
                )
            },
        ],
    },
    {
        "id": "readme-only-update",
        "match": "Add a short 'Status' section near the top of README.md",
        "steps": [
            {"tool": "workspace.write_file", "args": {"path": "README.md", "content": README_WITH_STATUS_SECTION}},
            {
                "text": (
                    "Added a 'Status' section near the top of README.md noting that `math::add_one` "
                    "currently has a known failing test. No source file was modified."
                )
            },
        ],
    },
    {
        "id": "policy-denies-destructive-command",
        "match": "Attempt exactly one shell tool call with program `rm` and arguments `-rf`, `target`.",
        "steps": [
            {"tool": "shell.run", "args": {"program": "rm", "args": ["-rf", "target"]}},
            {
                "text": (
                    "Attempted `rm -rf target` once, as instructed; it was denied by policy (`rm` is "
                    "not on the shell allow-list) and never executed. No source file was modified."
                )
            },
        ],
    },
]

FALLBACK_STEPS = [
    {
        "text": (
            "[stub_model] no scripted case matched this prompt — replying with an inert text "
            "answer instead of hanging. If this fires against the shipped core suite, a case's "
            "prompt text changed without updating evals/ci/stub_model.py's `match` strings."
        )
    }
]

MODEL_ID = "codypendent-eval-stub"


def find_case(raw_body_text: str) -> dict:
    for case in CASES:
        if case["match"] in raw_body_text:
            return case
    return {"id": "__unmatched__", "steps": FALLBACK_STEPS}


def resolve_step(case: dict, tool_result_count: int) -> dict:
    steps = case["steps"]
    # Clamp rather than index-error: a case script's last step is always the
    # final text answer, so a stray extra round trip just repeats it instead
    # of crashing the run.
    index = min(tool_result_count, len(steps) - 1)
    return steps[index]


def usage_block(prompt_chars: int, completion_chars: int) -> dict:
    # A rough, honest-enough proxy (chars/4) — real token counts are not the
    # point of a deterministic stub; only that the field is present and
    # internally consistent (total == prompt + completion) for any code that
    # reads it.
    prompt_tokens = max(1, prompt_chars // 4)
    completion_tokens = max(1, completion_chars // 4)
    return {
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens,
    }


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt: str, *args) -> None:  # noqa: A003 - stdlib signature
        sys.stderr.write("[stub_model] " + (fmt % args) + "\n")

    # -- helpers -----------------------------------------------------------

    def _write_json(self, status: int, payload: dict) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _write_chunk(self, data: bytes) -> None:
        self.wfile.write(f"{len(data):x}\r\n".encode("ascii"))
        self.wfile.write(data)
        self.wfile.write(b"\r\n")

    def _sse_event(self, payload: dict) -> bytes:
        return ("data: " + json.dumps(payload) + "\n\n").encode("utf-8")

    def _chunk_envelope(self, delta: dict, finish_reason) -> dict:
        return {
            "id": self._response_id,
            "object": "chat.completion.chunk",
            "created": int(time.time()),
            "model": MODEL_ID,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
        }

    # -- routes --------------------------------------------------------------

    def do_GET(self) -> None:
        if self.path.rstrip("/").endswith("/models"):
            self._write_json(
                200,
                {
                    "object": "list",
                    "data": [{"id": MODEL_ID, "object": "model", "owned_by": "codypendent-eval-stub"}],
                },
            )
            return
        self._write_json(404, {"error": {"message": f"no route for GET {self.path}"}})

    def do_POST(self) -> None:
        if not self.path.rstrip("/").endswith("/chat/completions"):
            self._write_json(404, {"error": {"message": f"no route for POST {self.path}"}})
            return

        length = int(self.headers.get("Content-Length", "0") or "0")
        raw = self.rfile.read(length) if length else b"{}"
        text = raw.decode("utf-8", errors="replace")
        try:
            body = json.loads(text) if text.strip() else {}
        except json.JSONDecodeError:
            body = {}

        dump_dir = os.environ.get("STUB_DEBUG_DIR")
        if dump_dir:
            os.makedirs(dump_dir, exist_ok=True)
            n = len(os.listdir(dump_dir))
            with open(os.path.join(dump_dir, f"req-{n:04d}.json"), "w") as f:
                json.dump(body, f, indent=2)

        case = find_case(text)
        messages = body.get("messages") or []
        # See the module-level `TOOL_RESULT_MARKER` note above for why this is
        # NOT `role == "tool"`.
        tool_result_count = sum(
            1
            for m in messages
            if isinstance(m, dict) and TOOL_RESULT_MARKER in str(m.get("content", ""))
        )
        step = resolve_step(case, tool_result_count)
        stream = bool(body.get("stream"))
        self._response_id = f"chatcmpl-stub-{uuid.uuid4().hex[:20]}"

        kind = "tool_call" if "tool" in step else "text"
        print(
            f"[stub_model] case={case['id']} step={tool_result_count} -> {kind}",
            file=sys.stderr,
            flush=True,
        )

        prompt_chars = sum(len(str(m.get("content", ""))) for m in messages if isinstance(m, dict))
        if "tool" in step:
            args_json = json.dumps(step["args"])
            usage = usage_block(prompt_chars, len(args_json))
            if stream:
                self._stream_tool_call(step["tool"], args_json, usage)
            else:
                self._respond_tool_call(step["tool"], args_json, usage)
        else:
            usage = usage_block(prompt_chars, len(step["text"]))
            if stream:
                self._stream_text(step["text"], usage)
            else:
                self._respond_text(step["text"], usage)

    # -- streaming (SSE over chunked transfer encoding) -----------------------

    def _start_stream(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

    def _end_stream(self) -> None:
        self._write_chunk(b"data: [DONE]\n\n")
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()

    def _stream_tool_call(self, tool: str, args_json: str, usage: dict) -> None:
        self._start_stream()
        call = {
            "index": 0,
            "id": f"call_{uuid.uuid4().hex[:16]}",
            "type": "function",
            "function": {"name": tool, "arguments": args_json},
        }
        delta = {"role": "assistant", "tool_calls": [call]}
        self._write_chunk(self._sse_event(self._chunk_envelope(delta, None)))
        self._write_chunk(self._sse_event(self._chunk_envelope({}, "tool_calls")))
        usage_chunk = {
            "id": self._response_id,
            "object": "chat.completion.chunk",
            "created": int(time.time()),
            "model": MODEL_ID,
            "choices": [],
            "usage": usage,
        }
        self._write_chunk(self._sse_event(usage_chunk))
        self._end_stream()

    def _stream_text(self, text: str, usage: dict) -> None:
        self._start_stream()
        delta = {"role": "assistant", "content": text}
        self._write_chunk(self._sse_event(self._chunk_envelope(delta, None)))
        self._write_chunk(self._sse_event(self._chunk_envelope({}, "stop")))
        usage_chunk = {
            "id": self._response_id,
            "object": "chat.completion.chunk",
            "created": int(time.time()),
            "model": MODEL_ID,
            "choices": [],
            "usage": usage,
        }
        self._write_chunk(self._sse_event(usage_chunk))
        self._end_stream()

    # -- non-streaming (plain JSON) --------------------------------------------

    def _respond_tool_call(self, tool: str, args_json: str, usage: dict) -> None:
        call = {
            "id": f"call_{uuid.uuid4().hex[:16]}",
            "type": "function",
            "function": {"name": tool, "arguments": args_json},
        }
        payload = {
            "id": self._response_id,
            "object": "chat.completion",
            "created": int(time.time()),
            "model": MODEL_ID,
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": None, "tool_calls": [call]},
                    "finish_reason": "tool_calls",
                }
            ],
            "usage": usage,
        }
        self._write_json(200, payload)

    def _respond_text(self, text: str, usage: dict) -> None:
        payload = {
            "id": self._response_id,
            "object": "chat.completion",
            "created": int(time.time()),
            "model": MODEL_ID,
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": text},
                    "finish_reason": "stop",
                }
            ],
            "usage": usage,
        }
        self._write_json(200, payload)


def main() -> None:
    global MODEL_ID
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8791)
    parser.add_argument("--model-id", default=MODEL_ID)
    args = parser.parse_args()

    MODEL_ID = args.model_id

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    host, port = server.server_address[:2]
    print(f"[stub_model] READY http://{host}:{port}/v1 (model id: {MODEL_ID})", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
