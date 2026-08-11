import { createRoot } from "react-dom/client";
import type { UiEvent, UiRuntimeMessage } from "@codypendent/ui";

import type { TranscriptMessage, WebviewCommandMessage } from "../messages.js";
import { isMediatedRuntimeWire, isUiWireMessage } from "../../remote-ui/wire.js";
import { createWebviewCapabilities, supportedContributionPoint, viewportFromWindow } from "./capabilities.js";
import { RemoteUiRenderer } from "./renderer.js";
import { RemoteUiStore, type RemoteUiPlacement } from "./store.js";
import { applyWireTheme } from "./theme.js";
import { publishMediatedWire, REMOTE_UI_WIRE_SEND_EVENT } from "./mediated.js";

interface PersistedRemoteUiState {
  version: 1;
  remoteUi?: ReturnType<RemoteUiStore["serialize"]>;
}

interface VsCodeApi<State> {
  postMessage(message: WebviewCommandMessage): void;
  getState(): State | undefined;
  setState(state: State): void;
}

declare function acquireVsCodeApi<State = unknown>(): VsCodeApi<State>;

const rootElement = document.getElementById("remote-ui");

function start(element: HTMLElement): void {
  const vscode = acquireVsCodeApi<PersistedRemoteUiState>();
  let capabilities = createWebviewCapabilities();
  const store = new RemoteUiStore(capabilities);
  let showTerminalFallback = false;

  const persisted = vscode.getState();
  if (persisted?.version === 1 && persisted.remoteUi !== undefined) store.restore(persisted.remoteUi);

  const root = createRoot(element, {
    onUncaughtError(error) {
      const message = error instanceof Error ? error.message : String(error);
      element.replaceChildren(Object.assign(document.createElement("div"), { className: "ui-host-error", role: "alert", textContent: `Remote UI renderer failed: ${message}` }));
      for (const mount of store.getSnapshot().mounts) postRuntime({ type: "resync", documentId: mount.document.documentId, knownRevision: mount.document.revision });
    },
  });

  function postRuntime(message: UiRuntimeMessage): void {
    vscode.postMessage({ kind: "remoteUiRuntime", message });
  }

  function dispatch(event: UiEvent): void {
    postRuntime({ type: "event", event });
  }

  function render(): void {
    root.render(<RemoteUiRenderer store={store} capabilities={capabilities} dispatch={dispatch} showTerminalFallback={showTerminalFallback} />);
  }

  function persist(): void {
    const remoteUi = store.serialize();
    try {
      const encoded = JSON.stringify(remoteUi);
      if (encoded.length <= 4_000_000) vscode.setState({ version: 1, remoteUi });
    } catch {
      // Persistence is a recovery optimization. The authoritative daemon
      // snapshot remains the source of truth.
    }
  }

  function point(value: string | undefined): RemoteUiPlacement["point"] | undefined {
    return supportedContributionPoint(value);
  }

  window.addEventListener("message", (event: MessageEvent<TranscriptMessage>) => {
    const message = event.data;
    if (message.kind === "remoteUi") {
      const placementPoint = point(message.placement?.point);
      const placement = message.placement === undefined || placementPoint === undefined
        ? undefined
        : { ...message.placement, point: placementPoint };
      const result = store.apply(message.message, placement);
      if (result.resync !== undefined) {
        postRuntime({ type: "resync", documentId: result.resync.documentId, ...(result.resync.knownRevision === undefined ? {} : { knownRevision: result.resync.knownRevision }) });
      }
      if (result.applied) persist();
      render();
    } else if (message.kind === "remoteUiPlacement") {
      const placementPoint = point(message.placement.point);
      if (placementPoint !== undefined) {
        store.setPlacement(message.documentId, { ...message.placement, point: placementPoint });
        persist();
        render();
      }
    } else if (message.kind === "remoteUiContributions") {
      const registrations = message.registrations.flatMap((registration) => {
        const placementPoint = point(registration.placement.point);
        return placementPoint === undefined
          ? []
          : [{ documentId: registration.documentId, placement: { ...registration.placement, point: placementPoint } }];
      });
      if (registrations.length === message.registrations.length
        && store.replaceContributions(message.owner, registrations)) {
        persist();
        render();
      }
    } else if (message.kind === "remoteUiWire" && isUiWireMessage(message.message)) {
      // Projection/action mediation is deliberately separate from DOM events.
      // A trusted SDK projection adapter can subscribe without exposing its
      // data to plugin-authored node props or invoking arbitrary commands.
      publishMediatedWire(message.message);
    } else if (message.kind === "remoteUiTheme") {
      applyWireTheme(document.documentElement, message.theme);
    } else if (message.kind === "remoteUiConfigure") {
      showTerminalFallback = message.showTerminalFallback ?? showTerminalFallback;
      render();
    } else if (message.kind === "clear") {
      store.clear();
      persist();
      render();
    }
  });

  let resizeFrame: number | undefined;
  window.addEventListener("resize", () => {
    if (resizeFrame !== undefined) cancelAnimationFrame(resizeFrame);
    resizeFrame = requestAnimationFrame(() => {
      resizeFrame = undefined;
      const viewport = viewportFromWindow();
      capabilities = createWebviewCapabilities(viewport);
      store.setCapabilities(capabilities);
      postRuntime({ type: "viewport", viewport });
      render();
    });
  }, { passive: true });

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible") return;
    for (const mount of store.getSnapshot().mounts) {
      postRuntime({ type: "resync", documentId: mount.document.documentId, knownRevision: mount.document.revision });
    }
  });

  window.addEventListener(REMOTE_UI_WIRE_SEND_EVENT, (event) => {
    const message = event instanceof CustomEvent ? event.detail : undefined;
    if (isMediatedRuntimeWire(message)) vscode.postMessage({ kind: "remoteUiWire", message });
  });

  render();
  vscode.postMessage({
    kind: "remoteUiReady",
    capabilities,
    documents: store.getSnapshot().mounts.map((mount) => ({ documentId: mount.document.documentId, revision: mount.document.revision })),
  });
}

if (rootElement !== null) start(rootElement);
