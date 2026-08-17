#!/bin/sh
set -eu

export LC_ALL=C

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
package="$repo_root/sdk/control-plane"
committed="$package/src/generated"
temporary=$(mktemp -d "${TMPDIR:-/tmp}/codypendent-typescript-control-plane.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

if [ ! -d "$package/node_modules/json-schema-to-typescript" ]; then
    echo "error: sdk/control-plane dependencies are not installed" >&2
    echo "run from any directory: npm --prefix '$package' ci" >&2
    exit 1
fi

if [ ! -d "$committed" ]; then
    echo "error: committed generated TypeScript directory is missing: $committed" >&2
    exit 1
fi

node "$package/scripts/generate.mjs" --output-dir "$temporary/first"
node "$package/scripts/generate.mjs" --output-dir "$temporary/second"

if ! diff -ru "$temporary/first" "$temporary/second"; then
    echo "error: TypeScript control plane generation is nondeterministic" >&2
    exit 1
fi

if ! diff -ru "$committed" "$temporary/first"; then
    echo "error: committed TypeScript control plane bindings are stale" >&2
    echo "regenerate them from any directory with: npm --prefix '$package' run generate" >&2
    exit 1
fi

echo "generated TypeScript control plane bindings: OK"
