import { describe, expect, it } from "vitest";
import { UI_CONTRIBUTION_POINTS, UI_PROTOCOL_VERSION, type UiEvent } from "@codypendent/ui";

import { isMediatedRuntimeWire, isUiEvent, isUiRuntimeMessage, isUiWireMessage, runtimeToWire, wireToHost } from "../src/remote-ui/wire.js";
import { createWebviewCapabilities, supportedContributionPoint } from "../src/webview/remote-ui/capabilities.js";

const EVENT: UiEvent = {
  protocolVersion: UI_PROTOCOL_VERSION,
  eventId: "event-1",
  documentId: "document-1",
  revision: 4,
  targetId: "button-1",
  type: "action",
  payload: { action: "build.open" },
};

describe("Remote UI wire bridge", () => {
  it("accepts bounded revision-bound events and rejects malformed input", () => {
    expect(isUiEvent(EVENT)).toBe(true);
    expect(isUiEvent({ ...EVENT, revision: -1 })).toBe(false);
    expect(isUiEvent({ ...EVENT, protocolVersion: { major: 99, minor: 0 } })).toBe(false);
    expect(isUiEvent({ ...EVENT, type: "executeArbitraryCode" })).toBe(false);
    expect(isUiRuntimeMessage({ type: "event", event: EVENT })).toBe(true);
  });

  it("wraps events in the dedicated daemon RemoteUi message shape", () => {
    expect(runtimeToWire({ type: "event", event: EVENT })).toEqual({
      type: "event",
      messageId: EVENT.eventId,
      event: EVENT,
    });
  });

  it("rejects malformed daemon envelopes before they reach the webview", () => {
    expect(isUiWireMessage({ kind: "snapshot", messageId: "one", snapshot: { document: {} } })).toBe(true);
    expect(isUiWireMessage({ kind: "snapshot", messageId: "one", snapshot: {} })).toBe(false);
    expect(isUiWireMessage({ kind: "event", messageId: "one", event: { ...EVENT, targetId: "" } })).toBe(false);
    expect(isUiWireMessage({ kind: "contributions", messageId: "one", contributions: [{ point: "panel" }] })).toBe(false);
    expect(isUiWireMessage({ type: "contributions", messageId: "empty", contributions: [] })).toBe(false);
    expect(isUiWireMessage({
      type: "contributions",
      messageId: "empty-owned",
      contributions: [],
      extensions: { contributionOwner: "acme.plugin" },
    })).toBe(true);
    expect(isUiWireMessage({ type: "event", kind: "snapshot", messageId: "one", event: EVENT })).toBe(false);
    expect(isUiWireMessage({ type: "event", messageId: "one", event: EVENT, cancellation: { invocationId: "two" } })).toBe(false);
  });

  it("safely preserves mediated projection and action message types", () => {
    const messages = [
      { type: "subscription", messageId: "sub-message", subscription: { subscriptionId: "sub-1", kind: "session", resourceId: "session-1" } },
      { type: "projection", messageId: "projection-message", projection: { subscriptionId: "sub-1", revision: 4, value: { id: "session-1", state: "Running" } } },
      { type: "action", messageId: "action-message", action: { invocationId: "invoke-1", documentId: "doc", revision: 2, sourceNodeId: "button", actionId: "run.start", payload: {} } },
      { type: "actionResult", messageId: "result-message", actionResult: { invocationId: "invoke-1", status: "succeeded", value: { accepted: true } } },
      { type: "cancelAction", messageId: "cancel-message", cancellation: { invocationId: "invoke-1" } },
    ] as const;
    for (const message of messages) {
      expect(isUiWireMessage(message), message.type).toBe(true);
      expect(wireToHost(message).mediated).toEqual([message]);
    }
    expect(isMediatedRuntimeWire(messages[0])).toBe(true);
    expect(isMediatedRuntimeWire(messages[2])).toBe(true);
    expect(isMediatedRuntimeWire(messages[4])).toBe(true);
    expect(isMediatedRuntimeWire(messages[1])).toBe(false);
    expect(isMediatedRuntimeWire(messages[3])).toBe(false);
    expect(isUiWireMessage({ type: "projection", messageId: "bad", projection: { subscriptionId: "sub-1", removed: true, value: "still here" } })).toBe(false);
    expect(isUiWireMessage({ type: "actionResult", messageId: "bad", actionResult: { invocationId: "invoke-1", status: "failed" } })).toBe(false);
  });

  it("translates snapshots, patches, errors, contributions, and themes", () => {
    const document = {
      protocolVersion: UI_PROTOCOL_VERSION,
      documentId: "document-1",
      revision: 1,
      root: { kind: "text" as const, id: "root", text: "hello" },
    };
    const projection = wireToHost({
      kind: "snapshot",
      messageId: "wire-1",
      snapshot: { document },
      contributions: [{ id: "item", extensionId: "acme", point: "sidebar", slot: "top", documentId: "document-1", priority: 8 }],
      theme: { id: "dark", name: "Dark", revision: 1, tokens: { accent: "#fff" } },
    });
    expect(projection.messages).toEqual([{ type: "snapshot", document }]);
    expect(projection.placements.get("document-1")).toEqual({
      point: "sidebar",
      extensionId: "acme",
      ownerScope: "acme",
      slot: "top",
      priority: 8,
    });
    expect(projection.theme?.id).toBe("dark");
  });

  it("preserves the opaque replacement scope while exposing attested extension chrome", () => {
    const projection = wireToHost({
      type: "contributions",
      messageId: "scoped-replacement",
      extensions: { contributionOwner: "ui-producer:018f" },
      contributions: [{
        id: "opaque-contribution",
        extensionId: "ui-producer:018f",
        point: "panel",
        slot: "panel",
        documentId: "opaque-document",
        priority: 0,
        metadata: {
          hostExtensionId: "acme.plugin",
          hostPublisher: "Acme",
          hostTrust: "signed",
        },
      }],
    });
    expect(projection.contributionReplacement).toEqual({
      owner: "ui-producer:018f",
      registrations: [{
        documentId: "opaque-document",
        point: "panel",
        extensionId: "acme.plugin",
        ownerScope: "ui-producer:018f",
        publisher: "Acme",
        trust: "signed",
        slot: "panel",
        priority: 0,
      }],
    });
  });

  it("validates advertised web capabilities", () => {
    const capabilities = createWebviewCapabilities({ width: 900, height: 700 });
    expect(capabilities.capabilities).toContain("command-invoke");
    expect(capabilities.contributionPoints).toEqual([
      "sidebar", "panel", "status-item", "command", "command-palette",
      "composer-accessory", "message-renderer", "tool-renderer", "artifact-renderer",
      "workflow-inspector", "blackboard-renderer", "document-block", "code-graph-node",
      "settings-section", "setup-step", "form", "wizard", "dashboard-card",
      "trace-span-renderer", "context-menu", "quick-pick", "notification",
    ]);
    expect(isUiRuntimeMessage({ type: "capabilities", capabilities })).toBe(true);
    expect(isUiRuntimeMessage({ type: "capabilities", capabilities: { ...capabilities, viewport: { width: 0, height: 0 } } })).toBe(false);
  });

  it("accepts every offered contribution point at webview ingress and rejects unknown points", () => {
    for (const point of UI_CONTRIBUTION_POINTS) {
      expect(supportedContributionPoint(point), point).toBe(point);
    }
    expect(supportedContributionPoint("approval-frame")).toBeUndefined();
    expect(supportedContributionPoint("future-unadvertised-point")).toBeUndefined();
    expect(supportedContributionPoint(undefined)).toBeUndefined();
  });
});
