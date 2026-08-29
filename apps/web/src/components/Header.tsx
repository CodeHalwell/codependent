import React, { useState } from "react";
import { Bell, LogOut, User as UserIcon } from "lucide-react";
import { useAuth, useInbox } from "@codypendent/control-plane-react";
import { Switcher } from "./Switcher.js";

export interface HeaderProps {
  onNavigateToInbox: () => void;
}

export function Header({ onNavigateToInbox }: HeaderProps): React.JSX.Element {
  const { currentUser, isAuthenticated, logout } = useAuth();
  const { unreadCount } = useInbox({ subscribeLive: true });
  const [showUserMenu, setShowUserMenu] = useState(false);
  const accountLabel = currentUser?.displayName ?? "Authenticated session";
  const accountDetail =
    currentUser?.primaryEmail ?? currentUser?.displayName ?? "Bearer credential configured";

  return (
    <header className="app-header">
      <div className="header-left">
        <a href="#overview" className="header-brand">
          <div className="brand-icon">C</div>
          <span>Codypendent Control Plane</span>
        </a>
        <Switcher />
      </div>

      <div className="header-right">
        <button
          className="btn btn-secondary btn-sm"
          style={{ position: "relative" }}
          onClick={onNavigateToInbox}
          aria-label={`Notifications, ${unreadCount} unread`}
          data-testid="inbox-bell-button"
        >
          <Bell size={16} />
          {unreadCount > 0 && (
            <span
              style={{
                position: "absolute",
                top: "-4px",
                right: "-4px",
                backgroundColor: "var(--danger)",
                color: "white",
                borderRadius: "var(--radius-full)",
                fontSize: "10px",
                padding: "1px 5px",
                fontWeight: "bold",
              }}
              data-testid="unread-badge-count"
            >
              {unreadCount}
            </span>
          )}
        </button>

        {isAuthenticated ? (
          <div style={{ position: "relative" }}>
            <button
              className="switcher-button"
              onClick={() => setShowUserMenu(!showUserMenu)}
              aria-label="User account menu"
              data-testid="user-menu-button"
            >
              <UserIcon size={14} />
              <span>{accountLabel}</span>
            </button>

            {showUserMenu && (
              <div
                className="switcher-dropdown"
                style={{ right: 0, left: "auto" }}
                data-testid="user-dropdown-menu"
              >
                <div className="dropdown-header">{accountDetail}</div>
                <button
                  className="dropdown-item"
                  onClick={() => {
                    setShowUserMenu(false);
                    logout();
                  }}
                  data-testid="logout-button"
                >
                  <span style={{ display: "flex", alignItems: "center", gap: "6px", color: "var(--danger)" }}>
                    <LogOut size={14} /> Log out
                  </span>
                </button>
              </div>
            )}
          </div>
        ) : (
          <a href="#login" className="btn btn-primary btn-sm">
            Sign In
          </a>
        )}
      </div>
    </header>
  );
}
