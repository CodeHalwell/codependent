import React, { useState } from "react";
import type { InboxDeepLink, InboxEntry, InboxEntryKind } from "@codypendent/protocol";

export interface InboxViewProps {
  entries: InboxEntry[];
  onAcknowledge: (entryId: string) => void;
  onDismiss: (entryId: string) => void;
  onNavigate: (deepLink: InboxDeepLink) => void;
  onApprove?: (approvalId: string) => void;
  onReject?: (approvalId: string) => void;
  onRefresh?: () => void;
  /**
   * Why the inbox could not be read, when it could not be read.
   *
   * An empty `entries` array is ambiguous — it is what "nothing pending" and
   * "never answered" both look like. When this is set the view says the inbox
   * is unavailable instead of drawing the empty state, which would assert that
   * no human work is waiting when we simply do not know.
   */
  unavailable?: string | null;
}

type StateFilter = "All" | "Unread" | "Acknowledged" | "Dismissed" | "Resolved";

export const InboxView: React.FC<InboxViewProps> = ({
  entries,
  onAcknowledge,
  onDismiss,
  onNavigate,
  onApprove,
  onReject,
  onRefresh,
  unavailable,
}) => {
  const [stateFilter, setStateFilter] = useState<StateFilter>("Unread");
  const [kindFilter, setKindFilter] = useState<string>("All");

  const filteredEntries = entries.filter((entry) => {
    const entryState = entry.state?.type ?? "Unread";
    if (stateFilter !== "All" && entryState !== stateFilter) {
      return false;
    }
    const entryKind = entry.kind?.type ?? "Unknown";
    if (kindFilter !== "All" && entryKind !== kindFilter) {
      return false;
    }
    return true;
  });

  const getKindBadgeColor = (kind: InboxEntryKind["type"]): { bg: string; text: string } => {
    switch (kind) {
      case "ApprovalRequest":
        return { bg: "#3d2600", text: "#f0883e" };
      case "AgentQuestion":
        return { bg: "#271033", text: "#d2a8ff" };
      case "RunCompleted":
        return { bg: "#122619", text: "#3fb950" };
      case "RunFailed":
      case "RunnerFailed":
        return { bg: "#3c181a", text: "#ff7b72" };
      case "BudgetWarning":
      case "WorkflowBlocked":
        return { bg: "#34220b", text: "#d29922" };
      case "PluginPermissionChanged":
        return { bg: "#16233b", text: "#58a6ff" };
      default:
        return { bg: "#21262d", text: "#8b949e" };
    }
  };

  const describeDeepLink = (deepLink: InboxDeepLink): string => {
    switch (deepLink.type) {
      case "Approval":
        return `Approval ${deepLink.approval_id}`;
      case "Question":
        return `Question ${deepLink.question_id}`;
      case "Session":
        return `Session ${deepLink.session_id}`;
      case "Run":
        return `Run ${deepLink.run_id}`;
      case "Workflow":
        return `Workflow ${deepLink.workflow_id}`;
      case "Plugin":
        return `Plugin ${deepLink.plugin_id}`;
      case "Repository":
        return `Repository ${deepLink.repository_id}`;
      default:
        return "Target";
    }
  };

  return (
    <div
      role="region"
      aria-label="Inbox"
      style={{
        flex: 1,
        display: "flex",
        flexDirection: "column",
        height: "100vh",
        background: "#0d1117",
        color: "#e6edf3",
        overflowY: "auto",
      }}
    >
      {/* Header */}
      <div
        style={{
          padding: "20px 24px 16px",
          borderBottom: "1px solid #21262d",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          flexWrap: "wrap",
          gap: 12,
        }}
      >
        <div>
          <h1 style={{ margin: 0, fontSize: 20, fontWeight: 600 }}>Durable Inbox</h1>
          <p style={{ margin: "4px 0 0", fontSize: 13, color: "#8b949e" }}>
            Pending human work, approval requests, and notifications
          </p>
        </div>
        {onRefresh && (
          <button
            onClick={onRefresh}
            style={{
              padding: "6px 12px",
              background: "#21262d",
              border: "1px solid #30363d",
              borderRadius: 6,
              color: "#e6edf3",
              cursor: "pointer",
              fontSize: 13,
            }}
          >
            Refresh
          </button>
        )}
      </div>

      {/* Filter Toolbar */}
      <div
        style={{
          padding: "12px 24px",
          borderBottom: "1px solid #21262d",
          display: "flex",
          alignItems: "center",
          gap: 16,
          flexWrap: "wrap",
          background: "#161b22",
        }}
      >
        {/* State filters */}
        <div style={{ display: "flex", gap: 6 }}>
          {(["All", "Unread", "Acknowledged", "Dismissed", "Resolved"] as StateFilter[]).map((state) => (
            <button
              key={state}
              onClick={() => setStateFilter(state)}
              style={{
                padding: "4px 10px",
                borderRadius: 6,
                fontSize: 12,
                fontWeight: stateFilter === state ? 600 : 400,
                background: stateFilter === state ? "#1f6feb" : "#21262d",
                color: stateFilter === state ? "#ffffff" : "#c9d1d9",
                border: "1px solid",
                borderColor: stateFilter === state ? "#388bfd" : "#30363d",
                cursor: "pointer",
              }}
            >
              {state}
            </button>
          ))}
        </div>

        {/* Kind filter */}
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginLeft: "auto" }}>
          <label htmlFor="kind-filter" style={{ fontSize: 12, color: "#8b949e" }}>
            Type:
          </label>
          <select
            id="kind-filter"
            value={kindFilter}
            onChange={(e) => setKindFilter(e.target.value)}
            style={{
              padding: "4px 8px",
              background: "#21262d",
              border: "1px solid #30363d",
              borderRadius: 6,
              color: "#e6edf3",
              fontSize: 12,
            }}
          >
            <option value="All">All Types</option>
            <option value="ApprovalRequest">Approval Request</option>
            <option value="AgentQuestion">Agent Question</option>
            <option value="RunCompleted">Run Completed</option>
            <option value="RunFailed">Run Failed</option>
            <option value="BudgetWarning">Budget Warning</option>
            <option value="WorkflowBlocked">Workflow Blocked</option>
            <option value="PluginPermissionChanged">Plugin Permission</option>
            <option value="RunnerFailed">Runner Failed</option>
          </select>
        </div>
      </div>

      {/* Entry List */}
      <div style={{ flex: 1, padding: "16px 24px" }}>
        {unavailable ? (
          /* Unread, not empty — we cannot claim there is no pending work. */
          <div
            data-testid="inbox-unavailable"
            role="status"
            style={{
              padding: "48px 24px",
              textAlign: "center",
              color: "#d29922",
              fontSize: 14,
            }}
          >
            Inbox unavailable — {unavailable}
          </div>
        ) : filteredEntries.length === 0 ? (
          <div
            data-testid="inbox-empty"
            style={{
              padding: "48px 24px",
              textAlign: "center",
              color: "#8b949e",
              fontSize: 14,
            }}
          >
            No notifications in inbox for filter: {stateFilter}
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            {filteredEntries.map((entry) => {
              const kindType = entry.kind?.type ?? "Unknown";
              const stateType = entry.state?.type ?? "Unread";
              const kindColors = getKindBadgeColor(kindType);
              const isApproval = kindType === "ApprovalRequest";
              const approvalId =
                entry.deep_link.type === "Approval"
                  ? entry.deep_link.approval_id
                  : entry.source?.identity?.type === "Approval"
                    ? entry.source.identity.approval_id
                    : undefined;

              return (
                <div
                  key={entry.id}
                  data-testid={`inbox-entry-${entry.id}`}
                  style={{
                    padding: 16,
                    background: "#161b22",
                    border: "1px solid #30363d",
                    borderRadius: 8,
                    display: "flex",
                    flexDirection: "column",
                    gap: 10,
                  }}
                >
                  <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 12 }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                      <span
                        style={{
                          padding: "2px 8px",
                          borderRadius: 12,
                          fontSize: 11,
                          fontWeight: 600,
                          background: kindColors.bg,
                          color: kindColors.text,
                        }}
                      >
                        {kindType}
                      </span>
                      <span
                        style={{
                          padding: "2px 6px",
                          borderRadius: 4,
                          fontSize: 11,
                          background: "#21262d",
                          color: "#8b949e",
                        }}
                      >
                        {stateType}
                      </span>
                      {entry.repository_id && (
                        <span style={{ fontSize: 11, color: "#8b949e" }}>repo: {entry.repository_id}</span>
                      )}
                    </div>
                    <span style={{ fontSize: 11, color: "#8b949e", whiteSpace: "nowrap" }}>
                      {entry.created_at}
                    </span>
                  </div>

                  <div>
                    <h3 style={{ margin: "0 0 4px", fontSize: 15, fontWeight: 600, color: "#e6edf3" }}>
                      {entry.title}
                    </h3>
                    {entry.summary && (
                      <p style={{ margin: 0, fontSize: 13, color: "#8b949e", lineHeight: 1.4 }}>
                        {entry.summary}
                      </p>
                    )}
                  </div>

                  {/* Quick Actions */}
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 8,
                      marginTop: 4,
                      paddingTop: 8,
                      borderTop: "1px solid #21262d",
                      flexWrap: "wrap",
                    }}
                  >
                    {/* Deep link navigate */}
                    <button
                      onClick={() => onNavigate(entry.deep_link)}
                      style={{
                        padding: "4px 10px",
                        background: "#238636",
                        border: "1px solid #2ea043",
                        borderRadius: 6,
                        color: "#ffffff",
                        fontSize: 12,
                        fontWeight: 500,
                        cursor: "pointer",
                      }}
                      aria-label={`Open ${entry.title}`}
                    >
                      Open {describeDeepLink(entry.deep_link)}
                    </button>

                    {/* Direct Approval Actions if available */}
                    {isApproval && approvalId && stateType !== "Resolved" && (
                      <>
                        {onApprove && (
                          <button
                            onClick={() => onApprove(approvalId)}
                            style={{
                              padding: "4px 10px",
                              background: "#1f6feb",
                              border: "1px solid #388bfd",
                              borderRadius: 6,
                              color: "#ffffff",
                              fontSize: 12,
                              fontWeight: 500,
                              cursor: "pointer",
                            }}
                            aria-label={`Approve ${entry.title}`}
                          >
                            Approve
                          </button>
                        )}
                        {onReject && (
                          <button
                            onClick={() => onReject(approvalId)}
                            style={{
                              padding: "4px 10px",
                              background: "#da3633",
                              border: "1px solid #f85149",
                              borderRadius: 6,
                              color: "#ffffff",
                              fontSize: 12,
                              fontWeight: 500,
                              cursor: "pointer",
                            }}
                            aria-label={`Reject ${entry.title}`}
                          >
                            Reject
                          </button>
                        )}
                      </>
                    )}

                    {/* Acknowledge */}
                    {stateType === "Unread" && (
                      <button
                        onClick={() => onAcknowledge(entry.id)}
                        style={{
                          padding: "4px 10px",
                          background: "#21262d",
                          border: "1px solid #30363d",
                          borderRadius: 6,
                          color: "#c9d1d9",
                          fontSize: 12,
                          cursor: "pointer",
                        }}
                        aria-label={`Acknowledge ${entry.title}`}
                      >
                        Acknowledge
                      </button>
                    )}

                    {/* Dismiss */}
                    {stateType !== "Dismissed" && stateType !== "Resolved" && (
                      <button
                        onClick={() => onDismiss(entry.id)}
                        style={{
                          padding: "4px 10px",
                          background: "#21262d",
                          border: "1px solid #30363d",
                          borderRadius: 6,
                          color: "#8b949e",
                          fontSize: 12,
                          cursor: "pointer",
                        }}
                        aria-label={`Dismiss ${entry.title}`}
                      >
                        Dismiss
                      </button>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
};
