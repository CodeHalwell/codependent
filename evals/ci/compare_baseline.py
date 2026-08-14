#!/usr/bin/env python3
"""Compare a `codypendent eval run` report against the stored baseline
(`evals/baselines/<suite>.json`) and fail if ANYTHING about the suite's
result moved — the actual regression gate `evals/ci/run_gate.sh` wires into CI.

WHAT THIS PROVES, AND WHAT IT DOES NOT. Every case in `evals/tasks/core/` is
run against `evals/ci/stub_model.py`, a deterministic, hand-scripted "model"
that replays the exact same fixed tool-call trajectory on every invocation
(see that file's own docstring). Because the model is fixed, the ONLY thing
that can change the suite's result between two commits is a change to the
harness itself: `crates/eval/**`, `crates/cli/src/eval.rs`, a case file
under `evals/tasks/core/`, the stub's own script, or the pinned fixture. So
this gate is a real, meaningful regression test of THAT — a scoring-logic
change, an assertion that stops firing, a case file edited into vacuity, a
fixture revision drift.

**It proves NOTHING about real model, prompt or skill quality.** The stub
reads neither a prompt file nor a skill file — it selects its reply by
matching a literal substring of the case's own `prompt` and replays a
precomputed answer — so editing a system prompt, a skill, or a retrieval
policy CANNOT move this score in either direction. The roadmap sentence "a
skill or prompt edit that lowers the score fails CI" is **not** what this
gate does and cannot be made true by a deterministic stub; it needs a live
model, which is deliberately out of CI's reach (no API key, no
nondeterminism, no per-PR spend). `evals/README.md` §"What the CI gate can
and cannot detect" is the long version, and is the claim of record.

WHY IT FAILS ON AN IMPROVEMENT TOO. A one-directional gate ("fail only if
the score dropped") cannot catch the failures this file's own docstring
claims to catch: a case edited into vacuity and an assertion that silently
stopped firing both RAISE the score. Deleting the ten failing cases from a
report and re-running this comparator used to print `success rate 1.0000
(3/3) … PASSED, EXIT=0`. So every difference from the baseline — a lower
score, a higher score, a case that flipped either way, a case id added or
removed, a different corpus size — fails, and the only way to accept one is
the deliberate, human-reviewed `--update-baseline "<why>"`.

Exit codes: 0 = the run matches the baseline exactly. 1 = it does not.
2 = usage/parse error.
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
    # `total` is the RESULT COUNT, not `len(results)`: two entries sharing a
    # case id would collapse in the dict above and silently shrink the corpus.
    total = len(report["results"])
    if total != len(results):
        raise ValueError(
            f"report contains {total} results but only {len(results)} distinct case ids — "
            "duplicate ids make every per-case comparison below unsound"
        )
    passed = sum(1 for ok in results.values() if ok)
    return {
        "total": total,
        "passed": passed,
        "success_rate": (passed / total) if total else 1.0,
        "case_results": results,
    }


def corpus_case_ids(corpus_dir: Path) -> set[str]:
    """The case ids actually on disk in a suite directory.

    Independent of the report: it catches the one shape the baseline
    comparison structurally cannot — a runner that loaded fewer case files
    than the suite ships (a glob that stopped matching, a parse failure
    swallowed into a skip). The baseline only ever sees what the run
    reported."""
    ids: set[str] = set()
    for path in sorted(corpus_dir.glob("*.json")):
        with open(path, "r", encoding="utf-8") as f:
            case = json.load(f)
        case_id = case.get("id")
        if not case_id:
            raise ValueError(f"{path}: case file has no `id`")
        if case_id in ids:
            raise ValueError(f"{path}: duplicate case id {case_id!r} in {corpus_dir}")
        ids.add(case_id)
    return ids


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


ACCEPT_HINT = (
    "if this change is intended, accept it deliberately: "
    'evals/ci/run_gate.sh --update-baseline "<why>"'
)


def compare(current: dict, baseline: dict, corpus_ids: set[str] | None) -> tuple[bool, list[str]]:
    """Returns `(regressed, messages)`.

    Every difference from the baseline is a failure, in both directions. See
    the module docstring for why a drop-only gate is not a gate: the two
    failures this file exists to catch — a case edited into vacuity, an
    assertion that stopped firing — both make the score go UP."""
    messages = []
    regressed = False

    baseline_cases = baseline.get("case_results", {})
    current_cases = current.get("case_results", {})

    # 1. The corpus itself. Checked FIRST and by id, not by count: a run that
    #    silently lost 10 of 13 cases used to report `1.0000 (3/3) … PASSED`,
    #    because a score computed over the survivors says nothing about the
    #    cases that vanished.
    missing = sorted(set(baseline_cases) - set(current_cases))
    if missing:
        regressed = True
        messages.append(
            "REGRESSION: case(s) in the baseline are absent from this run (deleted, renamed, or "
            "never loaded): " + ", ".join(missing) + f" — {ACCEPT_HINT}"
        )
    added = sorted(set(current_cases) - set(baseline_cases))
    if added:
        regressed = True
        messages.append(
            "REGRESSION: case(s) in this run are not in the baseline (corpus growth is a "
            "deliberate, reviewed change, not a silent one): "
            + ", ".join(added)
            + f" — {ACCEPT_HINT}"
        )
    if current["total"] != baseline["total"]:
        regressed = True
        messages.append(
            f"REGRESSION: corpus size changed — the baseline scored {baseline['total']} case(s), "
            f"this run scored {current['total']} — {ACCEPT_HINT}"
        )

    # 2. The suite the runner was pointed at, straight off disk. The baseline
    #    can only describe what some past run REPORTED; this is the only check
    #    that sees a case file the run never loaded at all.
    if corpus_ids is not None:
        unrun = sorted(corpus_ids - set(current_cases))
        if unrun:
            regressed = True
            messages.append(
                "REGRESSION: case file(s) ship in the corpus directory but produced no result in "
                "this run: " + ", ".join(unrun)
            )
        phantom = sorted(set(current_cases) - corpus_ids)
        if phantom:
            regressed = True
            messages.append(
                "REGRESSION: this run reported case(s) that no case file in the corpus directory "
                "declares: " + ", ".join(phantom)
            )

    # 3. Per-case, both directions. A case flipping fail→pass is not good news
    #    here: the model is fixed, so nothing it does can improve — a rise means
    #    an assertion stopped firing or a case lost its teeth.
    newly_failing = sorted(
        case_id
        for case_id, was_passing in baseline_cases.items()
        if was_passing and not current_cases.get(case_id, True)
    )
    if newly_failing:
        regressed = True
        messages.append(
            "REGRESSION: case(s) that passed against the stored baseline no longer pass: "
            + ", ".join(newly_failing)
        )
    newly_passing = sorted(
        case_id
        for case_id, was_passing in baseline_cases.items()
        if not was_passing and current_cases.get(case_id, False)
    )
    if newly_passing:
        regressed = True
        messages.append(
            "REGRESSION: case(s) that FAILED against the stored baseline now pass: "
            + ", ".join(newly_passing)
            + " — against a fixed stub model an improvement is evidence that an assertion stopped "
            f"firing or a case was edited into vacuity, not that the agent got better. {ACCEPT_HINT}"
        )

    # 4. The aggregate, last: after the checks above it can only differ if one
    #    of them missed something, so a bare rate mismatch is worth naming.
    if current["success_rate"] != baseline["success_rate"]:
        regressed = True
        messages.append(
            f"REGRESSION: success rate moved from {baseline['success_rate']:.4f} "
            f"({baseline['passed']}/{baseline['total']}) to {current['success_rate']:.4f} "
            f"({current['passed']}/{current['total']}) — {ACCEPT_HINT}"
        )

    if not regressed:
        messages.append(
            f"OK: {current['passed']}/{current['total']} "
            f"({current['success_rate']:.4f}) matches the baseline exactly, case for case"
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
    parser.add_argument(
        "--corpus-dir",
        type=Path,
        default=None,
        help=(
            "the suite directory the run was pointed at (e.g. evals/tasks/core); when given, every "
            "case file there must have produced a result in the report and vice versa"
        ),
    )
    args = parser.parse_args()

    try:
        report = load_report(args.report)
        current = summarize(report)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"error: could not read report {args.report}: {error}", file=sys.stderr)
        return 2

    corpus_ids: set[str] | None = None
    if args.corpus_dir is not None:
        try:
            corpus_ids = corpus_case_ids(args.corpus_dir)
        except (OSError, ValueError, json.JSONDecodeError) as error:
            print(
                f"error: could not read the corpus directory {args.corpus_dir}: {error}",
                file=sys.stderr,
            )
            return 2

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

    regressed, messages = compare(current, baseline, corpus_ids)
    for message in messages:
        print(message)

    if regressed:
        print(
            "eval regression gate: FAILED — this run does not match the stored baseline "
            "(see this script's own module docstring for exactly what that does and does "
            "not prove; in particular it says nothing about model, prompt or skill quality)",
            file=sys.stderr,
        )
        return 1

    print("eval regression gate: PASSED")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
