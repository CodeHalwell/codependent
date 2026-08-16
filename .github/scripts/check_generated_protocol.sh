#!/bin/sh
set -eu

export LC_ALL=C

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
committed="$repo_root/sdk/protocol/schema"
temporary=$(mktemp -d "${TMPDIR:-/tmp}/codypendent-protocol-schema.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

first="$temporary/first"
second="$temporary/second"

export_schema() {
    cargo run \
        --quiet \
        --locked \
        --manifest-path "$repo_root/Cargo.toml" \
        --package codypendent-protocol \
        --features schema-export \
        --bin export_schema \
        -- \
        --output-dir "$1"
}

if [ ! -d "$committed" ]; then
    echo "error: committed protocol schema directory is missing: $committed" >&2
    exit 1
fi

export_schema "$first"
export_schema "$second"

if ! diff -ru "$first" "$second"; then
    echo "error: protocol schema export is nondeterministic" >&2
    exit 1
fi

if ! diff -ru "$committed" "$first"; then
    echo "error: committed protocol schemas are stale" >&2
    echo "regenerate them with:" >&2
    echo "  rm -rf sdk/protocol/schema && cargo run --locked -p codypendent-protocol --features schema-export --bin export_schema -- --output-dir sdk/protocol/schema" >&2
    exit 1
fi

echo "generated protocol schemas: OK"
