/**
 * VS Code TreeDataProvider and Status Bar Indicator for the durable inbox.
 */
import * as vscode from "vscode";
import type { DaemonClient } from "./client.js";
import type { InboxDeepLink, InboxEntry, InboxEntryKind } from "@codypendent/protocol";

export class InboxItemTreeItem extends vscode.TreeItem {
  constructor(public readonly entry: InboxEntry) {
    const kind = entry.kind?.type ?? "Unknown";
    const state = entry.state?.type ?? "Unread";
    super(entry.title, vscode.TreeItemCollapsibleState.None);

    this.description = `${kind} · ${state}`;
    this.tooltip = entry.summary ? `${entry.title}\n${entry.summary}` : entry.title;
    this.contextValue = state === "Unread" ? "inboxItemUnread" : "inboxItem";

    // Set appropriate Codicon
    this.iconPath = getKindCodicon(kind);

    this.command = {
      command: "codypendent.openInboxEntry",
      title: "Open Inbox Target",
      arguments: [entry],
    };
  }
}

function getKindCodicon(kind: InboxEntryKind["type"]): vscode.ThemeIcon {
  switch (kind) {
    case "ApprovalRequest":
      return new vscode.ThemeIcon("shield");
    case "AgentQuestion":
      return new vscode.ThemeIcon("question");
    case "RunCompleted":
      return new vscode.ThemeIcon("pass");
    case "RunFailed":
    case "RunnerFailed":
      return new vscode.ThemeIcon("error");
    case "BudgetWarning":
    case "WorkflowBlocked":
      return new vscode.ThemeIcon("warning");
    case "PluginPermissionChanged":
      return new vscode.ThemeIcon("extensions");
    default:
      return new vscode.ThemeIcon("bell");
  }
}

export class InboxTreeDataProvider implements vscode.TreeDataProvider<InboxItemTreeItem> {
  private _onDidChangeTreeData: vscode.EventEmitter<InboxItemTreeItem | undefined | null | void> =
    new vscode.EventEmitter<InboxItemTreeItem | undefined | null | void>();
  readonly onDidChangeTreeData: vscode.Event<InboxItemTreeItem | undefined | null | void> =
    this._onDidChangeTreeData.event;

  private entries: InboxEntry[] = [];
  private client: DaemonClient | undefined;

  constructor(client?: DaemonClient) {
    this.client = client;
  }

  setClient(client: DaemonClient | undefined): void {
    this.client = client;
    void this.refresh();
  }

  setEntries(entries: InboxEntry[]): void {
    this.entries = entries;
    this._onDidChangeTreeData.fire();
  }

  getEntries(): InboxEntry[] {
    return this.entries;
  }

  get unreadCount(): number {
    return this.entries.filter((e) => !e.state || e.state.type === "Unread").length;
  }

  async refresh(): Promise<void> {
    if (!this.client) {
      this.entries = [];
      this._onDidChangeTreeData.fire();
      return;
    }
    try {
      const page = await this.client.listInbox();
      this.entries = page.items;
    } catch {
      // Keep existing entries or clear if unavailable
    }
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: InboxItemTreeItem): vscode.TreeItem {
    return element;
  }

  getChildren(): Thenable<InboxItemTreeItem[]> {
    return Promise.resolve(this.entries.map((entry) => new InboxItemTreeItem(entry)));
  }
}

export class InboxStatusBarIndicator {
  private statusBarItem: vscode.StatusBarItem;

  constructor() {
    this.statusBarItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Right,
      95,
    );
    this.statusBarItem.command = "codypendent.showInboxQuickPick";
    this.update(0);
  }

  update(unreadCount: number): void {
    if (unreadCount > 0) {
      this.statusBarItem.text = `$(bell) Inbox: ${unreadCount}`;
      this.statusBarItem.tooltip = `Codypendent: ${unreadCount} unread notification${unreadCount === 1 ? "" : "s"}`;
      this.statusBarItem.backgroundColor = new vscode.ThemeColor(
        "statusBarItem.warningBackground",
      );
    } else {
      this.statusBarItem.text = `$(inbox) Inbox`;
      this.statusBarItem.tooltip = "Codypendent: Inbox (all caught up)";
      this.statusBarItem.backgroundColor = undefined;
    }
    this.statusBarItem.show();
  }

  dispose(): void {
    this.statusBarItem.dispose();
  }
}

export interface InboxQuickPickItem extends vscode.QuickPickItem {
  entry: InboxEntry;
}

export async function showInboxQuickPick(
  client: DaemonClient | undefined,
  treeProvider?: InboxTreeDataProvider,
  onNavigate?: (deepLink: InboxDeepLink) => void,
): Promise<void> {
  if (!client) {
    void vscode.window.showWarningMessage("Codypendent: not connected to daemon.");
    return;
  }

  let entries: InboxEntry[] = [];
  try {
    const page = await client.listInbox();
    entries = page.items;
    treeProvider?.setEntries(entries);
  } catch (err) {
    void vscode.window.showErrorMessage(
      `Codypendent: failed to list inbox: ${err instanceof Error ? err.message : String(err)}`,
    );
    return;
  }

  if (entries.length === 0) {
    void vscode.window.showInformationMessage("Codypendent: Inbox is empty.");
    return;
  }

  const items: InboxQuickPickItem[] = entries.map((entry) => {
    const kind = entry.kind?.type ?? "Unknown";
    const state = entry.state?.type ?? "Unread";
    const icon = state === "Unread" ? "$(bell-dot)" : "$(check)";
    return {
      label: `${icon} ${entry.title}`,
      description: `${kind} · ${state}`,
      detail: entry.summary,
      entry,
    };
  });

  const selected = await vscode.window.showQuickPick(items, {
    title: "Codypendent: Inbox Notifications & Pending Work",
    placeHolder: "Select an inbox item to inspect or act upon",
    matchOnDescription: true,
    matchOnDetail: true,
  });

  if (!selected) return;

  const entry = selected.entry;
  const isUnread = !entry.state || entry.state.type === "Unread";

  const actions: string[] = ["Open Target"];
  if (isUnread) {
    actions.push("Acknowledge");
  }
  if (entry.state?.type !== "Dismissed" && entry.state?.type !== "Resolved") {
    actions.push("Dismiss");
  }

  const action = await vscode.window.showQuickPick(actions, {
    title: `${entry.title}`,
    placeHolder: "Choose an action for this notification",
  });

  if (!action) return;

  if (action === "Open Target") {
    onNavigate?.(entry.deep_link);
  } else if (action === "Acknowledge") {
    try {
      await client.mutateInbox({ type: "Acknowledge", entry_id: entry.id });
      void vscode.window.showInformationMessage(`Acknowledged: ${entry.title}`);
      await treeProvider?.refresh();
    } catch (err) {
      void vscode.window.showErrorMessage(`Failed to acknowledge: ${err instanceof Error ? err.message : String(err)}`);
    }
  } else if (action === "Dismiss") {
    try {
      await client.mutateInbox({ type: "Dismiss", entry_id: entry.id });
      void vscode.window.showInformationMessage(`Dismissed: ${entry.title}`);
      await treeProvider?.refresh();
    } catch (err) {
      void vscode.window.showErrorMessage(`Failed to dismiss: ${err instanceof Error ? err.message : String(err)}`);
    }
  }
}
