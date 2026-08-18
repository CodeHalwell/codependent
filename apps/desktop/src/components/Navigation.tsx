import React from "react";
import type { ConnectionStatus, SessionSummary, SessionId } from "../types.js";

export type DesktopView =
  | "onboarding"
  | "sessions"
  | "inbox"
  | "analytics"
  | "skills"
  | "memory"
  | "docs"
  | "plugins"
  | "context"
  | "library"
  | "workflow"
  | "board"
  | "blackboard"
  | "repository"
  | "council"
  | "models"
  | "providers"
  | "keys"
  | "mode"
  | "councilResults"
  | "edges"
  | "backtrack";

/**
 * The views the sidebar offers, grouped.
 *
 * This is the ONE table of desktop surfaces. The sidebar renders it and the
 * command palette derives from it (`CommandPalette.completePaletteEntries`),
 * so a view cannot be added to one and forgotten in the other — the defect
 * that shipped four fully-built, unreachable pickers last wave.
 *
 * Each row is `[view, sidebar label, palette description]`. The description
 * is what the palette shows when the app does not supply a richer entry of
 * its own for that view.
 */
export const NAV_GROUPS = [
  {
    // First-run setup. LOCAL CONFIG like the Configuration group below — it
    // reads `models.toml`, `auth.json` and the stored repository preference,
    // never the daemon — but it sits first because it is the answer to "I just
    // installed this and nothing tells me what to do".
    group: "Setup",
    views: [
      [
        "onboarding",
        "Get Started",
        "check what first-run setup still needs: a model, a credential, a repository",
      ],
    ],
  },
  {
    group: "Run",
    views: [
      ["sessions", "Sessions", "the attached session's transcript and composer"],
      ["library", "Session Library", "search every session and rename, pin, archive, or export one"],
      ["inbox", "Inbox", "notifications and human work from the durable inbox"],
      ["analytics", "Analytics", "measured execution observations and aggregates"],
      ["context", "Context", "token usage breakdown for the active run"],
      // Reads the attached session's own ledger for its recorded checkpoints and
      // acts on them with `ForkSession` / `RestoreCheckpoint`, so it belongs here.
      ["backtrack", "Backtrack", "branch this session at a recorded checkpoint, or ask to rewind a worktree to one"],
    ],
  },
  {
    group: "Workflow",
    views: [
      ["workflow", "Workflow Runs", "open and control persisted workflow runs with a live DAG"],
      ["board", "Task Board", "create, assign, and move repository backlog tasks in Kanban columns"],
      ["blackboard", "Blackboard", "inspect attributed workflow evidence, or post an open question"],
    ],
  },
  {
    group: "Workspace",
    views: [
      ["repository", "Repository", "choose the git checkout every repository-scoped command anchors to"],
      ["docs", "Docs", "edit, review, and publish documents that already exist"],
      ["skills", "Skills", "inspect registered skills and their permissions"],
      ["memory", "Memory", "browse curated memories and their provenance"],
      ["plugins", "Plugins", "inspect, scope, approve, reject, or revoke verified UI plugins"],
      // Daemon-backed like the rest of this group: `ReadCodeGraph` /
      // `ReadCodeGraphStatus` over the socket, never a local index.
      ["edges", "Code Graph", "search the stored code graph one bounded page of nodes and edges at a time"],
    ],
  },
  {
    // Model, provider, key and mode selection are LOCAL CONFIG (models.toml,
    // auth.json) rather than daemon protocol, so they group apart from the
    // daemon-backed views above — same reasoning as Councils below.
    group: "Configuration",
    views: [
      ["models", "Models", "choose the model pinned to your next and later runs (models.toml)"],
      ["providers", "Providers", "inspect and edit the configured providers (models.toml)"],
      ["keys", "API Keys", "inspect and set provider credentials (auth.json)"],
      ["mode", "Mode", "choose the submission mode for the next run"],
    ],
  },
  {
    // Councils are local configuration, not protocol — grouped apart from the
    // daemon-backed views so the distinction is visible in the chrome itself.
    group: "Councils",
    views: [
      ["council", "Councils", "list, run, and manage persisted multi-model councils"],
      ["councilResults", "Council Results", "open durable council outcomes"],
    ],
  },
] as const satisfies ReadonlyArray<{
  readonly group: string;
  readonly views: ReadonlyArray<readonly [DesktopView, string, string]>;
}>;

