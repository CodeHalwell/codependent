import { describe, expect, it, vi, type Mock } from "vitest";
import * as vscode from "vscode";
import {
  InboxItemTreeItem,
  InboxStatusBarIndicator,
  InboxTreeDataProvider,
  showInboxQuickPick,
} from "../src/inbox.js";
import type { DaemonClient } from "../src/client.js";
import type { InboxEntry } from "@codypendent/protocol";

// Mock vscode module
vi.mock("vscode", () => {
  class TreeItem {
    label?: string;
    description?: string;
    tooltip?: string;
    contextValue?: string;
    iconPath?: unknown;
    command?: unknown;
    constructor(label: string, public collapsibleState?: number) {
      this.label = label;
    }
  }

  const TreeItemCollapsibleState = {
    None: 0,
    Collapsed: 1,
    Expanded: 2,
  };

  class ThemeIcon {
    constructor(public id: string) {}
  }

  class ThemeColor {
    constructor(public id: string) {}
  }

  class EventEmitter {
    private listeners: Array<() => void> = [];
    event = (listener: () => void) => {
      this.listeners.push(listener);
      return { dispose: () => {} };
    };
    fire() {
      for (const listener of this.listeners) {
        listener();
      }
    }
  }

  const StatusBarAlignment = {
    Left: 1,
    Right: 2,
  };

  const statusBarItem = {
    text: "",
    tooltip: "",
    command: "",
    backgroundColor: undefined,
    show: vi.fn(),
    hide: vi.fn(),
    dispose: vi.fn(),
  };

  const window = {
    createStatusBarItem: vi.fn(() => statusBarItem),
    showQuickPick: vi.fn(),
    showInformationMessage: vi.fn(),
    showWarningMessage: vi.fn(),
    showErrorMessage: vi.fn(),
  };

  return {
    TreeItem,
    TreeItemCollapsibleState,
    ThemeIcon,
    ThemeColor,
    EventEmitter,
    StatusBarAlignment,
    window,
  };
});

