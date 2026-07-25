#!/usr/bin/env bash
# Codypendent installer — downloads the latest release tarball for this machine
# and installs `codypendent` onto your PATH (self-sufficient — it runs the
# daemon from itself; the optional standalone `codypendentd` is installed too if
# the tarball carries it).
#
# One-liner (uses your existing `gh` auth, so it works for a private repo):
#
#   gh api repos/CodeHalwell/codypendent/contents/install.sh \
#     -H 'Accept: application/vnd.github.raw' | bash
#
# Install a specific release instead of the latest:
#
#   … | bash -s -- v0.1.0-build.17
#
# Override the install dir (default /usr/local/bin):
#
#   … | CODYPENDENT_BIN="$HOME/.local/bin" bash
set -euo pipefail

REPO="CodeHalwell/codypendent"
BINDIR="${CODYPENDENT_BIN:-/usr/local/bin}"

command -v gh  >/dev/null || { echo "error: GitHub CLI (gh) is required — https://cli.github.com" >&2; exit 1; }
command -v tar >/dev/null || { echo "error: tar is required" >&2; exit 1; }

# 1. Detect this machine's release target.
os="$(uname -s)"; arch="$(uname -m)"
case "$os/$arch" in
  Darwin/arm64)   target="aarch64-apple-darwin" ;;
  Darwin/x86_64)  target="x86_64-apple-darwin" ;;
  Linux/x86_64)   target="x86_64-unknown-linux-gnu" ;;
  *) echo "error: no prebuilt binary for $os/$arch (Windows is unsupported)." >&2; exit 1 ;;
esac

# 2. Resolve the release tag: first arg wins; otherwise the newest release
#    (rolling builds are prereleases, so we ask for the latest of ALL releases).
tag="${1:-}"
if [ -z "$tag" ]; then
  tag="$(gh release list -R "$REPO" -L 1 --json tagName --jq '.[0].tagName' 2>/dev/null || true)"
fi
[ -n "$tag" ] || { echo "error: no releases found on $REPO" >&2; exit 1; }

asset="codypendent-$target.tar.gz"
echo "codypendent: installing $tag ($target) -> $BINDIR"

# 3. Download + extract into a temp dir that is always cleaned up.
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
gh release download "$tag" -R "$REPO" -p "$asset" -D "$tmp" --clobber
tar -xzf "$tmp/$asset" -C "$tmp"
src="$tmp/codypendent-$target"
[ -x "$src/codypendent" ] || { echo "error: codypendent binary missing in $asset" >&2; exit 1; }

# 4. macOS: clear the Gatekeeper quarantine on the unsigned binaries so they run
#    without the "developer cannot be verified" block.
if [ "$os" = Darwin ]; then
  xattr -dr com.apple.quarantine "$src" 2>/dev/null || true
fi

# 5. Install `codypendent` (self-sufficient — it runs the daemon from itself
#    via `codypendent __daemon`). Also install the OPTIONAL standalone
#    `codypendentd` if the tarball carried it. Use sudo only if the target dir
#    is not writable.
bins=("$src/codypendent")
[ -x "$src/codypendentd" ] && bins+=("$src/codypendentd")
mkdir -p "$BINDIR" 2>/dev/null || true
if [ -w "$BINDIR" ]; then
  install -m 0755 "${bins[@]}" "$BINDIR"/
else
  echo "codypendent: $BINDIR is not writable — using sudo"
  sudo install -m 0755 "${bins[@]}" "$BINDIR"/
fi

if [ -x "$src/codypendentd" ]; then
  echo "codypendent: installed $BINDIR/codypendent and $BINDIR/codypendentd"
else
  echo "codypendent: installed $BINDIR/codypendent"
fi
case ":$PATH:" in
  *":$BINDIR:"*) echo "codypendent: run  codypendent" ;;
  *) echo "codypendent: add $BINDIR to your PATH, then run  codypendent" ;;
esac