/**
 * The groups the sidebar opens on first paint.
 *
 * 22 destinations listed flat is a wall, not navigation. Everything stays in
 * `NAV_GROUPS` — the palette reads the whole table and `NAV_COVERS_EVERY_VIEW`
 * still proves it covers the union — but the sidebar starts with the surfaces
 * a session actually uses and lets the rest be opened when wanted. The group
 * holding the current view is always open regardless of this set.
 */
const DEFAULT_OPEN_GROUPS: ReadonlySet<string> = new Set(["Setup", "Run"]);

/** Every view `NAV_GROUPS` actually lists. */
type NavigatedView = (typeof NAV_GROUPS)[number]["views"][number][0];

/**
 * Compile-time proof that `NAV_GROUPS` covers `DesktopView` exhaustively.
 *
 * Adding a member to the union without adding it to the table above fails
 * `tsc --noEmit` right here, which is the whole point: the sidebar and the
 * palette both read that table, so an uncovered view is unreachable by BOTH.
 */
export const NAV_COVERS_EVERY_VIEW: [DesktopView] extends [NavigatedView] ? true : never = true;

interface NavigationProps {
  sessions: SessionSummary[];
  activeSessionId: SessionId | null;
  onSelectSession: (id: SessionId) => void;
  connectionStatus: ConnectionStatus;
  /**
   * Why the client is in this connection state.
   *
   * It is NOT rendered as text here any more. The dot below carries the state
   * for a glance and this string as its accessible name and tooltip; a
   * connection that is not healthy is reported by the banner across the top of
   * the main pane (`App.tsx`), where it cannot be missed. A failure hidden in
   * a 12px footer while the app looks otherwise normal was the bad outcome.
   */
  statusDetail?: string;
  currentView?: DesktopView;
  onSelectView?: (view: DesktopView) => void;
  unreadInboxCount?: number;
  /** Opens the command palette; the palette is the full command surface. */
  onOpenPalette?: () => void;
}

/**
 * The sidebar, memoized.
 *
 * Nothing here changes while a reply streams — not the session list, not the
 * current view, not the unread count — but without this boundary all 6 groups
 * and 22 destinations reconcile once per token alongside the transcript. Every
 * callback prop must be referentially stable at the call site in `App.tsx` or
 * the memo never hits.
 */
