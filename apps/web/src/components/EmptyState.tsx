import React from "react";

export interface EmptyStateProps {
  icon?: React.ReactNode;
  title: string;
  description: string;
  action?: React.ReactNode;
}

export function EmptyState({
  icon,
  title,
  description,
  action,
}: EmptyStateProps): React.JSX.Element {
  return (
    <div className="empty-state">
      {icon && <div style={{ color: "var(--text-muted)", display: "flex", justifyContent: "center" }}>{icon}</div>}
      <h3 className="empty-state-title">{title}</h3>
      <p className="empty-state-text">{description}</p>
      {action && <div>{action}</div>}
    </div>
  );
}
