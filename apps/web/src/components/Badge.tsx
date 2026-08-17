import React from "react";
import type { PublicationClass, SharedSessionState, ControlPlaneRole } from "@codypendent/control-plane";

export interface PublicationBadgeProps {
  publicationClass: PublicationClass;
}

export function PublicationBadge({ publicationClass }: PublicationBadgeProps): React.JSX.Element {
  let badgeClass = "badge-private";
  let label = "Private Local";

  switch (publicationClass) {
    case "metadata-shared":
      badgeClass = "badge-metadata";
      label = "Metadata Shared";
      break;
    case "content-shared":
      badgeClass = "badge-content";
      label = "Content Shared";
      break;
    case "organization-knowledge":
      badgeClass = "badge-org-knowledge";
      label = "Org Knowledge";
      break;
    case "public-marketplace":
      badgeClass = "badge-content";
      label = "Public Marketplace";
      break;
    case "private-local":
      badgeClass = "badge-private";
      label = "Private Local";
      break;
    default:
      badgeClass = "badge-private";
      label = publicationClass;
  }

  return (
    <span className={`badge ${badgeClass}`} title={`Publication ceiling: ${label}`}>
      {label}
    </span>
  );
}

export interface StatusBadgeProps {
  status: SharedSessionState | string;
}

export function StatusBadge({ status }: StatusBadgeProps): React.JSX.Element {
  let badgeClass = "badge-private";

  switch (status) {
    case "running":
      badgeClass = "badge-running";
      break;
    case "completed":
    case "active":
      badgeClass = "badge-completed";
      break;
    case "failed":
    case "revoked":
      badgeClass = "badge-failed";
      break;
    case "pending_approval":
    case "pending":
    case "invited":
      badgeClass = "badge-pending";
      break;
  }

  return <span className={`badge ${badgeClass}`}>{status.replace("_", " ")}</span>;
}

export interface RoleBadgeProps {
  role: ControlPlaneRole | string;
}

export function RoleBadge({ role }: RoleBadgeProps): React.JSX.Element {
  let badgeClass = "badge-metadata";
  if (role === "organization-admin" || role === "maintainer") {
    badgeClass = "badge-org-knowledge";
  } else if (role === "approver") {
    badgeClass = "badge-content";
  }

  return <span className={`badge ${badgeClass}`}>{role}</span>;
}
