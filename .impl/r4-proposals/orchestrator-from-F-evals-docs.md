# Proposal: three version/toolchain claims in files I must not edit

From **F-evals-docs** to the **orchestrator** (owner of `Cargo.toml` and
`migrations/`). Round 4. All three are documentation-only; none changes a build.

## 1. `Cargo.toml` contradicts itself about the MSRV, 51 lines apart

- `Cargo.toml:34` — `rust-version = "1.88"`
- `Cargo.toml:85` — a comment: *"Pinned to a version that builds on the
  workspace's rust-version (**1.82**)"*

I fixed the same drift in the build guide's direction last round
(`docs/docs/build/00-how-to-use-this-guide.md` now says 1.88 / agent-framework
0.2.0, matching `Cargo.toml` and `Cargo.lock` — re-verified today). The comment
at `:85` is the last copy of the old number, and it is the one a contributor
reads while editing that dependency. Suggested: change `1.82` → `1.88` in that
comment, or drop the parenthetical so there is one MSRV in the file.

## 2. `Cargo.toml:32-33` calls `rust-toolchain.toml` a pin; it is a channel

```
$ cat rust-toolchain.toml
[toolchain]
channel = "stable"
```

`Cargo.toml:32-33`'s safety argument reads *"The **pinned** toolchain
(`rust-toolchain.toml`) is newer, so builds are unaffected"*. `stable` is a
floating channel: it is newer today and will be different next month, so the
argument rests on something that is not a pin. Either pin a version there
(`channel = "1.88"`), or reword the comment to say "the stable channel, which
is currently newer". This is your call because pinning changes what CI compiles
with.

## 3. `migrations/README.md` is now the only fully-correct account — worth a link

No change requested to the file itself (and I did not touch it). Recording that
I re-verified its claim today, since three other documents cited the wrong
migration and I corrected them (`ROADMAP.md`,
`docs/docs/build/99-master-acceptance-checklist.md`,
`docs/cli-and-tui-user-guide.md`, plus a correction note in
`docs/releases/v0.5.1.md`):

| Published build | `migrations/0003_phase2.sql` sha256 (first 12) | bytes |
|---|---|---|
| `v0.1.0-build.42` | `a29143289fa4` | 6661 |
| `v0.1.0-build.43`, `.44`, `.45` | **`a5c81199c24b`** | 6828 |
| `v0.1.0-build.46`, `v0.1.1-build.50`, `v0.5.1`, HEAD | `a29143289fa4` | 6661 |

`0017_promotion_evidence.sql` — which the ROADMAP and the acceptance checklist
both blamed — is byte-identical (`5d5adab8ca8a`, 1490 bytes) at
`v0.1.1-build.50`, `v0.5.1` and HEAD, and there is no `v0.1.1` tag at all
(releases API: 404). The conclusion those documents drew was right; the example
was wrong.

## 4. Not mine, no clear owner: `extensions/vscode/package.json` is five releases behind

`"version": "0.4.2"` against a workspace at `0.5.1`. `extensions/**` is not in
the ownership table and bumping a published extension's version is a release
decision, so I left it. Flagging it here so it is not lost: it was three
releases behind at the previous review and is five behind now, so nothing is
keeping it in step.
