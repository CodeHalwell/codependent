# Policy files

Codypendent reads up to two policy files before a run starts, merges them
over a built-in default policy, and uses the result to decide every
filesystem read/write, shell command, network request, and git/GitHub
mutation the agent proposes. This page covers where the files live, what
they can and cannot do, and what happens when one is broken.

The code that implements all of this:

- `crates/daemon/src/policy/config.rs` — the TOML shape (`RawPolicy`), the
  merge rules (`MergedPolicy::apply_trusted_overlay` /
  `apply_untrusted_overlay`), and the built-in defaults.
- `crates/protocol/src/discovery.rs` (`RuntimePaths::global_policy_path`) —
  where the global file lives.
- `crates/daemon/src/policy/mod.rs` (`PolicyEngine::load`) — how the two
  files are loaded and which trust rule applies to each.
- `crates/codypendentd/src/executor.rs` (`Executor::load_run_policy`) — where
  a run wires this into an actual `PolicyEngine`.

## Two files, two trust levels

| Layer | Path | Trust | May widen? |
|---|---|---|---|
| Global | `<config_dir>/policy.toml` | Trusted (yours) | Yes — widen or narrow |
| Repo-local | `<repo>/.codypendent/policy.toml` | Untrusted | No — narrow only |

Both are optional. A missing file is a normal, unconfigured state — that
layer simply contributes nothing and the merge proceeds with whatever the
broader layers (built-in defaults, then global) already established.

### Global config: `<config_dir>/policy.toml`

`config_dir` is the OS convention for the `codypendent` config directory
(e.g. `~/.config/codypendent/` on Linux), or the directory named by the
`CODYPENDENT_CONFIG_DIR` environment variable if it is set to a non-empty
value. So by default this file is:

```
~/.config/codypendent/policy.toml
```

This file is **yours** — it lives on your own machine, under your own
control, outside any repository the agent might be operating on. Because of
that it is trusted, and a trusted layer is allowed to *widen* authority as
well as narrow it:

- add programs to the shell allow-list (e.g. `pytest`, `npm`),
- add endpoints to the network allow-list, or relax `network.default` to
  `"allow"`,
- widen `fs_read` to cover more of the filesystem,
- relax git approval dispositions (`git.commit`, `git.push`,
  `git.force_push`, `git.delete_branch`) toward `"allow"`.

It can also narrow any of the above — trusted just means "may move in either
direction," not "must widen."

### Repo-local config: `<repo>/.codypendent/policy.toml`

This file travels with the repository. Since the agent may be pointed at a
repository it is merely reviewing — one it does not own and did not write —
this file is treated as **untrusted**: it can only make things *stricter*
than whatever the built-in defaults and your global config already allow. In
`PolicyEngine::load`, the repo-local layer is applied last, specifically so
it can always claw back authority the global layer granted, but it can never
exceed what came before it. Concretely, `apply_untrusted_overlay`:

- intersects the shell allow-list and network allow-list (can only remove
  entries, never add one),
- intersects `fs_read`/`fs_write` root lists (can only shrink the allowed
  region),
- unions the `fs_deny` list (can only add denials, never remove one),
- ratchets git/network approval dispositions toward the stricter value
  (e.g. it can turn `"allow"` into `"approval"`, never the reverse).

This is the mechanism that stops a malicious or compromised repository from
granting the agent more power than your own machine already grants it — a
repo-local `policy.toml` cannot add a shell program, widen a scope, or relax
an approval, no matter what it contains.

## The `pytest` example

By default the shell allow-list is Rust-and-exploration oriented (`cargo`,
`git`, `rg`, `rustfmt`, plus a curated read-only set like `ls`/`cat`/`grep`).
It does not include test runners for other ecosystems. To let the agent run
a `pytest`-based test suite, add the program to your **global**
`policy.toml`:

```toml
# ~/.config/codypendent/policy.toml
[shell]
allowed_programs = ["pytest"]
```

This is a union with the built-in allow-list, not a replacement — `cargo`,
`git`, etc. all still work. It only changes `pytest` from **denied outright**
(`policy.program-not-allowlisted`) to **eligible for approval**
(`policy.command-requires-approval`): allow-listing a program never makes it
auto-run. Every shell command, allow-listed or not, still stops at a human
approval gate (`PolicyEngine::eval_command` requires approval for every
allow-listed program). Widening the allow-list only decides which commands
are *considered* at all — not whether they run without a human saying yes.

The same `[shell] allowed_programs = ["pytest"]` written in a repo-local
`.codypendent/policy.toml` instead has no effect: the untrusted merge
intersects allow-lists, so a repo cannot add `pytest` (or anything else) to
what your global config and the built-in defaults already permit.

## Limits that no config file can lift

**`fs_write` never widens**, even from the trusted global config. Writes
are confined to `$WORKTREE` — the isolated worktree a run operates in — and
that floor holds regardless of what any policy file, including your own
global one, sets `filesystem.write` to. `apply_trusted_overlay` still routes
`fs_write` through the narrow-only intersection (the same path the untrusted
overlay uses), specifically so this one field cannot be relaxed from any
source. If you write `write = ["$HOME"]` in your global config, the merged
`fs_write` scope simply keeps `$WORKTREE` (or empties, since `$HOME` is
disjoint from it) — it does not gain `$HOME`.

**Path entries must be anchored.** Every `fs_read`, `fs_write`, and
`fs_deny` entry — in the built-in defaults, the global config, or the
repo-local config — must begin with one of three recognized anchor tokens:

- `$REPOSITORY` — the repository root,
- `$WORKTREE` — the run's isolated worktree,
- `$HOME` — the operator's home directory.

A path is expanded and resolved against these anchors only at evaluation
time, but a merge-time safety check (`normalize_raw` in `config.rs`) runs
first: it lexically collapses `.`/`..` in the raw, unexpanded string,
treating the anchor as an immovable component a `..` may never pop above.
Two kinds of entries are **dropped outright, fail-closed**, before they can
affect the merged scope at all:

- a bare absolute path with no recognized anchor, e.g. `/etc` or
  `/opt/data` — this checker cannot reason about a path it doesn't
  recognize, so it treats it as unsafe;
- a path that escapes its anchor via `..`, e.g. `$WORKTREE/../etc` or
  `$HOME/../../etc`.

This applies even on the trusted global-widen path: a dropped root is a
narrower merged scope, so it is always the fail-closed direction, whether
the source is trusted or not. If you write an anchor-less or escaping path
in either policy file, expect it to be silently absent from the effective
scope (a `tracing::warn!` is emitted for the dropped root) rather than
rejected as a parse error — write scopes with `$REPOSITORY`, `$WORKTREE`,
or `$HOME` as the leading path segment to avoid this.

## Malformed policy fails the run

A policy file that exists but does not parse — including one with an
unrecognized key anywhere in it, since every section uses
`#[serde(deny_unknown_fields)]` — is **never** silently ignored and never
silently replaced with the built-in defaults. `PolicyEngine::load` returns
an `Err(PolicyLoadError::Parse { .. })`, and the executor
(`Executor::load_run_policy`) maps it to:

```
policy configuration error: failed to parse policy file <path>: <toml error>
```

and the run does not start. This is deliberate: falling back to
`with_defaults` on a broken file would silently *widen* the effective
policy back to the (weaker) built-ins for a layer you meant to narrow, or
silently drop a widen (like the `pytest` line above) you deliberately wrote
into your global config — either way, an inaudible change in what the agent
is allowed to do. Fix the TOML (or remove the file, which is a supported,
fully-legible state) and the run starts again.

A file that simply does not exist is **not** an error — that layer is
skipped, as if it were never configured.
