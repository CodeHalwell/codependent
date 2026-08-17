import React, { useState } from "react";
import { useApprovals, useInbox } from "@codypendent/control-plane-react";
import { Check, X, CheckSquare, Bell, CheckCircle2 } from "lucide-react";
import { EmptyState } from "../components/EmptyState.js";
import { LoadingSpinner } from "../components/LoadingSpinner.js";
import { Modal } from "../components/Modal.js";

export function ApprovalsView(): React.JSX.Element {
  const [activeTab, setActiveTab] = useState<"approvals" | "inbox">("approvals");

  const {
    pendingApprovals,
    isLoading: approvalsLoading,
    isSubmitting,
    decide,
  } = useApprovals();

  const {
    items: inboxItems,
    isLoading: inboxLoading,
    markAsRead,
    markAsDismissed,
  } = useInbox();

  const [decisionModalState, setDecisionModalState] = useState<{
    approvalId: string;
    actionDigest: string;
    decision: "approve" | "reject";
    description: string;
  } | null>(null);

  const [decisionReason, setDecisionReason] = useState("");

  const handleConfirmDecision = async () => {
    if (!decisionModalState) return;
    await decide(
      decisionModalState.approvalId,
      decisionModalState.decision,
      decisionModalState.actionDigest,
      decisionReason || undefined
    );
    setDecisionModalState(null);
    setDecisionReason("");
  };

  return (
    <div data-testid="approvals-view">
      <div className="page-header">
        <div>
          <h1 className="page-title">Approvals & Inbox</h1>
          <p className="page-description">
            Review security-sensitive approval requests and system notifications across the organization.
          </p>
        </div>
      </div>

      {/* Tabs */}
      <div style={{ display: "flex", gap: "8px", marginBottom: "16px" }}>
        <button
          className={`btn ${activeTab === "approvals" ? "btn-primary" : "btn-secondary"}`}
          onClick={() => setActiveTab("approvals")}
          data-testid="tab-approvals"
        >
          <CheckSquare size={15} /> Pending Approvals ({pendingApprovals.length})
        </button>
        <button
          className={`btn ${activeTab === "inbox" ? "btn-primary" : "btn-secondary"}`}
          onClick={() => setActiveTab("inbox")}
          data-testid="tab-inbox"
        >
          <Bell size={15} /> Inbox Notifications ({inboxItems.length})
        </button>
      </div>

      {activeTab === "approvals" ? (
        approvalsLoading ? (
          <LoadingSpinner label="Loading pending approvals..." />
        ) : pendingApprovals.length === 0 ? (
          <EmptyState
            icon={<CheckCircle2 size={36} color="var(--success)" />}
            title="All Clear"
            description="No pending actions requiring human approval at this time."
          />
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: "12px" }}>
            {pendingApprovals.map((approval) => (
              <div key={approval.id} className="card" data-testid={`approval-card-${approval.id}`}>
                <div className="card-header">
                  <div>
                    <h3 style={{ fontSize: "15px", fontWeight: 600 }}>{approval.description}</h3>
                    <p style={{ fontSize: "12px", color: "var(--text-secondary)", marginTop: "2px" }}>
                      Target: <code>{approval.targetKind}:{approval.targetId}</code> • Action: <code>{approval.action}</code>
                    </p>
                  </div>
                  <div style={{ display: "flex", gap: "8px" }}>
                    <button
                      className="btn btn-secondary btn-sm"
                      onClick={() =>
                        setDecisionModalState({
                          approvalId: approval.id,
                          actionDigest: approval.actionDigest,
                          decision: "reject",
                          description: approval.description,
                        })
                      }
                      disabled={isSubmitting}
                      data-testid={`reject-btn-${approval.id}`}
                    >
                      <X size={14} /> Reject
                    </button>
                    <button
                      className="btn btn-primary btn-sm"
                      onClick={() =>
                        setDecisionModalState({
                          approvalId: approval.id,
                          actionDigest: approval.actionDigest,
                          decision: "approve",
                          description: approval.description,
                        })
                      }
                      disabled={isSubmitting}
                      data-testid={`approve-btn-${approval.id}`}
                    >
                      <Check size={14} /> Approve
                    </button>
                  </div>
                </div>
                <div className="code-block" style={{ fontSize: "11px", marginTop: "8px" }}>
                  Action Digest (SHA-256): {approval.actionDigest}
                </div>
              </div>
            ))}
          </div>
        )
      ) : inboxLoading ? (
        <LoadingSpinner label="Loading inbox..." />
      ) : inboxItems.length === 0 ? (
        <EmptyState
          icon={<Bell size={36} />}
          title="Inbox Empty"
          description="You have no notifications or messages."
        />
      ) : (
        <div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
          {inboxItems.map((item) => (
            <div
              key={item.id}
              className="card"
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                opacity: item.state === "read" ? 0.75 : 1,
              }}
              data-testid={`inbox-card-${item.id}`}
            >
              <div>
                <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
                  {item.state === "unread" && (
                    <span
                      style={{
                        width: "8px",
                        height: "8px",
                        borderRadius: "50%",
                        backgroundColor: "var(--accent)",
                        display: "inline-block",
                      }}
                    />
                  )}
                  <h4 style={{ fontSize: "14px", fontWeight: 600 }}>{item.title}</h4>
                </div>
                <p style={{ fontSize: "13px", color: "var(--text-secondary)", marginTop: "4px" }}>
                  {item.body}
                </p>
                <span style={{ fontSize: "11px", color: "var(--text-muted)", marginTop: "4px", display: "inline-block" }}>
                  {new Date(item.createdAt).toLocaleString()}
                </span>
              </div>
              <div style={{ display: "flex", gap: "6px" }}>
                {item.state === "unread" && (
                  <button
                    className="btn btn-secondary btn-sm"
                    onClick={() => markAsRead(item.id)}
                    data-testid={`mark-read-btn-${item.id}`}
                  >
                    Mark Read
                  </button>
                )}
                <button
                  className="btn btn-secondary btn-sm"
                  onClick={() => markAsDismissed(item.id)}
                  data-testid={`dismiss-btn-${item.id}`}
                >
                  Dismiss
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Approval Confirmation Modal */}
      <Modal
        isOpen={decisionModalState !== null}
        onClose={() => setDecisionModalState(null)}
        title={decisionModalState?.decision === "approve" ? "Confirm Approval" : "Confirm Rejection"}
        footer={
          <>
            <button className="btn btn-secondary" onClick={() => setDecisionModalState(null)}>
              Cancel
            </button>
            <button
              className={`btn ${decisionModalState?.decision === "approve" ? "btn-primary" : "btn-danger"}`}
              onClick={handleConfirmDecision}
              data-testid="confirm-decision-btn"
            >
              Confirm {decisionModalState?.decision === "approve" ? "Approval" : "Rejection"}
            </button>
          </>
        }
      >
        <div>
          <p style={{ fontSize: "13px", color: "var(--text-secondary)", marginBottom: "12px" }}>
            {decisionModalState?.description}
          </p>
          <div className="form-group">
            <label className="form-label">Decision Reason / Notes (Optional)</label>
            <input
              className="form-input"
              value={decisionReason}
              onChange={(e) => setDecisionReason(e.target.value)}
              placeholder="e.g. Verified diff and test run passed"
              data-testid="decision-reason-input"
            />
          </div>
        </div>
      </Modal>
    </div>
  );
}