export const Navigation: React.FC<NavigationProps> = React.memo(function Navigation({
  sessions,
  activeSessionId,
  onSelectSession,
  connectionStatus,
  statusDetail,
  currentView = "sessions",
  onSelectView,
  unreadInboxCount = 0,
  onOpenPalette,
}: NavigationProps) {
  const connected = connectionStatus === "connected";

  // Which group each view lives in, so the group holding the current view is
  // never collapsed out from under it.
  const groupOfView = React.useMemo(() => {
    const index = new Map<DesktopView, string>();
    for (const { group, views } of NAV_GROUPS) {
      for (const [view] of views) {
        index.set(view, group);
      }
    }
    return index;
  }, []);

  const [openGroups, setOpenGroups] = React.useState<ReadonlySet<string>>(
    () => new Set(DEFAULT_OPEN_GROUPS),
  );
  const toggleGroup = React.useCallback((group: string) => {
    setOpenGroups((open) => {
      const next = new Set(open);
      if (!next.delete(group)) {
        next.add(group);
      }
      return next;
    });
  }, []);
  const activeGroup = currentView ? groupOfView.get(currentView) : undefined;

  const handleSelectSession = (id: SessionId) => {
    onSelectView?.("sessions");
    onSelectSession(id);
  };

  return (
    <aside
      style={{
        width: 260,
        background: "#16191f",
        borderRight: "1px solid #282e39",
        display: "flex",
        flexDirection: "column",
        height: "100vh",
        userSelect: "none",
      }}
    >
      {/* Brand Header */}
      <div
        style={{
          padding: "16px",
          borderBottom: "1px solid #282e39",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <div
            role="img"
            aria-label={`codypendentd ${connectionStatus}`}
            title={statusDetail ?? `codypendentd ${connectionStatus}`}
            style={{
              width: 10,
              height: 10,
              borderRadius: "50%",
              background: connected ? "#2ea043" : connectionStatus === "connecting" ? "#d29922" : "#da3633",
            }}
          />
          <span style={{ fontWeight: 600, fontSize: 14, color: "#e6edf3" }}>Codypendent</span>
        </div>
        {onOpenPalette && (
          <button
            onClick={onOpenPalette}
            aria-label="Open command palette"
            title="Command palette (⌘K / Ctrl-K)"
            style={{
              background: "#21262d",
              border: "1px solid #30363d",
              borderRadius: 6,
              color: "#8b949e",
              fontSize: 11,
              padding: "3px 8px",
              cursor: "pointer",
            }}
          >
            ⌘K
          </button>
        )}
      </div>

      {/* Main Views Nav */}
      <div
        style={{
          padding: "8px 8px 8px",
          borderBottom: "1px solid #282e39",
          display: "flex",
          flexDirection: "column",
          gap: 2,
        }}
      >
        {NAV_GROUPS.map(({ group, views }) => {
          // The group holding the current view is always open: collapsing the
          // ground you are standing on is how a sidebar loses you.
          const open = openGroups.has(group) || group === activeGroup;
          return (
          <React.Fragment key={group}>
            <button
              type="button"
              onClick={() => toggleGroup(group)}
              aria-expanded={open}
              aria-label={`${group} group`}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 6,
                width: "100%",
                background: "transparent",
                border: "none",
                fontSize: 10,
                letterSpacing: 0.6,
                textTransform: "uppercase",
                color: "#6e7681",
                padding: "8px 10px 2px",
                cursor: "pointer",
                textAlign: "left",
              }}
            >
              <span
                aria-hidden="true"
                style={{
                  display: "inline-block",
                  transition: "transform 120ms ease",
                  transform: open ? "rotate(90deg)" : "none",
                }}
              >
                ›
              </span>
              {group}
            </button>
            {open &&
            views.map(([view, label]) => {
              const active = currentView === view;
              return (
                <button
                  key={view}
                  onClick={() => onSelectView?.(view)}
                  aria-label={`${label} View`}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "space-between",
                    padding: "7px 10px",
                    borderRadius: 6,
                    background: active ? "#1f242c" : "transparent",
                    border: active ? "1px solid #388bfd" : "1px solid transparent",
                    color: "#e6edf3",
                    fontSize: 13,
                    fontWeight: active ? 600 : 400,
                    cursor: "pointer",
                    textAlign: "left",
                    width: "100%",
                  }}
                >
                  <span>{label}</span>
                  {view === "inbox" && unreadInboxCount > 0 && (
                    <span
                      data-testid="inbox-badge"
                      style={{
                        padding: "1px 6px",
                        borderRadius: 10,
                        fontSize: 11,
                        fontWeight: 600,
                        background: "#1f6feb",
                        color: "#ffffff",
                      }}
                    >
                      {unreadInboxCount}
                    </span>
                  )}
                </button>
              );
            })}
          </React.Fragment>
          );
        })}
      </div>

      {/* Sessions List */}
      <div style={{ flex: 1, overflowY: "auto", padding: "8px" }}>
        <div style={{ fontSize: 11, color: "#8b949e", padding: "6px 8px", textTransform: "uppercase" }}>
          Recent Sessions
        </div>
        {sessions.length === 0 ? (
          <div style={{ padding: "12px 8px", color: "#6e7681", fontSize: 13 }}>
            {connected ? "No sessions yet" : "No sessions (not connected)"}
          </div>
        ) : (
          sessions.map((session) => {
            const active = session.id === activeSessionId && currentView === "sessions";
            return (
              // A `<button>`, not a clickable `<div>`: the TUI is entirely
              // keyboard-driven, and a session you can only reach with a mouse
              // has no equivalent there at all.
              <button
                key={session.id}
                onClick={() => handleSelectSession(session.id)}
                aria-label={`Session ${session.title || "Untitled Session"}`}
                aria-current={active ? "true" : undefined}
                style={{
                  display: "block",
                  width: "100%",
                  textAlign: "left",
                  font: "inherit",
                  padding: "8px 10px",
                  borderRadius: 6,
                  marginBottom: 4,
                  cursor: "pointer",
                  background: active ? "#1f242c" : "transparent",
                  border: active ? "1px solid #388bfd" : "1px solid transparent",
                }}
              >
                <div style={{ fontSize: 13, fontWeight: active ? 600 : 400, color: "#e6edf3", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                  {session.title || "Untitled Session"}
                </div>
                <div style={{ fontSize: 11, color: "#8b949e", marginTop: 2 }}>
                  {session.state} · {session.updated_at}
                </div>
              </button>
            );
          })
        )}
      </div>
    </aside>
  );
});
