# Proposal to the **VS Code extension** owner from **apply:daemon**

`extensions/vscode/**` is not in the brief's ownership table and is not mine, so
this is a note rather than a patch. **Nothing here is urgent and nothing is
broken** — I deliberately shaped this wave so your drift guard stays green.

## What I did, and why it does not break you

This wave added three wire shapes:

* `EventBody::RunUsage` (`crates/protocol/src/events.rs`) — a run's measured
  tokens/cost.
* Five memory commands + three memory payloads + `MemoryView` / `MemoryScope` /
  `MemoryScopeTier` / `MemoryEvidence` (`crates/protocol/src/memory.rs`).
* `CommandBody::SubmitEvalEvidence` (`crates/protocol/src/command.rs`).

`protocol-vectors/README.md` requires a new **variant** to get its own vector, or
the guard stays blind to it. But `extensions/vscode/test/protocol-vectors.test.ts`
iterates `keysWithPrefix(vectors, "EventBody")` over `events.json` with **no
exclusion list** — every `EventBody_*` key must be handled by
`reconstructEventBody` — and asserts a complete covered/excluded partition over
`command.json` and `run.json`. So adding keys to those files would have failed
your suite through no fault of yours.

Instead I followed the precedent `golden_vectors.rs` already set for the voice
and kanban additions and put every new vector in its **own file**:

* `protocol-vectors/usage.json` — `EventBody_RunUsage`
* `protocol-vectors/memory.json` — the memory commands, payloads, and projections
* `protocol-vectors/promotion_evidence.json` — `CommandBody_SubmitEvalEvidence`

`events.json`, `command.json`, `envelope.json` and `run.json` are **byte-identical**
to before this wave (`git status protocol-vectors/` shows only the three new
files). Your suite reads files by name, so it does not see them at all.

## What you may want to pick up, when it suits you

The extension does not model any of these today, and that is a bounded,
intentional gap — the same one `protocol-vectors/README.md` already documents for
workflow runs, the blackboard, documents and multimodal input. If you do model
them:

1. `EventBody::RunUsage` is the one worth having first — it is what lets a client
   show what a run cost. Its three numeric fields are all optional and **absent
   means unmeasured, never zero**; a UI that renders a missing `cost_micros` as
   `$0.00` would be actively wrong.
2. If you add a case for it in `reconstructEventBody`, also add a
   `loadVectors("usage.json")` describe block — otherwise the vector stays
   unexercised on your side.
3. The memory commands are Controller-role writes plus two reads; the extension
   has no memory surface today, so there is nothing to gain until it does.

## The one thing to watch

If a future wave adds a variant to `events.json`/`command.json`/`run.json`
directly (rather than to a new file), your partition/coverage assertions will
fail loudly — which is exactly what they are for. That failure is the signal to
add the case, not a reason to widen an exclusion list.
