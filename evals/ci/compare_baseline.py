#!/usr/bin/env python3
"""Compare a `codypendent eval run` report against the stored baseline
(`evals/baselines/<suite>.json`) and fail if the score dropped — the actual
regression gate `evals/ci/run_gate.sh` wires into CI.

WHAT THIS PROVES, AND WHAT IT DOES NOT. Every case in `evals/tasks/core/` is
run against `evals/ci/stub_model.py`, a deterministic, hand-scripted "model"
that replays the exact same fixed tool-call trajectory on every invocation
(see that file's own docstring). Because the model is fixed, the ONLY thing
that can change the suite's score between two commits is a change to the
harness itself: `crates/eval/**`, `crates/cli/src/eval.rs`, a case file
under `evals/tasks/core/`, or the pinned fixture. So this gate is a real,
meaningful regression test of THAT — it catches a scoring-logic change, an
assertion that stops firing, a case file edited into vacuity, a fixture
revision drift — exactly the class of bug the shipped review found (the
harness scoring 3/12 PASS on a run where the agent never executed).

It proves NOTHING about real model/prompt/skill quality: the stub never
reads a skill or a prompt file, so editing either can never move this
score. That comparison needs a live model and is intentionally out of
CI's reach today (see `evals/README.md`).

Exit codes: 0 = no regression (score held or improved, and no previously-
passing case newly fails). 1 = regression. 2 = usage/parse error.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


def load_report(path: Path) -> dict:
    with open(path, "r", encoding="utf-8") as f:
        report = json.load(f)
    results = report.get("results")
    if not isinstance(results, list):
        raise ValueError(f"{path}: not a SuiteReport (no `results` array)")
    return report


def case_results(report: dict) -> dict[str, bool]:
    """`{case_id: passed}` — mirrors `CaseResult::passed()` exactly: every
    assertion held, both budgets respected, AND the run actually completed
    (`run_completed`) — never re-derive a looser definition here."""
    out: dict[str, bool] = {}
    for entry in report["results"]:
        passed = (
            entry.get("run_completed", False)
            and entry.get("within_cost", False)
            and entry.get("within_duration", False)
            and all(a.get("passed", False) for a in entry.get("assertion_results", []))
        )
        out[entry["case_id"]] = passed
    return out


def summarize(report: dict) -> dict:
    results = case_results(report)
    total = len(results)
    passed = sum(1 for ok in results.values() if ok)
    return {
        "total": total,
        "passed": passed,
        "success_rate": (passed / total) if total else 1.0,
        "case_results": results,
    }


def git_sha() -> str:
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        )
        return out.stdout.strip()
    except Exception:
        return "unknown"


def load_baseline_history(path: Path) -> list[dict]:
    if not path.is_file():
        return []
    with open(path, "r", encoding="utf-8") as f:
        history = json.load(f)
    if not isinstance(history, list):
        raise ValueError(f"{path}: baseline file must be a JSON array (the history)")
    return history


def write_baseline_history(path: Path, history: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(history, f, indent=2)
        f.write("\n")


def compare(current: dict, baseline: dict) -> tuple[bool, list[str]]:
    """Returns `(regressed, messages)`."""
    messages = []
    regressed = False

    if current["success_rate"] < baseline["success_rate"]:
        regressed = True
        messages.append(
            f"REGRESSION: success rate dropped from {baseline['success_rate']:.4f} "
            f"({baseline['passed']}/{baseline['total']}) to {current['success_rate']:.4f} "
            f"({current['passed']}/{current['total']})"
        )

    # Stricter than the raw rate: two regressions can cancel out in
    # aggregate (one case starts failing, a different one starts passing).
    # A case the baseline recorded as passing must still pass.
    baseline_cases = baseline.get("case_results", {})
    current_cases = current.get("case_results", {})
    newly_failing = sorted(
        case_id
        for case_id, was_passing in baseline_cases.items()
        if was_passing and not current_cases.get(case_id, False)
    )
    if newly_failing:
        regressed = True
        messages.append(
            "REGRESSION: case(s) that passed against the stored baseline no longer pass: "
            + ", ".join(newly_failing)
        )

    missing = sorted(set(baseline_cases) - set(current_cases))
    if missing:
        messages.append(
            "NOTE: case(s) in the baseline are absent from this run (renamed or removed): "
            + ", ".join(missing)
        )
    newly_added = sorted(set(current_cases) - set(baseline_cases))
    if newly_added:
        messages.append(
            "NOTE: case(s) in this run are not in the baseline yet (new corpus growth): "
            + ", ".join(newly_added)
        )

    if not regressed:
        messages.append(
            f"OK: success rate {current['success_rate']:.4f} "
            f"({current['passed']}/{current['total']}) holds against the baseline "
            f"{baseline['success_rate']:.4f} ({baseline['passed']}/{baseline['total']})"
        )
    return regressed, messages


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("report", type=Path, help="the SuiteReport JSON `codypendent eval run --report` wrote")
    parser.add_argument(
        "--baseline",
        type=Path,
        default=Path("evals/baselines/core.json"),
        help="the baseline history file (default: evals/baselines/core.json)",
    )
    parser.add_argument(
        "--update-baseline",
        action="store_true",
        help=(
            "append this run's score as a NEW baseline entry instead of comparing "
            "(a deliberate, human-reviewed acceptance of a score change — see "
            "evals/README.md)"
        ),
    )
    parser.add_argument("--note", default="", help="a short note recorded with --update-baseline")
    args = parser.parse_args()

    try:
        report = load_report(args.report)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"error: could not read report {args.report}: {error}", file=sys.stderr)
        return 2

    current = summarize(report)

    if args.update_baseline:
        history = load_baseline_history(args.baseline)
        entry = {
            "date": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
            "git_sha": git_sha(),
            "note": args.note,
            **current,
        }
        history.append(entry)
        write_baseline_history(args.baseline, history)
        print(
            f"baseline updated: {args.baseline} now has {len(history)} entr"
            f"{'y' if len(history) == 1 else 'ies'}; latest is "
            f"{current['passed']}/{current['total']} ({current['success_rate']:.4f})"
        )
        return 0

    try:
        history = load_baseline_history(args.baseline)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"error: could not read baseline {args.baseline}: {error}", file=sys.stderr)
        return 2
    if not history:
        print(
            f"error: no baseline recorded at {args.baseline} — run with "
            "--update-baseline once to establish one (see evals/README.md)",
            file=sys.stderr,
        )
        return 2
    baseline = history[-1]

    regressed, messages = compare(current, baseline)
    for message in messages:
        print(message)

    if regressed:
        print(
            "eval regression gate: FAILED — a skill/prompt/harness edit lowered the "
            "deterministic-stub score (see this script's own module docstring for "
            "exactly what that does and does not prove)",
            file=sys.stderr,
        )
        return 1

    print("eval regression gate: PASSED")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
