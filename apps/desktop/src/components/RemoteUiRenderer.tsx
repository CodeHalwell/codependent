import React, { useEffect, useMemo, useRef } from "react";
import type { UiDocument, UiEvent } from "@codypendent/ui";
import {
  RemoteUiRenderer as SharedRemoteUiRenderer,
  RemoteUiStore,
  createHostReactCapabilities,
  type RemoteUiRecoveryRequest,
} from "@codypendent/ui/host-react";

export interface RemoteUiRendererProps {
  documents: Map<string, UiDocument>;
  onEvent?: (event: UiEvent) => void;
  onRecover?: (request: RemoteUiRecoveryRequest) => void;
  showTerminalFallback?: boolean;
}

export const RemoteUiRenderer: React.FC<RemoteUiRendererProps> = ({
  documents,
  onEvent,
  onRecover,
  showTerminalFallback = false,
}) => {
  const capabilities = useMemo(() => createHostReactCapabilities(undefined, "desktop"), []);
  const store = useMemo(() => new RemoteUiStore(capabilities), [capabilities]);
  const prevDocIdsRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    const currentDocIds = new Set(documents.keys());
    for (const id of prevDocIdsRef.current) {
      if (!currentDocIds.has(id)) {
        store.apply({ type: "dispose", documentId: id, revision: 1 });
      }
    }
    for (const [, doc] of documents.entries()) {
      const placement = {
        point: "panel" as const,
        extensionId: (typeof doc.metadata?.source === "string" && doc.metadata.source.length > 0)
          ? doc.metadata.source
          : (typeof doc.metadata?.contributionId === "string" && doc.metadata.contributionId.length > 0)
            ? doc.metadata.contributionId
            : "desktop.extension",
      };
      store.apply({ type: "snapshot", document: doc }, placement);
    }
    prevDocIdsRef.current = currentDocIds;
  }, [documents, store]);

  if (documents.size === 0) return null;

  const dispatch = onEvent ?? (() => {
    // Default desktop event sink
  });

  return (
    <aside
      className="desktop-remote-ui-sidebar"
      style={{
        width: 360,
        background: "#16191f",
        borderLeft: "1px solid #282e39",
        display: "flex",
        flexDirection: "column",
        height: "100vh",
        overflowY: "auto",
        padding: 16,
      }}
      aria-label="Extension surfaces"
    >
      <header style={{ fontSize: 12, fontWeight: 600, color: "#8b949e", textTransform: "uppercase", marginBottom: 12 }}>
        Live Extensions ({documents.size})
      </header>
      <SharedRemoteUiRenderer
        store={store}
        capabilities={capabilities}
        dispatch={dispatch}
        recover={onRecover}
        showTerminalFallback={showTerminalFallback}
      />
    </aside>
  );
};
