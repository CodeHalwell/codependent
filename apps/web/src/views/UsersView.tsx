import React, { useState } from "react";
import { useUsers } from "@codypendent/control-plane-react";
import { UserPlus, Shield, Trash2, Users as UsersIcon } from "lucide-react";
import { RoleBadge, StatusBadge } from "../components/Badge.js";
import { EmptyState } from "../components/EmptyState.js";
import { LoadingSpinner } from "../components/LoadingSpinner.js";
import { Modal } from "../components/Modal.js";
import type { ControlPlaneRole } from "@codypendent/control-plane";

export function UsersView(): React.JSX.Element {
  const {
    members,
    roleGrants,
    isLoading,
    isMutating,
    inviteUser,
    grantRole,
    removeUser,
  } = useUsers();

  const [showInviteModal, setShowInviteModal] = useState(false);
  const [inviteEmail, setInviteEmail] = useState("");
  const [inviteRole, setInviteRole] = useState<ControlPlaneRole>("contributor");

  const [showGrantModal, setShowGrantModal] = useState(false);
  const [selectedUserId, setSelectedUserId] = useState<string>("");
  const [newRole, setNewRole] = useState<ControlPlaneRole>("contributor");

  const handleInvite = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!inviteEmail) return;
    await inviteUser(inviteEmail, inviteRole);
    setShowInviteModal(false);
    setInviteEmail("");
    setInviteRole("contributor");
  };

  const handleGrantRole = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!selectedUserId) return;
    await grantRole({
      userId: selectedUserId,
      role: newRole,
    });
    setShowGrantModal(false);
    setSelectedUserId("");
  };

  const getMemberRole = (userId: string): ControlPlaneRole => {
    const grant = roleGrants.find((g) => g.userId === userId && !g.revokedAt);
    return grant?.role ?? "observer";
  };

  return (
    <div data-testid="users-view">
      <div className="page-header">
        <div>
          <h1 className="page-title">Members & Access Control</h1>
          <p className="page-description">
            Manage organization members, role-based access control (RBAC), and team permissions.
          </p>
        </div>
        <button
          className="btn btn-primary"
          onClick={() => setShowInviteModal(true)}
          data-testid="invite-member-btn"
        >
          <UserPlus size={16} />
          <span>Invite Member</span>
        </button>
      </div>

      {isLoading ? (
        <LoadingSpinner label="Loading organization members..." />
      ) : members.length === 0 ? (
        <EmptyState
          icon={<UsersIcon size={36} />}
          title="No Members"
          description="Invite team members to collaborate in this organization."
          action={
            <button className="btn btn-primary" onClick={() => setShowInviteModal(true)}>
              Invite Member
            </button>
          }
        />
      ) : (
        <div className="table-container">
          <table className="data-table" data-testid="members-table">
            <thead>
              <tr>
                <th>Member</th>
                <th>Email</th>
                <th>Role</th>
                <th>Status</th>
                <th>Joined</th>
                <th style={{ textAlign: "right" }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {members.map((member) => (
                <tr key={member.id} data-testid={`member-row-${member.id}`}>
                  <td>
                    <strong>{member.displayName}</strong>
                  </td>
                  <td>{member.primaryEmail ?? "—"}</td>
                  <td>
                    <RoleBadge role={getMemberRole(member.id)} />
                  </td>
                  <td>
                    <StatusBadge status={member.state} />
                  </td>
                  <td>{new Date(member.createdAt).toLocaleDateString()}</td>
                  <td style={{ textAlign: "right" }}>
                    <div style={{ display: "inline-flex", gap: "6px" }}>
                      <button
                        className="btn btn-secondary btn-sm"
                        onClick={() => {
                          setSelectedUserId(member.id);
                          setNewRole(getMemberRole(member.id));
                          setShowGrantModal(true);
                        }}
                        data-testid={`change-role-btn-${member.id}`}
                      >
                        <Shield size={13} /> Change Role
                      </button>
                      <button
                        className="btn btn-secondary btn-sm"
                        style={{ color: "var(--danger)" }}
                        onClick={() => removeUser(member.id)}
                        disabled={isMutating}
                        data-testid={`remove-member-btn-${member.id}`}
                      >
                        <Trash2 size={13} />
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* Invite Modal */}
      <Modal
        isOpen={showInviteModal}
        onClose={() => setShowInviteModal(false)}
        title="Invite New Member"
        footer={
          <>
            <button className="btn btn-secondary" onClick={() => setShowInviteModal(false)}>
              Cancel
            </button>
            <button className="btn btn-primary" onClick={handleInvite} data-testid="send-invite-btn">
              Send Invite
            </button>
          </>
        }
      >
        <form onSubmit={handleInvite}>
          <div className="form-group">
            <label className="form-label">Email Address</label>
            <input
              type="email"
              className="form-input"
              value={inviteEmail}
              onChange={(e) => setInviteEmail(e.target.value)}
              placeholder="colleague@example.com"
              required
              data-testid="invite-email-input"
            />
          </div>
          <div className="form-group">
            <label className="form-label">Role</label>
            <select
              className="form-select"
              value={inviteRole}
              onChange={(e) => setInviteRole(e.target.value as ControlPlaneRole)}
              data-testid="invite-role-select"
            >
              <option value="observer">Observer (Read-only)</option>
              <option value="contributor">Contributor (Execute runs & tasks)</option>
              <option value="approver">Approver (Approve sensitive actions)</option>
              <option value="maintainer">Maintainer (Manage repos & settings)</option>
              <option value="organization-admin">Organization Admin (Full authority)</option>
            </select>
          </div>
        </form>
      </Modal>

      {/* Change Role Modal */}
      <Modal
        isOpen={showGrantModal}
        onClose={() => setShowGrantModal(false)}
        title="Update Member Role"
        footer={
          <>
            <button className="btn btn-secondary" onClick={() => setShowGrantModal(false)}>
              Cancel
            </button>
            <button className="btn btn-primary" onClick={handleGrantRole} data-testid="save-role-btn">
              Save Role
            </button>
          </>
        }
      >
        <form onSubmit={handleGrantRole}>
          <div className="form-group">
            <label className="form-label">Select Role</label>
            <select
              className="form-select"
              value={newRole}
              onChange={(e) => setNewRole(e.target.value as ControlPlaneRole)}
              data-testid="change-role-select"
            >
              <option value="observer">Observer (Read-only)</option>
              <option value="contributor">Contributor (Execute runs & tasks)</option>
              <option value="approver">Approver (Approve sensitive actions)</option>
              <option value="maintainer">Maintainer (Manage repos & settings)</option>
              <option value="organization-admin">Organization Admin (Full authority)</option>
            </select>
          </div>
        </form>
      </Modal>
    </div>
  );
}
