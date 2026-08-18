/**
 * Conformance guard for the ONE remaining locally-declared wire view.
 *
 * The extension's 811-line hand-written `src/protocol/types.ts` mirror is gone;
 * every daemon wire type is imported from the generated `@codypendent/protocol`.
 * `src/remote-ui/wire.ts` is the single exception, and its module header states
 * why: the webview side of that boundary is typed against the `@codypendent/ui`
 * SDK's `UiDocument`/`UiPatchBatch`/`UiEvent`/`UiCapabilities` trees, while the
 * generated `UiWireMessage` nests the SCHEMA-generated versions of those same
 * trees. Identical bytes, non-assignable type families, so one declaration
 * cannot serve both sides yet.
 *
 * A locally-declared view is exactly how the wire drifted twice in one day: two
 * capability fields were added in Rust and BOTH hand-written copies silently
 * failed to grow them. So this view is guarded rather than trusted.
 *
 * THE MECHANISM — a compile-time PARTITION, deliberately the same shape as the
 * runtime `assertPartitionIsComplete` in `protocol-vectors.test.ts`:
 *
 *   for every shape, { generated field names } is partitioned into
 *     - MODELED   — a field the local view declares, and
 *     - NOT MODELED — a field this extension does not read, named explicitly
 *
 * `AssertSubsetOf` pins the *unmodeled remainder* to the declared "not modeled"
 * union — asserted in BOTH directions wherever that list is non-empty, so the
 * partition is exact rather than merely open-ended. Therefore:
 *
 *   - A field added to the Rust type lands in the remainder, no longer equals
 *     the declared union, and `npm run typecheck` FAILS **naming the field**.
 *     It cannot go missing silently — which is the whole point.
 *   - A field this view invents that the wire does not have fails the same way,
 *     via `AssertNoInventedKeys`.
 *   - Modeling a previously-unmodeled field also fails until its name is struck
 *     from the list, so the list cannot rot into a lie.
 *
 * These are `export type` (not `type`) purely so `noUnusedLocals` accepts them;
 * they are assertions, evaluated by `tsc`, and carry no runtime value. The
 * `it()` blocks below cover the half `tsc` cannot: that the fields named as NOT
 * MODELED are genuinely safe to ignore at runtime.
 */
import { describe, expect, it } from "vitest";

import type { UiWireMessage as GeneratedUiWireMessage } from "@codypendent/protocol";

import {
  isUiWireMessage,
  wireToHost,
  type UiActionInvocation,
  type UiActionResult,
  type UiProjectionSubscription,
  type UiProjectionUnsubscription,
  type UiProjectionUpdate,
  type UiWireContribution,
  type UiWireMessage,
  type UiWireSnapshot,
  type UiWireTheme,
} from "../src/remote-ui/wire.js";

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

/** String-valued property names of `T`. */
type KeyOf<T> = Extract<keyof T, string>;

/** Generated field names the local view declares no home for. */
type UnmodeledKeys<Local, Generated> = Exclude<KeyOf<Generated>, KeyOf<Local>>;

/** Local field names the generated wire does not declare. */
type InventedKeys<Local, Generated> = Exclude<KeyOf<Local>, KeyOf<Generated>>;

/**
 * Compiles only when every member of `Actual` is also a member of `Expected`.
 * A one-way constraint rather than a mutual one: mutual constraints are
 * circular in TypeScript (TS2313), and the one-way form is what makes the
 * compiler error NAME the offending field instead of just reporting `false`.
 *
 * Exactness is recovered by asserting both directions where it matters — see
 * `RemoteErrorNoNewWireFields` / `RemoteErrorNoStaleExclusions`.
 */
type AssertSubsetOf<Actual extends Expected, Expected> = Actual;

/** Compiles only when the local view invents no field the wire lacks. */
type AssertNoInventedKeys<T extends never> = T;

