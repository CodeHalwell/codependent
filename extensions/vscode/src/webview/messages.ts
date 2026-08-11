import type { UiCapabilities, UiHostMessage, UiRuntimeMessage } from "@codypendent/ui";

import type { UiWireTheme } from "../remote-ui/wire.js";
import type { UiWireMessage } from "../remote-ui/wire.js";
import type { RemoteUiPlacement } from "./remote-ui/store.js";

/** Messages posted from the extension host into the webview. */
export type TranscriptMessage =
  | { kind: "status"; status: string }
  | { kind: "event"; sequence: number; label: string; detail: string }
  | { kind: "runState"; runId: string; state: string }
  | { kind: "approval"; approvalId: string; summary: string; risk: string }
  | { kind: "approvalResolved"; approvalId: string; decision: string }
  | { kind: "remoteUi"; message: UiHostMessage; placement?: RemoteUiPlacement }
  | { kind: "remoteUiPlacement"; documentId: string; placement: RemoteUiPlacement }
  | { kind: "remoteUiContributions"; owner: string; registrations: { documentId: string; placement: RemoteUiPlacement }[] }
  | { kind: "remoteUiWire"; message: UiWireMessage }
  | { kind: "remoteUiTheme"; theme: UiWireTheme }
  | { kind: "remoteUiConfigure"; showTerminalFallback?: boolean }
  | { kind: "clear" };

/** Messages posted from the webview back to the extension host. */
export type WebviewCommandMessage =
  | { kind: "approve"; approvalId: string }
  | { kind: "reject"; approvalId: string }
  | { kind: "startRun"; objective: string }
  | { kind: "remoteUiRuntime"; message: UiRuntimeMessage }
  | { kind: "remoteUiWire"; message: UiWireMessage }
  | { kind: "remoteUiReady"; capabilities: UiCapabilities; documents: { documentId: string; revision: number }[] };
