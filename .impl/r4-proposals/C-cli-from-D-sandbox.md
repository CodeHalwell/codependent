# Proposal: C-cli — two skill-trust surfaces that my `manifest.rs` change invalidates

From **D-sandbox**. Round 4, review finding F6
(`docs/reviews/2026-08-13-r4-verticals/sandbox-wasm-hooks.md` §5).

## What changed on my side

`crates/knowledge/src/manifest.rs` no longer derives a package's `TrustTier`
from `[trust] publisher`. **Every package loaded from disk is
`TrustTier::Community`** (`manifest::PACKAGE_TRUST_TIER`), whatever its
manifest says. `publisher` is still recorded verbatim, as an unverified claim.

Reason: `publisher` is package-authored. A cloned package writing
`publisher = "local-user"` was recorded `trust_tier = first_party`, and
`crates/knowledge/src/context.rs:686-689` renders that tier on the card it
discloses to the model — so the prompt-injection labelling was bypassed by one
line of attacker-controlled TOML. Verified against a live DB by the reviewer.

`load_package`'s signature is unchanged. Nothing in `crates/cli` fails to
compile, and no `crates/cli` test asserts a trust tier, so nothing breaks. Two
things are now *wrong* rather than broken:

## 1. `crates/cli/src/skill_writer.rs:117` — a doc comment that is now false

```rust
    /// `"local-user"` (the reserved value granting `TrustTier::FirstParty`)
    /// unless overridden — a skill authored on this machine, for this
    /// operator, by definition qualifies.
    pub publisher: String,
```

`"local-user"` grants nothing. Suggested replacement:

```rust
    /// `"local-user"` unless overridden — a claim about who authored the
    /// package, recorded and displayed but **not** a trust decision. Since the
    /// 2026-08-13 review every package loaded from disk is
    /// `TrustTier::Community` (`knowledge::manifest::PACKAGE_TRUST_TIER`);
    /// nothing a manifest says can raise its own tier.
    pub publisher: String,
```

## 2. `skill add` still prints nothing about what the sandbox can enforce

Not caused by my change — review finding F11, and it is in your file.
`crates/cli/src/commands.rs:651-677` prints only the install line. On a host
with no `bwrap` the backend is unavailable and **every** run of that skill
fails closed, and the user is told nothing. The diagnostic already exists and
has no caller:

```rust
codypendent_knowledge::SkillRunner::enforcing(gate)?.capability_diagnostic()
// -> "sandbox backend: none on linux (UNAVAILABLE — runs fail closed); degraded: ..."
```

Cheaper variant with no gate needed, if you would rather not construct a
runner in the CLI: `codypendent_sandbox::enforcing_executor()` returns the same
`Err` early, and `RefusingSandbox.capability_report().diagnostic()` renders the
platform text. Suggested, after the existing `installed skill …` line:

```rust
    if item.executable {
        match codypendent_sandbox::enforcing_executor() {
            Ok(executor) => {
                let report = executor.capability_report();
                if !report.enforces_exit_criteria() {
                    println!("warning: {}", report.diagnostic());
                }
            }
            Err(e) => println!(
                "warning: this skill carries executable behaviour, but no sandbox backend is \
                 available here, so every run will be refused: {e}"
            ),
        }
    }
```

Threat model 12 §7 previously claimed this was already done. I have corrected
that claim in `.impl/threat-models/12-executable-skills.md`; the code fix is
yours.
