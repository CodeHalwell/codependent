#!/usr/bin/env python3
"""Check that committed SQL migrations remain byte-for-byte immutable.

The checksum file is a JSON object mapping migration filenames to lowercase
SHA-384 digests. Normal CI usage is read-only:

    python3 .github/scripts/check_migration_immutability.py

When intentionally adding a migration, append its checksum and commit the
result with the SQL file:

    python3 .github/scripts/check_migration_immutability.py --update

``--update`` only records migrations whose number is greater than every
already-recorded migration. It still rejects modified or deleted historical
files, so it cannot be used to bless drift accidentally. On the first use it
bootstraps the checksum file from all currently present migrations.

Exit status 1 means migration drift or an unrecorded append; status 2 means the
check itself cannot run because its inputs are missing or malformed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
MIGRATIONS_DIR = REPO_ROOT / "migrations"
CHECKSUMS_FILE = MIGRATIONS_DIR / "checksums.json"
MIGRATION_RE = re.compile(r"^(?P<number>[0-9]+)_.+\.sql$")
SHA384_RE = re.compile(r"^[0-9a-f]{96}$")


def migration_number(filename: str) -> int:
    match = MIGRATION_RE.fullmatch(filename)
    if match is None:
        raise ValueError(f"not a numbered migration filename: {filename!r}")
    return int(match.group("number"))


def migrations_on_disk() -> dict[str, Path]:
    migrations: dict[str, Path] = {}
    numbers: dict[int, str] = {}
    for path in sorted(MIGRATIONS_DIR.iterdir()):
        if not path.is_file() or MIGRATION_RE.fullmatch(path.name) is None:
            continue
        number = migration_number(path.name)
        if number in numbers:
            raise ValueError(
                f"migration number {number:04d} is used by both "
                f"{numbers[number]!r} and {path.name!r}"
            )
        numbers[number] = path.name
        migrations[path.name] = path
    return migrations


def sha384(path: Path) -> str:
    digest = hashlib.sha384()
    try:
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(128 * 1024), b""):
                digest.update(chunk)
    except OSError as error:
        raise ValueError(f"could not read {path}: {error}") from error
    return digest.hexdigest()


def validate_checksums(value: object, source: str) -> dict[str, str]:
    if not isinstance(value, dict):
        raise ValueError(f"{source} must contain a JSON object")
    checksums: dict[str, str] = {}
    for filename, digest in value.items():
        if not isinstance(filename, str) or MIGRATION_RE.fullmatch(filename) is None:
            raise ValueError(f"invalid migration key in {source}: {filename!r}")
        if not isinstance(digest, str) or SHA384_RE.fullmatch(digest) is None:
            raise ValueError(
                f"invalid SHA-384 checksum for {filename!r}: expected 96 lowercase hex characters"
            )
        checksums[filename] = digest
    return checksums


def read_checksums() -> dict[str, str]:
    try:
        with CHECKSUMS_FILE.open("r", encoding="utf-8") as source:
            value = json.load(source)
    except FileNotFoundError:
        raise
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"could not read {CHECKSUMS_FILE}: {error}") from error

    return validate_checksums(value, str(CHECKSUMS_FILE))


def read_parent_checksums() -> dict[str, str] | None:
    """Read the previous commit's manifest when Git history is available.

    Pull-request merge commits and ordinary pushes both expose the trusted
    pre-change tree as ``HEAD^``. The first commit introducing the manifest has
    no parent copy and is the one intentional bootstrap exception.
    """
    result = subprocess.run(
        ["git", "show", "HEAD^:migrations/checksums.json"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return None
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ValueError(f"could not parse parent migration checksum manifest: {error}") from error
    return validate_checksums(value, "parent migrations/checksums.json")


def write_checksums(checksums: dict[str, str]) -> None:
    ordered = dict(sorted(checksums.items(), key=lambda item: (migration_number(item[0]), item[0])))
    try:
        with CHECKSUMS_FILE.open("w", encoding="utf-8") as destination:
            json.dump(ordered, destination, indent=2)
            destination.write("\n")
    except OSError as error:
        raise ValueError(f"could not write {CHECKSUMS_FILE}: {error}") from error


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--update",
        action="store_true",
        help="record newly appended migrations after verifying every historical checksum",
    )
    args = parser.parse_args()

    try:
        disk = migrations_on_disk()
        try:
            recorded = read_checksums()
        except FileNotFoundError:
            if not args.update:
                print(
                    f"error: checksum manifest not found: {CHECKSUMS_FILE}\n"
                    "Bootstrap it intentionally with: python3 "
                    ".github/scripts/check_migration_immutability.py --update",
                    file=sys.stderr,
                )
                return 2
            recorded = {}

        if not disk and not recorded:
            raise ValueError(f"no numbered SQL migrations found under {MIGRATIONS_DIR}")

        failures: list[str] = []
        parent = read_parent_checksums()
        if parent is not None:
            for filename, expected in parent.items():
                if recorded.get(filename) != expected:
                    failures.append(
                        f"historical checksum entry changed or was deleted: {filename}"
                    )
            parent_highest = max((migration_number(name) for name in parent), default=0)
            for filename in recorded.keys() - parent.keys():
                if migration_number(filename) <= parent_highest:
                    failures.append(
                        f"checksum entry {filename} is not an append "
                        f"(parent highest number is {parent_highest})"
                    )

        for filename, expected in recorded.items():
            path = disk.get(filename)
            if path is None:
                failures.append(f"historical migration was deleted: {filename}")
                continue
            actual = sha384(path)
            if actual != expected:
                failures.append(
                    f"historical migration changed: {filename}\n"
                    f"      recorded: {expected}\n"
                    f"      actual:   {actual}"
                )

        unrecorded = [name for name in disk if name not in recorded]
        if recorded:
            highest_recorded = max(migration_number(name) for name in recorded)
            for filename in unrecorded:
                if migration_number(filename) <= highest_recorded:
                    failures.append(
                        f"unrecorded migration {filename} is not an append "
                        f"(highest recorded number is {highest_recorded:04d})"
                    )

        if failures:
            print(f"migration immutability: FAILED — {len(failures)} problem(s):\n", file=sys.stderr)
            for failure in failures:
                print(f"  - {failure}", file=sys.stderr)
            print("\nHistorical migrations are immutable; fix forward in a new migration.", file=sys.stderr)
            return 1

        if unrecorded and not args.update:
            print("migration immutability: FAILED — unrecorded appended migration(s):", file=sys.stderr)
            for filename in sorted(unrecorded, key=migration_number):
                print(f"  - {filename}", file=sys.stderr)
            print(
                "\nIf these additions are intentional, run: python3 "
                ".github/scripts/check_migration_immutability.py --update\n"
                "Then commit migrations/checksums.json with the new SQL files.",
                file=sys.stderr,
            )
            return 1

        if unrecorded:
            for filename in unrecorded:
                recorded[filename] = sha384(disk[filename])
            write_checksums(recorded)
            print(
                f"migration immutability: updated — recorded {len(unrecorded)} new migration(s); "
                f"{len(recorded)} total"
            )
            return 0

        print(f"migration immutability: OK — {len(recorded)} historical migration(s) unchanged")
        return 0
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
