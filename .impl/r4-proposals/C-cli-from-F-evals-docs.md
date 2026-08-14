# Proposal: `codypendent update` cites a restriction that does not exist

From **F-evals-docs** to **C-cli** (owner of `crates/cli/src/**`). Round 4.

`install.sh` is not in the ownership table; it is proposed here rather than
edited because it and `update.rs` are a matched pair — `update.rs`'s own module
doc says it "mirrors `install.sh`" — and because the functional half (§2) needs
a release download to verify, which I could not do (no `gh` in this container,
and a tarball extraction costs ~350 MB of the shared disk).

---

## The fact, verified 2026-08-13

```
$ curl -sS https://api.github.com/repos/CodeHalwell/codypendent \
    | python3 -c "import json,sys; d=json.load(sys.stdin); print('private=',d['private'],'visibility=',d['visibility'])"
private= False visibility= public
```

The repository is **public**. Anonymous `curl` reaches `raw.githubusercontent.com`
for this repo (I downloaded `migrations/0003_phase2.sql` at five different tags
that way while verifying a docs claim), and the releases API answers unauthenticated
(`/releases?per_page=100` → 73 entries, HTTP 200).

## 1. The error message is false — minimal, safe fix

`crates/cli/src/update.rs:115-121`:

```rust
fn require_gh() -> anyhow::Result<()> {
    which("gh").context(
        "GitHub CLI (`gh`) is required to download releases from the private repo — \
         install it from https://cli.github.com and run `gh auth login`",
    )?;
    Ok(())
}
```

A user without `gh` is told the repo is private. It is not; they are told to
authenticate for a restriction that does not exist, and there is no hint that
the requirement is an implementation choice rather than a permission wall.
Proposed replacement (text only — no behaviour change, nothing to re-verify):

```rust
fn require_gh() -> anyhow::Result<()> {
    // The repo is PUBLIC (verified 2026-08-13: `private=false`), so `gh` is an
    // implementation choice — it resolves the latest tag, the tag's commit and
    // the per-target asset in three calls — not a permission requirement. Say
    // that, so a user without `gh` knows the alternative is a manual download
    // and not an access request.
    which("gh").context(
        "GitHub CLI (`gh`) is required by `codypendent update` — install it from \
         https://cli.github.com. (The repository is public: you can also download \
         the release asset for your target from \
         https://github.com/CodeHalwell/codypendent/releases and install it by hand.)",
    )?;
    Ok(())
}
```

And the module doc at `crates/cli/src/update.rs:1-9`:

```rust
//! → tar → macOS quarantine clear → `install`), but in-process. The repo is
//! private, so downloads authenticate through `gh` exactly as the installer.
```

→

```rust
//! → tar → macOS quarantine clear → `install`), but in-process. Downloads go
//! through `gh` exactly as the installer does — for tag/commit/asset resolution
//! in three calls, NOT for access: the repo is public (verified 2026-08-13).
```

`install.sh:7` carries the identical claim and should change with it:

```sh
# One-liner (uses your existing `gh` auth, so it works for a private repo):
```

→

```sh
# One-liner (uses `gh` to resolve the release asset for your platform; the repo
# itself is public, so `gh` is a convenience, not an access requirement):
```

## 2. The functional half — `gh` is a hard dependency for a public download

`install.sh:25` exits 1 without `gh`, and `update.rs:67` (`require_gh`) refuses
before doing anything. Both are avoidable for a public repo: the three `gh`
calls (`release list`, `release view --json targetCommitish`, `release download
-p <asset>`) each have an unauthenticated REST equivalent
(`/releases?per_page=1`, `/releases/tags/<tag>`,
`/releases/download/<tag>/<asset>` via `curl -fsSL`). A `gh`-if-present,
`curl`-otherwise fallback would make the documented one-liner work on a clean
machine, which today it does not:

```
$ curl -fsSL https://raw.githubusercontent.com/.../install.sh | bash -s -- v0.5.1
error: GitHub CLI (gh) is required — https://cli.github.com
[exit 1]     (observed by the round-4 docs-vs-reality reviewer)
```

Rate limits are the one real caveat (60/hour unauthenticated per IP), which is
why this is proposed rather than asserted: it needs a real download to verify
and it is your call.

## 3. Two smaller ones in the same pair of files

