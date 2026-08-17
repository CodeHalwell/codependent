import React from "react";
import {
  useOrganizations,
  useSessions,
  useApprovals,
  useDaemons,
  useAuditLogs,
} from "@codypendent/control-plane-react";
import { PlaySquare, CheckSquare, ShieldCheck, Laptop, Lock } from "lucide-react";
import { PublicationBadge, StatusBadge } from "../components/Badge.js";
import { LoadingSpinner } from "../components/LoadingSpinner.js";

export interface OverviewViewProps {
  onNavigateToSessions: () => void;
  onNavigateToApprovals: () => void;
  onNavigateToAudit: () => void;
  onNavigateToDaemons: () => void;
}

export function OverviewView({
  onNavigateToSessions,
  onNavigateToApprovals,
  onNavigateToAudit,
  onNavigateToDaemons,
}: OverviewViewProps): React.JSX.Element {
  const { activeOrganization } = useOrganizations();
  const { sessions, isLoading: sessionsLoading } = useSessions({ limit: 5 });
  const { pendingApprovals } = useApprovals();
  const { daemons } = useDaemons();
  const { records } = useAuditLogs({ initialQuery: { limit: 5 } });

  const activeRunsCount = sessions.filter((s) => s.state === "running").length;

  if (!activeOrganization) {
    return (
      <div>
        <div className="page-header">
          <div>
            <h1 className="page-title">Overview</h1>
            <p className="page-description">Select or create an organization to get started</p>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div data-testid="overview-view">
      <div className="page-header">
        <div>
          <div style={{ display: "flex", alignItems: "center", gap: "10px" }}>
            <h1 className="page-title">{activeOrganization.displayName}</h1>
            <PublicationBadge publicationClass={activeOrganization.maxPublicationClass} />
          </div>
          <p className="page-description">
            Organization Slug: <code>{activeOrganization.slug}</code> • Policy Version: {activeOrganization.policyVersion}
          </p>
        </div>
      </div>

      {/* Non-disclosure / Authority boundary notice */}
      <div className="privacy-banner" role="region" aria-label="Privacy & Authority Isolation">
        <div style={{ display: "flex", alignItems: "center", gap: "8px", marginBottom: "4px" }}>
          <Lock size={15} color="var(--accent)" />
          <strong>Local Authority & Non-Disclosure Boundary:</strong>
        </div>
        <p>
          The local daemon on your workstation remains strictly authoritative for source code, private history,
          and unpublished transcripts. The control plane only stores shared metadata bounded by the{" "}
          <code>{activeOrganization.maxPublicationClass}</code> ceiling.
        </p>
      </div>

      {/* Stats Cards */}
      <div className="grid-stats">
        <div className="stat-card" onClick={onNavigateToSessions} style={{ cursor: "pointer" }}>
          <div className="stat-label" style={{ display: "flex", alignItems: "center", gap: "6px" }}>
            <PlaySquare size={14} color="var(--accent)" /> Active Runs
          </div>
          <div className="stat-value">{activeRunsCount}</div>
          <div className="stat-subtext">{sessions.length} total shared sessions</div>
        </div>

        <div className="stat-card" onClick={onNavigateToApprovals} style={{ cursor: "pointer" }}>
          <div className="stat-label" style={{ display: "flex", alignItems: "center", gap: "6px" }}>
            <CheckSquare size={14} color="var(--warning)" /> Pending Approvals
          </div>
          <div className="stat-value">{pendingApprovals.length}</div>
          <div className="stat-subtext">Awaiting human review</div>
        </div>

        <div className="stat-card" onClick={onNavigateToDaemons} style={{ cursor: "pointer" }}>
          <div className="stat-label" style={{ display: "flex", alignItems: "center", gap: "6px" }}>
            <Laptop size={14} color="var(--primary)" /> Paired Daemons
          </div>
          <div className="stat-value">{daemons.filter((d) => d.state === "active").length}</div>
          <div className="stat-subtext">{daemons.length} registered workstations</div>
        </div>

        <div className="stat-card" onClick={onNavigateToAudit} style={{ cursor: "pointer" }}>
          <div className="stat-label" style={{ display: "flex", alignItems: "center", gap: "6px" }}>
            <ShieldCheck size={14} color="var(--success)" /> Audit Trail
          </div>
          <div className="stat-value">{records.length}</div>
          <div className="stat-subtext">Cryptographically verifiable</div>
        </div>
      </div>

      {/* Recent Sessions */}
      <div className="card">
        <div className="card-header">
          <h2 className="card-title">Recent Shared Sessions & Runs</h2>
          <button className="btn btn-secondary btn-sm" onClick={onNavigateToSessions}>
            View All
          </button>
        </div>

        {sessionsLoading ? (
          <LoadingSpinner label="Loading sessions..." />
        ) : sessions.length === 0 ? (
          <p style={{ color: "var(--text-muted)", fontSize: "13px", padding: "12px 0" }}>
            No sessions synchronized yet. Pair a local daemon to publish session summaries.
          </p>
        ) : (
          <div className="table-container">
            <table className="data-table">
              <thead>
                <tr>
                  <th>Session Key</th>
                  <th>Repository</th>
                  <th>Publication Class</th>
                  <th>State</th>
                  <th>Started</th>
                </tr>
              </thead>
              <tbody>
                {sessions.slice(0, 5).map((session) => (
                  <tr key={session.id} onClick={onNavigateToSessions}>
                    <td>
                      <code>{session.remoteSessionKey}</code>
                    </td>
                    <td>{session.repositoryDisplayName ?? "Repository"}</td>
                    <td>
                      <PublicationBadge publicationClass={session.class} />
                    </td>
                    <td>
                      <StatusBadge status={session.state} />
                    </td>
                    <td>{new Date(session.startedAt).toLocaleString()}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
