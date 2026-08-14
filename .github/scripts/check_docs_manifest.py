#!/usr/bin/env python3
"""CI check: `docs/MANIFEST.json`'s `files` list is exactly what is on disk.

The manifest is the doc suite's index — the only machine-readable answer to
"what documents ship". It is hand-maintained, and it drifted the way every
hand-maintained list drifts: a 2026-08 review found 57 listed against 130 on
disk; the repair pass regenerated it by hand and the SAME release cycle then
added two release notes it never got. That is the loop this closes.

It compares both directions, because they fail differently:

- **listed but missing from disk** — a dangling entry; a reader following the
  index hits a 404.
- **on disk but unlisted** — a shipped document no index mentions, which is how
  the two newest release notes became invisible.

Usage: `check_docs_manifest.py [--fix]`. Without `--fix` it prints every
difference and exits 1. With `--fix` it rewrites the `files` list (sorted) and
the `date`, so regenerating is a one-command answer to the failure message
rather than a hand edit that will drift again.
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
DOCS_ROOT = REPO_ROOT / "docs"
MANIFEST = DOCS_ROOT / "MANIFEST.json"
#: The manifest indexes the doc suite, not itself.
EXCLUDED_NAMES = {"MANIFEST.json"}


def files_on_disk() -> list[str]:
    return sorted(
        str(path.relative_to(DOCS_ROOT).as_posix())
        for path in DOCS_ROOT.rglob("*")
        if path.is_file() and path.name not in EXCLUDED_NAMES
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--fix", action="store_true", help="rewrite the manifest to match disk")
    args = parser.parse_args()

    try:
        with open(MANIFEST, "r", encoding="utf-8") as f:
            manifest = json.load(f)
    except (OSError, json.JSONDecodeError) as error:
        print(f"error: could not read {MANIFEST}: {error}", file=sys.stderr)
        return 2
    listed = manifest.get("files")
    if not isinstance(listed, list):
        print(f"error: {MANIFEST} has no `files` array", file=sys.stderr)
        return 2

    disk = files_on_disk()
    missing = [f for f in listed if not (DOCS_ROOT / f).is_file()]
    unlisted = [f for f in disk if f not in set(listed)]
    duplicates = sorted({f for f in listed if listed.count(f) > 1})

    if args.fix:
        manifest["files"] = disk
        manifest["date"] = datetime.now(timezone.utc).strftime("%Y-%m-%d")
        with open(MANIFEST, "w", encoding="utf-8") as f:
            json.dump(manifest, f, indent=2, ensure_ascii=False)
            f.write("\n")
        print(f"docs manifest: rewritten — {len(disk)} file(s) listed")
        return 0

    if not (missing or unlisted or duplicates):
        print(f"docs manifest: OK — all {len(disk)} file(s) under docs/ are listed, and nothing else")
        return 0

    print("docs manifest: FAILED — docs/MANIFEST.json does not match docs/ on disk:\n")
    for entry in missing:
        print(f"  - listed but NOT on disk: {entry}")
    for entry in unlisted:
        print(f"  - on disk but NOT listed: {entry}")
    for entry in duplicates:
        print(f"  - listed twice: {entry}")
    print("\n  Fix: python3 .github/scripts/check_docs_manifest.py --fix")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
