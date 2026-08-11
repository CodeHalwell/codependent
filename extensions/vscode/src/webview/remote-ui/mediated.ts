import {
  isMediatedHostWire,
  isMediatedRuntimeWire,
  type UiWireMessage,
} from "../../remote-ui/wire.js";

export const REMOTE_UI_WIRE_RECEIVE_EVENT = "codypendent:remote-ui-wire";
export const REMOTE_UI_WIRE_SEND_EVENT = "codypendent:remote-ui-send";

/** Subscribe a trusted SDK projection adapter to host projection/action results. */
export function subscribeMediatedWire(listener: (message: UiWireMessage) => void): () => void {
  const handler = (event: Event): void => {
    const message = event instanceof CustomEvent ? event.detail : undefined;
    if (isMediatedHostWire(message)) listener(message);
  };
  window.addEventListener(REMOTE_UI_WIRE_RECEIVE_EVENT, handler);
  return () => window.removeEventListener(REMOTE_UI_WIRE_RECEIVE_EVENT, handler);
}

/** Send only subscription, action, or cancellation requests toward the host. */
export function sendMediatedWire(message: UiWireMessage): boolean {
  if (!isMediatedRuntimeWire(message)) return false;
  return window.dispatchEvent(new CustomEvent(REMOTE_UI_WIRE_SEND_EVENT, { detail: message }));
}

export function publishMediatedWire(message: UiWireMessage): void {
  if (isMediatedHostWire(message)) {
    window.dispatchEvent(new CustomEvent(REMOTE_UI_WIRE_RECEIVE_EVENT, { detail: message }));
  }
}
