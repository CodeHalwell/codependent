import React, { useState } from "react";
import { useAuditLogs } from "@codypendent/control-plane-react";
import { ShieldCheck, ShieldAlert, CheckCircle, ChevronLeft, ChevronRight, FileText } from "lucide-react";
import { Drawer } from "../components/Drawer.js";
import { EmptyState } from "../components/EmptyState.js";
import { LoadingSpinner } from "../components/LoadingSpinner.js";
import type { AuditActorKind, AuditRecord } from "@codypendent/control-plane";

export function AuditLogsView(): React.JSX.Element {
  const {
    records,
    isLoading,
    hasMore,
    nextPage,
    filter,
    setFilter,
    verifyChain,
    verificationResult,
    isVerifying,
  } = useAuditLogs();

  const [selectedRecord, setSelectedRecord] = useState<AuditRecord | null>(null);

  return (
    <div data-testid="audit-view">
      <div className="page-header">
        <div>
          <h1 className="page-title">Immutable Audit Trail</h1>
          <p className="page-description">
            Append-only, cryptographically hash-chained log of all control plane mutations and administrative actions.
          </p>
        </div>
        <button
          className="btn btn-secondary"
          onClick={() => verifyChain()}
          disabled={isVerifying}
          data-testid="verify-audit-chain-btn"
        >
          {isVerifying ? (
            <LoadingSpinner size={14} label="" />
          ) : (
            <ShieldCheck size={16} color="var(--success)" />
          )}
          <span>Verify Hash Chain</span>
        </button>
      </div>

      {/* Verification Status Banner */}
      {verificationResult && (
        <div
          style={{
            backgroundColor: verificationResult.valid
              ? "var(--success-surface)"
              : "var(--danger-surface)",
            border: `1px solid ${verificationResult.valid ? "var(--success)" : "var(--danger)"}`,
            borderRadius: "var(--radius-md)",
            padding: "12px 16px",
            marginBottom: "16px",
            display: "flex",
            alignItems: "center",
            gap: "10px",
          }}
          data-testid="audit-verification-banner"
        >
          {verificationResult.valid ? (
            <CheckCircle size={18} color="var(--success)" />
          ) : (
            <ShieldAlert size={18} color="var(--danger)" />
          )}
          <span style={{ fontSize: "13px", fontWeight: 500 }}>
            {verificationResult.message} ({verificationResult.totalRecordsChecked} records verified)
          </span>
        </div>
      )}

      {/* Filter Bar */}
      <div className="card" style={{ padding: "12px 16px" }}>
        <div style={{ display: "flex", gap: "12px", alignItems: "center", flexWrap: "wrap" }}>
          {/* Actor Kind Filter */}
          <div style={{ minWidth: "150px" }}>
            <select
              className="form-select"
              value={filter.actorKind ?? ""}
              onChange={(e) =>
                setFilter((prev) => ({
                  ...prev,
                  actorKind: e.target.value ? (e.target.value as AuditActorKind) : undefined,
                }))
              }
              data-testid="audit-actor-filter"
            >
              <option value="">All Actors</option>
              <option value="user">User</option>
              <option value="daemon">Daemon</option>
              <option value="system">System</option>
            </select>
          </div>

          {/* Action Filter */}
          <div style={{ flex: 1, minWidth: "200px" }}>
            <input
              className="form-input"
              value={filter.action ?? ""}
              onChange={(e) =>
                setFilter((prev) => ({
                  ...prev,
                  action: e.target.value ? e.target.value : undefined,
                }))
              }
              placeholder="Filter by action (e.g. role.grant, repo.register)..."
              data-testid="audit-action-filter"
            />
          </div>
        </div>
      </div>

      {/* Audit Records Table */}
      {isLoading ? (
        <LoadingSpinner label="Loading audit trail..." />
      ) : records.length === 0 ? (
        <EmptyState
          icon={<FileText size={36} />}
          title="No Audit Records"
          description="No audit records matched the selected filters."
        />
      ) : (
        <div className="table-container">
          <table className="data-table" data-testid="audit-table">
            <thead>
              <tr>
                <th>Timestamp</th>
                <th>Actor</th>
                <th>Action</th>
                <th>Target</th>
                <th>Action Digest</th>
                <th>Hash Chain</th>
              </tr>
            </thead>
            <tbody>
              {records.map((record) => (
                <tr
                  key={record.id}
                  onClick={() => setSelectedRecord(record)}
                  data-testid={`audit-row-${record.id}`}
                >
                  <td>{new Date(record.occurredAt).toLocaleString()}</td>
                  <td>
                    <span className="badge badge-metadata" style={{ textTransform: "capitalize" }}>
                      {record.actorKind}
                    </span>
                  </td>
                  <td>
                    <code>{record.action}</code>
                  </td>
                  <td>
                    <code>
                      {record.targetKind}:{record.targetId}
                    </code>
                  </td>
                  <td>
                    <code style={{ fontSize: "11px" }}>
                      {record.actionDigest.slice(0, 12)}...
                    </code>
                  </td>
                  <td>
                    <span className="badge badge-completed">Chained</span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* Pagination Footer */}
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          marginTop: "16px",
        }}
      >
        <span style={{ fontSize: "13px", color: "var(--text-secondary)" }}>
          Showing {records.length} records
        </span>
        <div style={{ display: "flex", gap: "8px" }}>
          <button className="btn btn-secondary btn-sm" disabled>
            <ChevronLeft size={14} /> Previous
          </button>
          <button
            className="btn btn-secondary btn-sm"
            onClick={nextPage}
            disabled={!hasMore}
            data-testid="audit-pagination-next"
          >
            Next <ChevronRight size={14} />
          </button>
        </div>
      </div>

      {/* Audit Detail Drawer */}
      <Drawer
        isOpen={selectedRecord !== null}
        onClose={() => setSelectedRecord(null)}
        title={`Audit Event: ${selectedRecord?.action ?? ""}`}
        subtitle={selectedRecord ? `Record ID: ${selectedRecord.id}` : undefined}
      >
        {selectedRecord && (
          <div data-testid="audit-detail-content">
            <div className="form-group">
              <label className="form-label">Occurred At</label>
              <div style={{ fontSize: "13px" }}>{new Date(selectedRecord.occurredAt).toISOString()}</div>
            </div>

            <div className="form-group">
              <label className="form-label">Actor</label>
              <div style={{ fontSize: "13px" }}>
                <code>{selectedRecord.actorKind}</code> {selectedRecord.actorId ? `(${selectedRecord.actorId})` : ""}
              </div>
            </div>

            <div className="form-group">
              <label className="form-label">Target</label>
              <div style={{ fontSize: "13px" }}>
                <code>{selectedRecord.targetKind}</code> : <code>{selectedRecord.targetId}</code>
              </div>
            </div>

            <div className="form-group">
              <label className="form-label">Action Digest (SHA-256)</label>
              <div className="code-block">{selectedRecord.actionDigest}</div>
            </div>

            <div className="form-group">
              <label className="form-label">Record Hash</label>
              <div className="code-block">{selectedRecord.recordHash}</div>
            </div>

            <div className="form-group">
              <label className="form-label">Previous Record Hash</label>
              <div className="code-block">{selectedRecord.prevHash ?? "(Genesis Record)"}</div>
            </div>

            <div className="form-group">
              <label className="form-label">Metadata Detail (Sanitized / No Secrets)</label>
              <pre className="code-block">{JSON.stringify(selectedRecord.detail, null, 2)}</pre>
            </div>
          </div>
        )}
      </Drawer>
    </div>
  );
}
