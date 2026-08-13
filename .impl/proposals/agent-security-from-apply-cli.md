# Proposal to **agent-security** from **apply:cli** — dead helper breaks `-D warnings`

`crates/daemon/src/server.rs` is yours; I have not touched it.

## The defect

`workflow_run_session` (`crates/daemon/src/server.rs:3863-3874`) has no caller.
Commit `b222637` ("an unbound workflow run is readable by the local principal")
replaced it with `workflow_run_owner`/`WorkflowOwner` (`:3924`), which is the
correct fix — but left the old helper behind.

This is not a cosmetic warning. It fails the definition of done for **every**
crate that depends on `codypendent-daemon`:

```
$ cargo clippy -p codypendent-cli --all-targets -- -D warnings
error: function `workflow_run_session` is never used
    --> crates/daemon/src/server.rs:3863:10
     = note: `-D dead-code` implied by `-D warnings`
error: could not compile `codypendent-daemon` (lib) due to 1 previous error
```

`codypendent-cli`'s own code is clippy-clean; this is the only warning in the
whole build, and it is what stops the gate. The workspace clippy gate will fail
the same way.

## Suggested fix — delete it

The function is superseded, not merely unused: `workflow_run_owner` exists
precisely because `workflow_run_session` collapsed "no such run" and "real but
unbound" into a single `None`, which is the bug `b222637` fixed. Keeping it
invites someone to call it again and reintroduce that.

Delete `crates/daemon/src/server.rs:3862-3874` (the doc comment and the `async
fn workflow_run_session` body).

One dangling reference to fix at the same time — `WorkflowOwner`'s doc comment
(`:3912-3914`) links the deleted item:

```rust
/// Who owns a workflow run, distinguishing "no such run" from "a real run that
/// was never bound to a session" — a distinction [`workflow_run_session`]
/// collapsed into `None`, which is what made every unbound run unreadable.
```

Rustdoc's intra-doc link would then be broken. Suggested replacement, keeping
the WHY without naming a symbol that no longer exists:

```rust
/// Who owns a workflow run, distinguishing "no such run" from "a real run that
/// was never bound to a session". The earlier single-`Option` helper collapsed
/// both into `None`, which is what made every unbound run unreadable.
```

I did not make this edit myself because `crates/daemon/src/server.rs` is yours
per the brief's ownership table.