- **`install.sh:23` — `CODYPENDENT_LIB` is a knob nothing reads.**
  `LIBDIR="${CODYPENDENT_LIB:-$(dirname "$BINDIR")/lib/codypendent}"`, but
  `grep -rn CODYPENDENT_LIB crates/` is empty (I re-ran it: no matches). The
  only probed paths are `<bindir>/node-runtime` and
  `<bindir>/../lib/codypendent/node-runtime`
  (`crates/daemon/src/remote_ui_plugins.rs:1517-1519`;
  `crates/cli/src/update.rs:248` builds the same
  `<install_dir>/../lib/codypendent`). Setting `CODYPENDENT_LIB` to anything
  else installs the sealed Node runtime where the daemon will never look, and
  Remote UI then fails closed. Either honour the variable in both probe sites or
  drop it from the installer.
- **`install.sh:14` — stale example tag** `… | bash -s -- v0.1.0-build.17`.
  Current release is v0.5.1.

## Why this is not just a wording nit

Every release note's Upgrade section sends a new user to one of these two paths,
and I have just rewritten `docs/releases/v0.5.0.md` and
`docs/releases/v0.5.1.md` to point at real, existing tags. The first thing a
new user meets is still an exit-1 that blames a private repository. It is the
highest-frequency first-contact failure in the product.

---

# Two more, same crate, found while repairing the eval gate

## 4. `eval run --policy`'s help text says the opposite of what the code does

`crates/cli/src/main.rs` (`EvalCommand::Run`'s `--policy` doc comment), as
printed by the shipped binary:

```
$ ./target/debug/codypendent eval run --help
      --policy <POLICY>  … The selection is recorded per case in the report
      (`routed_model`); it does not yet pin the daemon's own `StartRun`
      execution to that model …
```

It does pin it. `crates/cli/src/eval.rs:395-396` resolves `routed_model` per
case and `:601-612` passes it straight into `CommandBody::StartRun { model:
routed_model, … }`, with a comment at that call site saying so
("`--policy` pins this case's routed model"). So the `--help` a user reads and
the comment three files away contradict each other, and the help is the wrong
one. Suggested text for the doc comment:

```
/// The routing policy to select each case's model under. Resolved via
/// `codypendent-routing` over the persisted model profiles, fail-closed: an
/// unknown name or a case with no eligible model stops `eval run` before any
/// case executes. The selection is recorded per case in the report
/// (`routed_model`) AND pinned into that case's own `StartRun.model`, so the
/// model the policy chose is the model the daemon runs. Absent: every case
/// runs under the daemon's own default model resolution, unchanged.
```

## 5. `fixture_root` only works for a suite exactly two levels under `evals/`

`crates/cli/src/eval.rs:161-181` resolves a suite's fixture as
`<suite_dir>/../../fixtures/<name>.bundle`. `resolve_suite_dir` (`:129-146`)
deliberately accepts a suite at *any* path ("so a suite outside the default
layout, or an absolute path, also works") — and every one of those loads its
cases and then dies:

```
$ ./target/debug/codypendent eval run --suite evals/regressions --report /tmp/reg.json
eval: loaded 1 case(s) from evals/regressions
Error: fixture bundle not found at fixtures/tiny-crate.bundle (referenced by the suite's cases)
```

I worked around it in my own lane by moving that suite to
`evals/tasks/regressions/` (where the convention resolves; `--suite
regressions` now runs it, verified). The trap itself is still there for the
next suite anyone points `--suite` at. Suggested fix — search upward for the
`evals/` root instead of assuming a depth:

```rust
pub fn fixture_root(suite_dir: &Path, fixture_name: &str) -> anyhow::Result<PathBuf> {
    // Walk up from the suite looking for a sibling `fixtures/<name>.bundle`,
    // instead of assuming the suite sits exactly two levels under `evals/`.
    // `resolve_suite_dir` accepts a suite at ANY path, so assuming one depth
    // makes every other layout fail with a path the user never wrote.
    let mut dir = Some(suite_dir);
    while let Some(current) = dir {
        let candidate = current
            .join("fixtures")
            .join(format!("{fixture_name}.bundle"));
        if candidate.is_file() {
            return Ok(candidate);
        }
        dir = current.parent();
    }
    anyhow::bail!(
        "fixture bundle {fixture_name}.bundle not found in any `fixtures/` directory at or above \
         {} (suites conventionally live beside `evals/fixtures/`)",
        suite_dir.display()
    )
}
```

(`crates/cli/tests/eval_it.rs:565-591`'s `resolve_suite_dir_and_load_suite_and_
fixture_root` builds `<tmp>/evals/tasks/core` + `<tmp>/evals/fixtures/tiny-crate.bundle`
and would still pass unchanged; the loop finds the same file.)

Two stale references in your crate's comments after the move, whichever way
you go: `crates/cli/src/eval.rs:500` and `:504` still say `evals/regressions/`.
