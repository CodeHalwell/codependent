#!/usr/bin/env bash
# The eval corpus regression gate (Outcome 16). Runs the REAL, shipped
# `evals/tasks/core/` suite against a deterministic, hand-scripted stub model
# (`evals/ci/stub_model.py`) — never a live model, never a paid API key — and
# compares the score against the stored baseline (`evals/baselines/core.json`)
# via `evals/ci/compare_baseline.py`, which is the file that actually decides
# pass/fail and documents exactly what that pass/fail does and does not prove.
#
# Usage:
#   evals/ci/run_gate.sh                 # run the gate (exit 1 on regression)
#   evals/ci/run_gate.sh --update-baseline [note...]
#                                         # accept the current score as the
#                                         # new baseline (a deliberate,
#                                         # human-reviewed action — see
#                                         # evals/README.md)
#
# Assumes `cargo build -p codypendent-cli` has already produced
# `target/debug/codypendent` (CI splits build and run into separate steps —
# see `.github/workflows/ci.yml`'s own comment on why — but this script
# builds it itself when missing, for local use).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

BIN="$REPO_ROOT/target/debug/codypendent"
STUB_PORT="${EVAL_GATE_STUB_PORT:-8791}"
STUB_MODEL_ID="codypendent-eval-stub"

if [ ! -x "$BIN" ]; then
  echo "eval-gate: $BIN not found, building codypendent-cli (this can take a while)..." >&2
  CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build -p codypendent-cli
fi

# A short-lived, short-path scratch dir: the daemon socket lives under
# `<data_dir>/run/daemon.sock`, and Unix domain socket paths are capped at
# ~104-108 bytes — a scratchpad-style nested tmp path blows that budget (hit
# for real while building this gate; see the task report). `/tmp` directly,
# not `mktemp -d` under a longer default, keeps this safely short.
DATA_DIR="$(mktemp -d /tmp/cev-gate.XXXXXX)"
REPORT_PATH="$DATA_DIR/report.json"
STUB_LOG="$DATA_DIR/stub.log"
cleanup() {
  local status=$?
  if [ -n "${STUB_PID:-}" ] && kill -0 "$STUB_PID" 2>/dev/null; then
    kill "$STUB_PID" 2>/dev/null || true
    wait "$STUB_PID" 2>/dev/null || true
  fi
  if [ "$status" -ne 0 ] && [ -f "$DATA_DIR/logs/daemon.log" ]; then
    echo "eval-gate: daemon log (last 40 lines):" >&2
    tail -n 40 "$DATA_DIR/logs/daemon.log" >&2 || true
  fi
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

cat > "$DATA_DIR/models.toml" <<EOF
[[model]]
id = "stub-coder"
provider = "openai-compatible"
base_url = "http://127.0.0.1:$STUB_PORT/v1"
model = "$STUB_MODEL_ID"
api_key_env = ""
EOF

python3 "$REPO_ROOT/evals/ci/stub_model.py" \
  --port "$STUB_PORT" --model-id "$STUB_MODEL_ID" \
  > "$STUB_LOG" 2>&1 &
STUB_PID=$!

# Wait for the stub to actually bind before pointing the daemon at it,
# rather than a fixed sleep — `stub_model.py` prints its READY line as soon
# as the socket is listening.
for _ in $(seq 1 50); do
  if grep -q "READY" "$STUB_LOG" 2>/dev/null; then
    break
  fi
  if ! kill -0 "$STUB_PID" 2>/dev/null; then
    echo "eval-gate: stub_model.py exited before becoming ready:" >&2
    cat "$STUB_LOG" >&2
    exit 1
  fi
  sleep 0.1
done

echo "eval-gate: running the core suite against the deterministic stub model..." >&2
set +e
CODYPENDENT_DATA_DIR="$DATA_DIR" "$BIN" eval run --suite core --report "$REPORT_PATH"
RUN_EXIT=$?
set -e

if [ ! -f "$REPORT_PATH" ]; then
  echo "eval-gate: no report was written to $REPORT_PATH (codypendent exited $RUN_EXIT \
before scoring) — this is a harness failure, not a scored regression" >&2
  echo "eval-gate: stub model log:" >&2
  cat "$STUB_LOG" >&2
  exit 1
fi

# `codypendent eval run`'s own exit code reflects "did every case pass" —
# NOT "did the score regress". The baseline is allowed to be less than
# 12/12 (a deterministic stub is not a capable model); the comparison below
# is the actual gate.
echo "eval-gate: codypendent eval run exited $RUN_EXIT (informational; the score comparison below is the gate)" >&2

BASELINE="$REPO_ROOT/evals/baselines/core.json"

if [ "${1:-}" = "--update-baseline" ]; then
  shift
  exec python3 "$REPO_ROOT/evals/ci/compare_baseline.py" "$REPORT_PATH" \
    --baseline "$BASELINE" --update-baseline --note "$*"
fi

# Bootstrap, don't fabricate: this gate ships with NO pre-computed baseline
# number in `evals/baselines/core.json` (an honest empty history, not a
# guessed score — see that file's own comment). The first time this job
# ever runs against a real, working `codypendent` binary, it establishes the
# baseline from that real run and passes; every run after that compares
# against it for real. This is a one-time, self-documenting bootstrap, not a
# standing escape hatch — `compare_baseline.py --update-baseline` outside
# this bootstrap path always requires the explicit flag.
if [ ! -s "$BASELINE" ] || [ "$(python3 -c "import json,sys; print(len(json.load(open(sys.argv[1]))))" "$BASELINE" 2>/dev/null || echo 0)" = "0" ]; then
  if [ "${EVAL_GATE_ALLOW_BOOTSTRAP:-0}" = "1" ]; then
    echo "eval-gate: no baseline at $BASELINE — establishing one (explicit bootstrap; NOT a comparison)" >&2
    python3 "$REPO_ROOT/evals/ci/compare_baseline.py" "$REPORT_PATH" \
      --baseline "$BASELINE" --update-baseline \
      --note "bootstrap: established a baseline on request"
    exit 0
  fi
  # An empty baseline used to bootstrap itself and exit 0. In CI that write
  # lands in a disposable checkout and is gone before the next run, so the gate
  # silently re-bootstrapped on every run and could never fail — a complete
  # score collapse would have passed. Refuse instead, and say exactly how to
  # produce the missing artifact.
  echo "eval-gate: no baseline recorded at $BASELINE." >&2
  echo "eval-gate: this gate compares against a COMMITTED baseline; without one it" >&2
  echo "eval-gate: would pass unconditionally, so it fails instead." >&2
  echo "eval-gate: to establish one, run locally and commit the result:" >&2
  echo "eval-gate:   EVAL_GATE_ALLOW_BOOTSTRAP=1 evals/ci/run_gate.sh" >&2
  exit 1
fi

exec python3 "$REPO_ROOT/evals/ci/compare_baseline.py" "$REPORT_PATH" --baseline "$BASELINE"