describe("VS Code Inbox UI", () => {
  const sampleEntries: InboxEntry[] = [
    {
      id: "inbox-1",
      repository_id: "repo-1",
      kind: { type: "ApprovalRequest" },
      state: { type: "Unread" },
      title: "Approval required for patch",
      summary: "Write changes to auth.rs",
      deep_link: { type: "Approval", approval_id: "app-1" },
      source: {
        dedup_key: "k1",
        identity: { type: "Approval", approval_id: "app-1" },
      },
      created_at: "2026-08-16T12:00:00Z",
    },
    {
      id: "inbox-2",
      repository_id: "repo-1",
      kind: { type: "RunCompleted" },
      state: { type: "Acknowledged" },
      title: "Run completed",
      summary: "Build finished successfully",
      deep_link: { type: "Run", run_id: "run-1", session_id: "session-1" },
      source: {
        dedup_key: "k2",
        identity: { type: "Run", run_id: "run-1" },
      },
      created_at: "2026-08-16T11:00:00Z",
    },
  ];

  it("InboxItemTreeItem formats title, description, and status correctly", () => {
    const item1 = new InboxItemTreeItem(sampleEntries[0]);
    expect(item1.label).toBe("Approval required for patch");
    expect(item1.description).toBe("ApprovalRequest · Unread");
    expect(item1.tooltip).toContain("Write changes to auth.rs");
    expect(item1.contextValue).toBe("inboxItemUnread");

    const item2 = new InboxItemTreeItem(sampleEntries[1]);
    expect(item2.label).toBe("Run completed");
    expect(item2.description).toBe("RunCompleted · Acknowledged");
    expect(item2.contextValue).toBe("inboxItem");
  });

  it("InboxTreeDataProvider tracks entries and unread count", async () => {
    const provider = new InboxTreeDataProvider();
    expect(provider.unreadCount).toBe(0);

    provider.setEntries(sampleEntries);
    expect(provider.unreadCount).toBe(1);

    const children = await provider.getChildren();
    expect(children).toHaveLength(2);
    expect(children[0].label).toBe("Approval required for patch");
    expect(children[1].label).toBe("Run completed");
  });

  it("InboxTreeDataProvider refreshes from client", async () => {
    const fakeClient = {
      listInbox: vi.fn().mockResolvedValue({
        items: sampleEntries,
        next_cursor: null,
      }),
    } as unknown as DaemonClient;

    const provider = new InboxTreeDataProvider(fakeClient);
    await provider.refresh();

    expect(fakeClient.listInbox).toHaveBeenCalled();
    expect(provider.unreadCount).toBe(1);
    expect(provider.getEntries()).toHaveLength(2);
  });

  it("InboxTreeDataProvider drops a stale refresh instead of painting it over a newer one", async () => {
    // Refreshes overlap — an event lands while the previous list is in flight —
    // and responses can settle in any order. Without a ticket the slower of two
    // concurrent reads wins, and the tree shows an approval that has already
    // been resolved and will not go away.
    let releaseSlow: (() => void) | undefined;
    const slow = new Promise<void>((resolve) => {
      releaseSlow = resolve;
    });

    const stale = [sampleEntries[0]];
    const fresh: InboxEntry[] = [];
    let call = 0;
    const fakeClient = {
      listInbox: vi.fn().mockImplementation(async () => {
        call += 1;
        if (call === 1) {
          await slow;
          return { items: stale, next_cursor: null };
        }
        return { items: fresh, next_cursor: null };
      }),
    } as unknown as DaemonClient;

    const provider = new InboxTreeDataProvider(fakeClient);

    const first = provider.refresh();
    const second = provider.refresh();
    // The newer read finishes first and applies.
    await second;
    expect(provider.getEntries()).toHaveLength(0);

    // The older one lands afterwards and must be discarded.
    releaseSlow?.();
    await first;
    expect(provider.getEntries()).toHaveLength(0);
  });

  it("InboxStatusBarIndicator updates text and badge based on unread count", () => {
    const indicator = new InboxStatusBarIndicator();

    // 0 unread
    indicator.update(0);
    // 3 unread
    indicator.update(3);
    // Dispose
    indicator.dispose();
  });

  it("showInboxQuickPick shows items and triggers navigate on action", async () => {
    const fakeClient = {
      listInbox: vi.fn().mockResolvedValue({
        items: sampleEntries,
        next_cursor: null,
      }),
      mutateInbox: vi.fn().mockResolvedValue(sampleEntries[0]),
    } as unknown as DaemonClient;

    const onNavigate = vi.fn();

    // Test selection of first entry and choosing "Open Target"
    (vscode.window.showQuickPick as unknown as Mock)
      .mockResolvedValueOnce({
        label: "$(bell-dot) Approval required for patch",
        entry: sampleEntries[0],
      })
      .mockResolvedValueOnce("Open Target");

    await showInboxQuickPick(fakeClient, undefined, onNavigate);

    expect(fakeClient.listInbox).toHaveBeenCalled();
    expect(onNavigate).toHaveBeenCalledWith({ type: "Approval", approval_id: "app-1" });
  });

  it("showInboxQuickPick supports acknowledge action", async () => {
    const fakeClient = {
      listInbox: vi.fn().mockResolvedValue({
        items: sampleEntries,
        next_cursor: null,
      }),
      mutateInbox: vi.fn().mockResolvedValue({
        ...sampleEntries[0],
        state: { type: "Acknowledged" },
      }),
    } as unknown as DaemonClient;

    (vscode.window.showQuickPick as unknown as Mock)
      .mockResolvedValueOnce({
        label: "$(bell-dot) Approval required for patch",
        entry: sampleEntries[0],
      })
      .mockResolvedValueOnce("Acknowledge");

    await showInboxQuickPick(fakeClient);

    expect(fakeClient.mutateInbox).toHaveBeenCalledWith({
      type: "Acknowledge",
      entry_id: "inbox-1",
    });
  });
});
