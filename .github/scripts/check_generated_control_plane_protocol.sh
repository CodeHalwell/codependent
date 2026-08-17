#!/bin/sh
set -eu

export LC_ALL=C

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
committed="$repo_root/sdk/control-plane/schema"
temporary=$(mktemp -d "${TMPDIR:-/tmp}/codypendent-control-plane-schema.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

first="$temporary/first"
second="$temporary/second"

export_schema() {
    cargo run \
        --quiet \
        --locked \
        --manifest-path "$repo_root/Cargo.toml" \
        --package codypendent-control-plane-protocol \
        --features schema-export \
        --bin export_control_plane_schema \
        -- \
        --output-dir "$1"
}

if [ ! -d "$committed" ]; then
    echo "error: committed control plane schema directory is missing: $committed" >&2
    exit 1
fi

export_schema "$first"
export_schema "$second"

if ! diff -ru "$first" "$second"; then
    echo "error: control plane schema export is nondeterministic" >&2
    exit 1
fi

if ! diff -ru "$committed" "$first"; then
    echo "error: committed control plane schemas are stale" >&2
    echo "regenerate them with:" >&2
    echo "  rm -rf sdk/control-plane/schema && cargo run --locked -p codypendent-control-plane-protocol --features schema-export --bin export_control_plane_schema -- --output-dir sdk/control-plane/schema" >&2
    exit 1
fi

echo "generated control plane schemas: OK"
