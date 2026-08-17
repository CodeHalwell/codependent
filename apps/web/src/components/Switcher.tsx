import React, { useState, useRef, useEffect } from "react";
import { ChevronDown, Building2, Users, FolderGit2, Plus } from "lucide-react";
import {
  useOrganizations,
  useWorkspaces,
  useRepositories,
} from "@codypendent/control-plane-react";
import { PublicationBadge } from "./Badge.js";
import { Modal } from "./Modal.js";

export function Switcher(): React.JSX.Element {
  const {
    organizations,
    activeOrganization,
    setActiveOrganizationId,
    createOrganization,
  } = useOrganizations();

  const { teams, activeTeam, setActiveTeamId, createTeam } = useWorkspaces();

  const {
    repositories,
    activeRepository,
    setActiveRepositoryId,
    registerRepository,
  } = useRepositories();

  const [openDropdown, setOpenDropdown] = useState<"org" | "team" | "repo" | null>(null);

  const [showNewOrgModal, setShowNewOrgModal] = useState(false);
  const [newOrgSlug, setNewOrgSlug] = useState("");
  const [newOrgName, setNewOrgName] = useState("");

  const [showNewTeamModal, setShowNewTeamModal] = useState(false);
  const [newTeamSlug, setNewTeamSlug] = useState("");
  const [newTeamName, setNewTeamName] = useState("");

  const [showNewRepoModal, setShowNewRepoModal] = useState(false);
  const [newRepoFedId, setNewRepoFedId] = useState("");
  const [newRepoName, setNewRepoName] = useState("");

  const switcherRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (switcherRef.current && !switcherRef.current.contains(event.target as Node)) {
        setOpenDropdown(null);
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  const handleCreateOrg = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newOrgSlug || !newOrgName) return;
    await createOrganization({ slug: newOrgSlug, displayName: newOrgName });
    setShowNewOrgModal(false);
    setNewOrgSlug("");
    setNewOrgName("");
  };

  const handleCreateTeam = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newTeamSlug || !newTeamName) return;
    await createTeam({ slug: newTeamSlug, displayName: newTeamName });
    setShowNewTeamModal(false);
    setNewTeamSlug("");
    setNewTeamName("");
  };

  const handleCreateRepo = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newRepoFedId || !newRepoName) return;
    await registerRepository({ federatedId: newRepoFedId, displayName: newRepoName });
    setShowNewRepoModal(false);
    setNewRepoFedId("");
    setNewRepoName("");
  };

  return (
    <div className="switchers-group" ref={switcherRef}>
      {/* Organization Switcher */}
      <div className="switcher-item">
        <button
          className="switcher-button"
          onClick={() => setOpenDropdown(openDropdown === "org" ? null : "org")}
          aria-haspopup="listbox"
          aria-expanded={openDropdown === "org"}
          data-testid="org-switcher-button"
        >
          <Building2 size={15} color="var(--text-secondary)" />
          <span>{activeOrganization?.displayName ?? "Select Organization"}</span>
          {activeOrganization && (
            <PublicationBadge publicationClass={activeOrganization.maxPublicationClass} />
          )}
          <ChevronDown size={14} color="var(--text-muted)" />
        </button>

        {openDropdown === "org" && (
          <div className="switcher-dropdown" role="listbox" data-testid="org-dropdown-menu">
            <div className="dropdown-header">Organizations</div>
            {organizations.map((org) => (
              <button
                key={org.id}
                role="option"
                aria-selected={org.id === activeOrganization?.id}
                className={`dropdown-item ${org.id === activeOrganization?.id ? "active" : ""}`}
                onClick={() => {
                  setActiveOrganizationId(org.id);
                  setOpenDropdown(null);
                }}
                data-testid={`org-option-${org.slug}`}
              >
                <span>{org.displayName}</span>
                <PublicationBadge publicationClass={org.maxPublicationClass} />
              </button>
            ))}
            <div style={{ borderTop: "1px solid var(--border-subtle)", margin: "4px 0" }} />
            <button
              className="dropdown-item"
              onClick={() => {
                setOpenDropdown(null);
                setShowNewOrgModal(true);
              }}
              data-testid="add-org-btn"
            >
              <span style={{ display: "flex", alignItems: "center", gap: "6px" }}>
                <Plus size={14} /> New Organization
              </span>
            </button>
          </div>
        )}
      </div>

      {/* Workspace / Team Switcher */}
      {activeOrganization && (
        <div className="switcher-item">
          <button
            className="switcher-button"
            onClick={() => setOpenDropdown(openDropdown === "team" ? null : "team")}
            aria-haspopup="listbox"
            aria-expanded={openDropdown === "team"}
            data-testid="team-switcher-button"
          >
            <Users size={15} color="var(--text-secondary)" />
            <span>{activeTeam?.displayName ?? "All Teams"}</span>
            <ChevronDown size={14} color="var(--text-muted)" />
          </button>

          {openDropdown === "team" && (
            <div className="switcher-dropdown" role="listbox" data-testid="team-dropdown-menu">
              <div className="dropdown-header">Teams / Workspaces</div>
              <button
                role="option"
                aria-selected={activeTeam === null}
                className={`dropdown-item ${activeTeam === null ? "active" : ""}`}
                onClick={() => {
                  setActiveTeamId(null);
                  setOpenDropdown(null);
                }}
              >
                <span>All Teams</span>
              </button>
              {teams.map((t) => (
                <button
                  key={t.id}
                  role="option"
                  aria-selected={t.id === activeTeam?.id}
                  className={`dropdown-item ${t.id === activeTeam?.id ? "active" : ""}`}
                  onClick={() => {
                    setActiveTeamId(t.id);
                    setOpenDropdown(null);
                  }}
                  data-testid={`team-option-${t.slug}`}
                >
                  <span>{t.displayName}</span>
                </button>
              ))}
              <div style={{ borderTop: "1px solid var(--border-subtle)", margin: "4px 0" }} />
              <button
                className="dropdown-item"
                onClick={() => {
                  setOpenDropdown(null);
                  setShowNewTeamModal(true);
                }}
                data-testid="add-team-btn"
              >
                <span style={{ display: "flex", alignItems: "center", gap: "6px" }}>
                  <Plus size={14} /> New Team
                </span>
              </button>
            </div>
          )}
        </div>
      )}

      {/* Repository Switcher */}
      {activeOrganization && (
        <div className="switcher-item">
          <button
            className="switcher-button"
            onClick={() => setOpenDropdown(openDropdown === "repo" ? null : "repo")}
            aria-haspopup="listbox"
            aria-expanded={openDropdown === "repo"}
            data-testid="repo-switcher-button"
          >
            <FolderGit2 size={15} color="var(--text-secondary)" />
            <span>{activeRepository?.displayName ?? "All Repositories"}</span>
            {activeRepository && (
              <PublicationBadge publicationClass={activeRepository.maxPublicationClass} />
            )}
            <ChevronDown size={14} color="var(--text-muted)" />
          </button>

          {openDropdown === "repo" && (
            <div className="switcher-dropdown" role="listbox" data-testid="repo-dropdown-menu">
              <div className="dropdown-header">Repositories</div>
              <button
                role="option"
                aria-selected={activeRepository === null}
                className={`dropdown-item ${activeRepository === null ? "active" : ""}`}
                onClick={() => {
                  setActiveRepositoryId(null);
                  setOpenDropdown(null);
                }}
              >
                <span>All Repositories</span>
              </button>
              {repositories.map((repo) => (
                <button
                  key={repo.id}
                  role="option"
                  aria-selected={repo.id === activeRepository?.id}
                  className={`dropdown-item ${repo.id === activeRepository?.id ? "active" : ""}`}
                  onClick={() => {
                    setActiveRepositoryId(repo.id);
                    setOpenDropdown(null);
                  }}
                  data-testid={`repo-option-${repo.id}`}
                  title={`Federated ID: ${repo.federatedId}`}
                >
                  <span style={{ overflow: "hidden", textOverflow: "ellipsis", marginRight: "8px" }}>
                    {repo.displayName}
                  </span>
                  <PublicationBadge publicationClass={repo.maxPublicationClass} />
                </button>
              ))}
              <div style={{ borderTop: "1px solid var(--border-subtle)", margin: "4px 0" }} />
              <button
                className="dropdown-item"
                onClick={() => {
                  setOpenDropdown(null);
                  setShowNewRepoModal(true);
                }}
                data-testid="add-repo-btn"
              >
                <span style={{ display: "flex", alignItems: "center", gap: "6px" }}>
                  <Plus size={14} /> Register Repository
                </span>
              </button>
            </div>
          )}
        </div>
      )}

      {/* New Org Modal */}
      <Modal
        isOpen={showNewOrgModal}
        onClose={() => setShowNewOrgModal(false)}
        title="Create New Organization"
        footer={
          <>
            <button className="btn btn-secondary" onClick={() => setShowNewOrgModal(false)}>
              Cancel
            </button>
            <button className="btn btn-primary" onClick={handleCreateOrg}>
              Create Organization
            </button>
          </>
        }
      >
        <form onSubmit={handleCreateOrg}>
          <div className="form-group">
            <label className="form-label">Organization Name</label>
            <input
              className="form-input"
              value={newOrgName}
              onChange={(e) => setNewOrgName(e.target.value)}
              placeholder="e.g. Acme Corp"
              required
            />
          </div>
          <div className="form-group">
            <label className="form-label">Slug (Identifier)</label>
            <input
              className="form-input"
              value={newOrgSlug}
              onChange={(e) => setNewOrgSlug(e.target.value.toLowerCase())}
              placeholder="e.g. acme-corp"
              required
            />
          </div>
        </form>
      </Modal>

      {/* New Team Modal */}
      <Modal
        isOpen={showNewTeamModal}
        onClose={() => setShowNewTeamModal(false)}
        title="Create New Team"
        footer={
          <>
            <button className="btn btn-secondary" onClick={() => setShowNewTeamModal(false)}>
              Cancel
            </button>
            <button className="btn btn-primary" onClick={handleCreateTeam}>
              Create Team
            </button>
          </>
        }
      >
        <form onSubmit={handleCreateTeam}>
          <div className="form-group">
            <label className="form-label">Team Name</label>
            <input
              className="form-input"
              value={newTeamName}
              onChange={(e) => setNewTeamName(e.target.value)}
              placeholder="e.g. Core Engineering"
              required
            />
          </div>
          <div className="form-group">
            <label className="form-label">Slug</label>
            <input
              className="form-input"
              value={newTeamSlug}
              onChange={(e) => setNewTeamSlug(e.target.value.toLowerCase())}
              placeholder="e.g. core-eng"
              required
            />
          </div>
        </form>
      </Modal>

      {/* Register Repository Modal */}
      <Modal
        isOpen={showNewRepoModal}
        onClose={() => setShowNewRepoModal(false)}
        title="Register Repository"
        footer={
          <>
            <button className="btn btn-secondary" onClick={() => setShowNewRepoModal(false)}>
              Cancel
            </button>
            <button className="btn btn-primary" onClick={handleCreateRepo}>
              Register Repository
            </button>
          </>
        }
      >
        <form onSubmit={handleCreateRepo}>
          <div className="form-group">
            <label className="form-label">Repository Display Name</label>
            <input
              className="form-input"
              value={newRepoName}
              onChange={(e) => setNewRepoName(e.target.value)}
              placeholder="e.g. web-app"
              required
            />
          </div>
          <div className="form-group">
            <label className="form-label">Federated ID (SHA-256 Hex)</label>
            <input
              className="form-input"
              value={newRepoFedId}
              onChange={(e) => setNewRepoFedId(e.target.value)}
              placeholder="64-character hex string from local daemon"
              maxLength={64}
              required
            />
          </div>
        </form>
      </Modal>
    </div>
  );
}