/** Pull a generated sub-shape out of the generated envelope, as `extension.ts` does. */
type Generated<K extends keyof GeneratedUiWireMessage> = NonNullable<GeneratedUiWireMessage[K]>;

// ---------------------------------------------------------------------------
// The partition: envelope
// ---------------------------------------------------------------------------

/**
 * The envelope models every generated field. `kind` is the one invented name:
 * it is the migration-era alias the Rust side still deserializes via `#[serde(alias)]`
 * but no longer emits, so it has no generated counterpart by design.
 */
export type EnvelopeUnmodeled = AssertSubsetOf<UnmodeledKeys<UiWireMessage, GeneratedUiWireMessage>, never>;
export type EnvelopeInvented = AssertNoInventedKeys<
  Exclude<InventedKeys<UiWireMessage, GeneratedUiWireMessage>, "kind">
>;

// ---------------------------------------------------------------------------
// The partition: sub-shapes
// ---------------------------------------------------------------------------

export type SnapshotUnmodeled = AssertSubsetOf<UnmodeledKeys<UiWireSnapshot, Generated<"snapshot">>, never>;
export type SnapshotInvented = AssertNoInventedKeys<InventedKeys<UiWireSnapshot, Generated<"snapshot">>>;

export type ContributionUnmodeled = AssertSubsetOf<UnmodeledKeys<UiWireContribution, Generated<"contributions">[number]>, never>;
export type ContributionInvented = AssertNoInventedKeys<
  InventedKeys<UiWireContribution, Generated<"contributions">[number]>
>;

export type ThemeUnmodeled = AssertSubsetOf<UnmodeledKeys<UiWireTheme, Generated<"theme">>, never>;
export type ThemeInvented = AssertNoInventedKeys<InventedKeys<UiWireTheme, Generated<"theme">>>;

export type SubscriptionUnmodeled = AssertSubsetOf<UnmodeledKeys<UiProjectionSubscription, Generated<"subscription">>, never>;
export type SubscriptionInvented = AssertNoInventedKeys<
  InventedKeys<UiProjectionSubscription, Generated<"subscription">>
>;

export type UnsubscriptionUnmodeled = AssertSubsetOf<UnmodeledKeys<UiProjectionUnsubscription, Generated<"unsubscription">>, never>;
export type UnsubscriptionInvented = AssertNoInventedKeys<
  InventedKeys<UiProjectionUnsubscription, Generated<"unsubscription">>
>;

export type ProjectionUnmodeled = AssertSubsetOf<UnmodeledKeys<UiProjectionUpdate, Generated<"projection">>, never>;
export type ProjectionInvented = AssertNoInventedKeys<InventedKeys<UiProjectionUpdate, Generated<"projection">>>;

export type ActionUnmodeled = AssertSubsetOf<UnmodeledKeys<UiActionInvocation, Generated<"action">>, never>;
export type ActionInvented = AssertNoInventedKeys<InventedKeys<UiActionInvocation, Generated<"action">>>;

export type ActionResultUnmodeled = AssertSubsetOf<UnmodeledKeys<UiActionResult, Generated<"actionResult">>, never>;
export type ActionResultInvented = AssertNoInventedKeys<InventedKeys<UiActionResult, Generated<"actionResult">>>;

// Inline shapes declared directly on the envelope.

export type CancellationUnmodeled = AssertSubsetOf<UnmodeledKeys<NonNullable<UiWireMessage["cancellation"]>, Generated<"cancellation">>, never>;
export type DisposeUnmodeled = AssertSubsetOf<UnmodeledKeys<NonNullable<UiWireMessage["dispose"]>, Generated<"dispose">>, never>;
export type ViewportUnmodeled = AssertSubsetOf<UnmodeledKeys<NonNullable<UiWireMessage["viewport"]>, Generated<"viewport">>, never>;
export type ResyncUnmodeled = AssertSubsetOf<UnmodeledKeys<NonNullable<UiWireMessage["resync"]>, Generated<"resync">>, never>;
export type HotReloadUnmodeled = AssertSubsetOf<UnmodeledKeys<NonNullable<UiWireMessage["hotReload"]>, Generated<"hotReload">>, never>;

