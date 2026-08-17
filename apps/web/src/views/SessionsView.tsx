import React, { useState } from "react";
import {
  useSessions,
  useRepositories,
} from "@codypendent/control-plane-react";
import { Search, ChevronLeft, ChevronRight, PlaySquare, Shield, CheckCircle, XCircle, Clock } from "lucide-react";
import { PublicationBadge, StatusBadge } from "../components/Badge.js";
import { Drawer } from "../components/Drawer.js";
import { EmptyState } from "../components/EmptyState.js";
import { LoadingSpinner } from "../components/LoadingSpinner.js";

export function SessionsView(): React.JSX.Element {
  const { repositories } = useRepositories();
  const {
    sessions,
    selectedSession,
    selectedSessionId,
    setSelectedSessionId,
    isLoading,
    isDetailLoading,
    hasMore,
    nextPage,
    prevPage,
    filter,
    setFilter,
  } = useSessions();

  const [searchQuery, setSearchQuery] = useState(filter.search ?? "");

  const handleSearchSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    setFilter((prev) => ({ ...prev, search: searchQuery }));
  };

  return (
    <div data-testid="sessions-view">
      <div className="page-header">
        <div>
          <h1 className="page-title">Runs & Sessions</h1>
          <p className="page-description">
            Explore synchronized session runs, steps, and execution statuses across your repositories.
          </p>
        </div>
      </div>

      {/* Filter Bar */}
      <div className="card" style={{ padding: "12px 16px" }}>
        <form
          onSubmit={handleSearchSubmit}
          style={{ display: "flex", gap: "12px", alignItems: "center", flexWrap: "wrap" }}
        >
          {/* Repository Filter */}
          <div style={{ minWidth: "180px" }}>
            <select
              className="form-select"
              value={filter.repositoryId ?? ""}
              onChange={(e) =>
                setFilter((prev) => ({
                  ...prev,
                  repositoryId: e.target.value ? e.target.value : undefined,
                }))
              }
              data-testid="session-repo-filter"
            >
              <option value="">All Repositories</option>
              {repositories.map((r) => (
                <option key={r.id} value={r.id}>
                  {r.displayName}
                </option>
              ))}
            </select>
          </div>

          {/* State Filter */}
          <div style={{ minWidth: "140px" }}>
            <select
              className="form-select"
              value={filter.state ?? ""}
              onChange={(e) =>
                setFilter((prev) => ({
                  ...prev,
                  state: e.target.value ? e.target.value : undefined,
                }))
              }
              data-testid="session-state-filter"
            >
              <option value="">All States</option>
              <option value="running">Running</option>
              <option value="completed">Completed</option>
              <option value="failed">Failed</option>
              <option value="pending_approval">Pending Approval</option>
            </select>
          </div>

          {/* Search Input */}
          <div style={{ flex: 1, minWidth: "200px", display: "flex", gap: "6px" }}>
            <input
              className="form-input"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Search by session key or summary..."
              data-testid="session-search-input"
            />
            <button type="submit" className="btn btn-secondary" data-testid="session-search-btn">
              <Search size={15} />
            </button>
          </div>
        </form>
      </div>

      {/* Sessions Table */}
      {isLoading ? (
        <LoadingSpinner label="Loading runs and sessions..." />
      ) : sessions.length === 0 ? (
        <EmptyState
          icon={<PlaySquare size={36} />}
          title="No Sessions Found"
          description="No sessions match the current repository, state, or search filters."
        />
      ) : (
        <div className="table-container">
          <table className="data-table" data-testid="sessions-table">
            <thead>
              <tr>
                <th>Session Key</th>
                <th>Repository</th>
                <th>Publication Class</th>
                <th>Status</th>
                <th>Started</th>
                <th>Last Activity</th>
              </tr>
            </thead>
            <tbody>
              {sessions.map((session) => (
                <tr
                  key={session.id}
                  onClick={() => setSelectedSessionId(session.id)}
                  data-testid={`session-row-${session.id}`}
                >
                  <td>
                    <code>{session.remoteSessionKey}</code>
                    {session.title && (
                      <div style={{ fontSize: "12px", color: "var(--text-secondary)", marginTop: "2px" }}>
                        {session.title}
                      </div>
                    )}
                  </td>
                  <td>{session.repositoryDisplayName ?? "Repository"}</td>
                  <td>
                    <PublicationBadge publicationClass={session.class} />
                  </td>
                  <td>
                    <StatusBadge status={session.state} />
                  </td>
                  <td>{new Date(session.startedAt).toLocaleString()}</td>
                  <td>
                    {session.lastActivityAt
                      ? new Date(session.lastActivityAt).toLocaleTimeString()
                      : "—"}
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
          Showing {sessions.length} sessions
        </span>
        <div style={{ display: "flex", gap: "8px" }}>
          <button
            className="btn btn-secondary btn-sm"
            onClick={prevPage}
            data-testid="pagination-prev"
          >
            <ChevronLeft size={14} /> Previous
          </button>
          <button
            className="btn btn-secondary btn-sm"
            onClick={nextPage}
            disabled={!hasMore}
            data-testid="pagination-next"
          >
            Next <ChevronRight size={14} />
          </button>
        </div>
      </div>

      {/* Session Details Drawer */}
      <Drawer
        isOpen={selectedSessionId !== null}
        onClose={() => setSelectedSessionId(null)}
        title={selectedSession?.title ?? `Session ${selectedSession?.remoteSessionKey ?? ""}`}
        subtitle={selectedSession ? `Remote Key: ${selectedSession.remoteSessionKey}` : undefined}
      >
        {isDetailLoading ? (
          <LoadingSpinner label="Loading session details..." />
        ) : selectedSession ? (
          <div data-testid="session-detail-content">
            {/* Non-disclosure Banner */}
            <div className="privacy-banner" style={{ marginBottom: "20px" }}>
              <div style={{ display: "flex", alignItems: "center", gap: "6px", marginBottom: "4px" }}>
                <Shield size={14} color="var(--accent)" />
                <strong>Local Isolation Boundary</strong>
              </div>
              <p>
                Transcripts, raw prompt logs, and local workspace code remain exclusively on the workstation daemon.
                Only synchronized step summaries and publication metadata are stored remotely.
              </p>
            </div>

            <div style={{ display: "flex", gap: "10px", marginBottom: "20px" }}>
              <StatusBadge status={selectedSession.state} />
              <PublicationBadge publicationClass={selectedSession.class} />
            </div>

            <h3 style={{ fontSize: "14px", fontWeight: 600, marginBottom: "12px" }}>
              Run Execution Timeline ({selectedSession.steps.length} steps)
            </h3>

            {selectedSession.steps.length === 0 ? (
              <p style={{ color: "var(--text-muted)", fontSize: "13px" }}>
                No run step events recorded for this session.
              </p>
            ) : (
              <div>
                {selectedSession.steps.map((step) => (
                  <div key={step.id} className="timeline-item">
                    <div className="timeline-marker">
                      {step.status === "completed" ? (
                        <CheckCircle size={14} color="var(--success)" />
                      ) : step.status === "failed" ? (
                        <XCircle size={14} color="var(--danger)" />
                      ) : (
                        <Clock size={14} color="var(--accent)" />
                      )}
                    </div>
                    <div className="timeline-content">
                      <div
                        style={{
                          display: "flex",
                          justifyContent: "space-between",
                          alignItems: "center",
                          marginBottom: "4px",
                        }}
                      >
                        <span style={{ fontWeight: 600, fontSize: "13px" }}>{step.title}</span>
                        <span style={{ fontSize: "11px", color: "var(--text-muted)" }}>
                          {new Date(step.startedAt).toLocaleTimeString()}
                        </span>
                      </div>
                      {step.summary && (
                        <p style={{ fontSize: "12px", color: "var(--text-secondary)", marginTop: "4px" }}>
                          {step.summary}
                        </p>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        ) : null}
      </Drawer>
    </div>
  );
}
