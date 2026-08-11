import { createElement, type ReactNode } from "react";
import type { UiHostMessage, UiWireMessage } from "../protocol.js";
import { UiProvider } from "../react/provider.js";
import { createReactUiRoot } from "../react/renderer.js";
import type { MediatedUiBridge } from "./bridge.js";
import type { UiSurfaceFactory } from "./runtime.js";

export interface ReactUiSurfaceOptions {
  documentId: string;
  idPrefix?: string;
  strictMode?: boolean;
  render(bridge: MediatedUiBridge): ReactNode;
  onError?: (cause: unknown) => void;
}

function wireFromRenderer(message: UiHostMessage, messageId: string): UiWireMessage | undefined {
  switch (message.type) {
    case "snapshot": return { type: "snapshot", messageId, snapshot: { document: message.document } };
    case "patch": return { type: "patchBatch", messageId, patchBatch: message.batch };
    case "dispose": return undefined;
    default: return undefined;
  }
}

export function createReactUiSurface(options: ReactUiSurfaceOptions): UiSurfaceFactory {
  return {
    documentId: options.documentId,
    mount(context) {
      let sequence = 0;
      let disposing = false;
      let root: ReturnType<typeof createReactUiRoot>;
      const render = (): void => root.render(createElement(
        UiProvider,
        { state: context.bridge, actions: context.bridge, meta: context.bridge.meta, children: options.render(context.bridge) },
      ));
      root = createReactUiRoot({
        documentId: options.documentId,
        ...(options.idPrefix === undefined ? {} : { idPrefix: options.idPrefix }),
        ...(options.strictMode === undefined ? {} : { strictMode: options.strictMode }),
        limits: {
          maxDepth: context.selection.limits.maxTreeDepth,
          maxNodes: context.selection.limits.maxNodes,
          maxTextBytes: context.selection.limits.maxTextBytes,
          maxPropertiesPerNode: context.selection.limits.maxPropertiesPerNode,
          maxActionsPerNode: context.selection.limits.maxActionsPerNode,
          maxJsonDepth: context.selection.limits.maxJsonDepth,
          maxJsonValues: context.selection.limits.maxJsonValues,
          maxPatchCount: context.selection.limits.maxPatchesPerBatch,
          maxDocumentBytes: context.selection.limits.maxPatchBytes * 2,
        },
        onMessage(message) {
          if (disposing) return;
          sequence += 1;
          const wire = wireFromRenderer(message, `${options.documentId}-${sequence}`);
          if (wire !== undefined) void context.send(wire).catch(context.fail);
        },
        onError(cause) { options.onError?.(cause); context.fail(cause); },
      });
      render();
      return {
        documentId: options.documentId,
        getDocument: root.getDocument,
        dispatch: root.dispatch,
        render,
        dispose: () => { disposing = true; root.unmount(); },
      };
    },
  };
}