/**
 * The one shape with a NON-EMPTY not-modeled list, and the only place this
 * guard found existing drift.
 *
 * The generated `UiRemoteError` carries `details`, `fallback` and `patchIndex`;
 * this view declares none of them, because `wireToHost` projects a daemon error
 * onto the SDK's `UiHostMessage` error, which has nowhere to put them:
 *
 *   - `fallback` nests a schema-generated `UiNode`, which is the very type
 *     family this boundary exists to keep separate — modeling it would drag the
 *     generated node tree into `@codypendent/ui`-typed code;
 *   - `details` and `patchIndex` are renderer diagnostics with no surface in
 *     the VS Code webview.
 *
 * They are ignored, never coerced to a placeholder. The `it()` block below
 * proves ignoring them is safe rather than merely assumed.
 */
/** A generated field appearing here that is not named below fails the build. */
export type RemoteErrorNoNewWireFields = AssertSubsetOf<
  UnmodeledKeys<NonNullable<UiWireMessage["error"]>, Generated<"error">>,
  "details" | "fallback" | "patchIndex"
>;

/**
 * The other direction, so the exclusion list cannot rot into a lie: if one of
 * these fields is later modeled (or removed from the Rust type), its name must
 * be struck from the list above or this fails, naming it.
 */
export type RemoteErrorNoStaleExclusions = AssertSubsetOf<
  "details" | "fallback" | "patchIndex",
  UnmodeledKeys<NonNullable<UiWireMessage["error"]>, Generated<"error">>
>;

export type RemoteErrorInvented = AssertNoInventedKeys<
  InventedKeys<NonNullable<UiWireMessage["error"]>, Generated<"error">>
>;

// ---------------------------------------------------------------------------
// The runtime half: fields named NOT MODELED must be safe to ignore
// ---------------------------------------------------------------------------

describe("remote-ui wire view conforms to the generated UiWireMessage", () => {
  it("accepts an error carrying the fields the view does not model", () => {
    // Shaped as the generated `UiRemoteError`, including all three unmodeled
    // fields, exactly as the daemon may emit them.
    const message = {
      type: "error",
      messageId: "msg-error-1",
      error: {
        code: "render_failed",
        message: "component threw",
        recoverable: true,
        documentId: "doc-1",
        nodeId: "node-7",
        recovery: "resync",
        details: { stack: "…" },
        patchIndex: 3,
        fallback: { plainText: "component unavailable" },
      },
    };

    expect(isUiWireMessage(message)).toBe(true);
  });

  it("projects that error onto a host message without inventing values for them", () => {
    const message: UiWireMessage = {
      type: "error",
      messageId: "msg-error-2",
      error: {
        code: "render_failed",
        message: "component threw",
        documentId: "doc-1",
      },
    };

    const projection = wireToHost({
      ...message,
      // Re-attach the unmodeled fields the way the wire delivers them; the
      // local view narrows them away rather than defaulting them.
      error: { ...message.error!, details: { stack: "…" }, patchIndex: 3 },
    } as UiWireMessage);

    expect(projection.messages).toEqual([
      { type: "error", documentId: "doc-1", code: "render_failed", message: "component threw" },
    ]);
    const [projected] = projection.messages;
    expect(Object.keys(projected!).sort()).toEqual(["code", "documentId", "message", "type"]);
  });

  it("still rejects an error message whose modeled fields are malformed", () => {
    // The unmodeled fields must not become a way to smuggle an invalid error
    // past validation: `code` is still required and still bounded.
    expect(
      isUiWireMessage({
        type: "error",
        messageId: "msg-error-3",
        error: { message: "no code", details: { stack: "…" }, patchIndex: 3 },
      }),
    ).toBe(false);
  });
});
