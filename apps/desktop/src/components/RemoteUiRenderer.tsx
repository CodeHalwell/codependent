import React from "react";
import type { UiDocument } from "@codypendent/ui";

interface RemoteUiRendererProps {
  documents: Map<string, UiDocument>;
}

export const RemoteUiRenderer: React.FC<RemoteUiRendererProps> = ({ documents }) => {
  if (documents.size === 0) return null;

  return (
    <div
      style={{
        width: 320,
        background: "#16191f",
        borderLeft: "1px solid #282e39",
        display: "flex",
        flexDirection: "column",
        height: "100vh",
        overflowY: "auto",
        padding: 16,
      }}
    >
      <div style={{ fontSize: 12, fontWeight: 600, color: "#8b949e", textTransform: "uppercase", marginBottom: 12 }}>
        Live Components ({documents.size})
      </div>
      {Array.from(documents.entries()).map(([id, doc]) => (
        <div
          key={id}
          style={{
            background: "#0d1117",
            border: "1px solid #30363d",
            borderRadius: 8,
            padding: 12,
            marginBottom: 12,
          }}
        >
          <div style={{ fontSize: 13, fontWeight: 600, color: "#58a6ff", marginBottom: 6 }}>
            {doc.metadata?.title || doc.documentId}
          </div>
          <div style={{ fontSize: 11, color: "#8b949e", marginBottom: 8 }}>
            Revision: {doc.revision} · Protocol: {String(doc.protocolVersion)}
          </div>
          <div style={{ fontSize: 12, color: "#c9d1d9", background: "#161b22", padding: 8, borderRadius: 4 }}>
            {doc.root?.id ? `Root: ${doc.root.id} (${doc.root.kind})` : "Empty Document"}
          </div>
        </div>
      ))}
    </div>
  );
};
