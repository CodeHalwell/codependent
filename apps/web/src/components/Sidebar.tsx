import React from "react";
import {
  LayoutDashboard,
  PlaySquare,
  CheckSquare,
  ShieldAlert,
  Users,
  Key,
  Settings,
} from "lucide-react";
import { useApprovals } from "@codypendent/control-plane-react";

export type NavView =
  | "overview"
  | "sessions"
  | "approvals"
  | "audit"
  | "users"
  | "apikeys"
  | "settings";

export interface SidebarProps {
  currentView: NavView;
  onNavigate: (view: NavView) => void;
}

export function Sidebar({ currentView, onNavigate }: SidebarProps): React.JSX.Element {
  const { pendingApprovals } = useApprovals({ subscribeLive: true });

  const navItems: { id: NavView; label: string; icon: React.ReactNode; badge?: number }[] = [
    {
      id: "overview",
      label: "Overview",
      icon: <LayoutDashboard size={16} />,
    },
    {
      id: "sessions",
      label: "Runs & Sessions",
      icon: <PlaySquare size={16} />,
    },
    {
      id: "approvals",
      label: "Approvals & Inbox",
      icon: <CheckSquare size={16} />,
      badge: pendingApprovals.length > 0 ? pendingApprovals.length : undefined,
    },
    {
      id: "audit",
      label: "Audit Logs",
      icon: <ShieldAlert size={16} />,
    },
    {
      id: "users",
      label: "Members & Access",
      icon: <Users size={16} />,
    },
    {
      id: "apikeys",
      label: "API Keys & Daemons",
      icon: <Key size={16} />,
    },
    {
      id: "settings",
      label: "Organization Policy",
      icon: <Settings size={16} />,
    },
  ];

  return (
    <aside className="app-sidebar" aria-label="Main Navigation">
      <nav className="sidebar-nav">
        {navItems.map((item) => (
          <button
            key={item.id}
            className={`nav-link ${currentView === item.id ? "active" : ""}`}
            onClick={() => onNavigate(item.id)}
            data-testid={`nav-${item.id}`}
            aria-current={currentView === item.id ? "page" : undefined}
          >
            {item.icon}
            <span>{item.label}</span>
            {item.badge !== undefined && (
              <span className="nav-badge" data-testid={`nav-badge-${item.id}`}>
                {item.badge}
              </span>
            )}
          </button>
        ))}
      </nav>
    </aside>
  );
}
