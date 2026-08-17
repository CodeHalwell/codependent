import type { UiWireMessage } from "../protocol.js";

export const REMOTE_UI_WIRE_RECEIVE_EVENT = "codypendent:remote-ui-wire";
export const REMOTE_UI_WIRE_SEND_EVENT = "codypendent:remote-ui-send";

const MEDIATED_RUNTIME_TYPES = new Set(["subscription", "unsubscribe", "action", "cancelAction"]);
const MEDIATED_HOST_TYPES = new Set(["projection", "actionResult", "subscription", "unsubscribe", "action", "cancelAction"]);

function wireType(message: { type?: string; kind?: string }): string {
  return message.type ?? message.kind ?? "";
}

export function isMediatedRuntimeWire(value: unknown): value is UiWireMessage {
  return typeof value === "object" && value !== null && MEDIATED_RUNTIME_TYPES.has(wireType(value as { type?: string; kind?: string }));
}

export function isMediatedHostWire(value: unknown): value is UiWireMessage {
  return typeof value === "object" && value !== null && MEDIATED_HOST_TYPES.has(wireType(value as { type?: string; kind?: string }));
}

/** Subscribe a trusted SDK projection adapter to host projection/action results. */
export function subscribeMediatedWire(listener: (message: UiWireMessage) => void): () => void {
  const handler = (event: Event): void => {
    const message = event instanceof CustomEvent ? event.detail : undefined;
    if (isMediatedHostWire(message)) listener(message);
  };
  if (typeof window !== "undefined") {
    window.addEventListener(REMOTE_UI_WIRE_RECEIVE_EVENT, handler);
    return () => window.removeEventListener(REMOTE_UI_WIRE_RECEIVE_EVENT, handler);
  }
  return () => {};
}

/** Send only subscription, action, or cancellation requests toward the host. */
export function sendMediatedWire(message: UiWireMessage): boolean {
  if (!isMediatedRuntimeWire(message)) return false;
  if (typeof window === "undefined") return false;
  return window.dispatchEvent(new CustomEvent(REMOTE_UI_WIRE_SEND_EVENT, { detail: message }));
}

export function publishMediatedWire(message: UiWireMessage): void {
  if (isMediatedHostWire(message) && typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(REMOTE_UI_WIRE_RECEIVE_EVENT, { detail: message }));
  }
}
