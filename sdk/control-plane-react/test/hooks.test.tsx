import { describe, it, expect, vi } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { ControlPlaneProvider } from "../src/context.js";
import { useSessions } from "../src/hooks/useSessions.js";
import { useApprovals } from "../src/hooks/useApprovals.js";
import { useAuditLogs } from "../src/hooks/useAuditLogs.js";
import { useInbox } from "../src/hooks/useInbox.js";
import { useUsers } from "../src/hooks/useUsers.js";
import { useApiKeys } from "../src/hooks/useApiKeys.js";
import { useDaemons } from "../src/hooks/useDaemons.js";
import { ControlPlaneClient } from "@codypendent/control-plane";

function SessionsComponent() {
  const { sessions, isLoading, selectedSession, setSelectedSessionId } = useSessions({
    subscribeLive: false,
  });

  return (
    <div>
      <div data-testid="sessions-loading">{isLoading ? "loading" : "done"}</div>
      <div data-testid="sessions-count">{sessions.length}</div>
      {sessions.map((s) => (
        <button key={s.id} data-testid={`session-${s.id}`} onClick={() => setSelectedSessionId(s.id)}>
          {s.remoteSessionKey}
        </button>
      ))}
      <div data-testid="selected-session-steps">{selectedSession?.steps.length ?? 0}</div>
    </div>
  );
}

function ApprovalsComponent() {
  const { pendingApprovals, decide } = useApprovals({ subscribeLive: false });

  return (
    <div>
      <div data-testid="approvals-count">{pendingApprovals.length}</div>
      {pendingApprovals.map((a) => (
        <button
          key={a.id}
          data-testid={`approve-${a.id}`}
          onClick={() => decide(a.id, "approve", a.actionDigest, "LGTM")}
        >
          Approve {a.action}
        </button>
      ))}
    </div>
  );
}

function AuditComponent() {
  const { records, verifyChain, verificationResult } = useAuditLogs();

  return (
    <div>
      <div data-testid="audit-count">{records.length}</div>
      <button data-testid="verify-btn" onClick={() => verifyChain()}>
        Verify
      </button>
      <div data-testid="verify-status">{verificationResult?.valid ? "verified" : "not-verified"}</div>
    </div>
  );
}

function InboxComponent() {
  const { items, unreadCount, markAsRead } = useInbox({ subscribeLive: false });

  return (
    <div>
      <div data-testid="inbox-count">{items.length}</div>
      <div data-testid="unread-count">{unreadCount}</div>
      {items.map((i) => (
        <button key={i.id} data-testid={`read-${i.id}`} onClick={() => markAsRead(i.id)}>
          Read
        </button>
      ))}
    </div>
  );
}

function UsersAndApiKeysComponent() {
  const { members, inviteUser } = useUsers();
  const { apiKeys, createApiKey, revokeApiKey } = useApiKeys();
  const { daemons, createPairingChallenge } = useDaemons();

  return (
    <div>
      <div data-testid="members-count">{members.length}</div>
      <div data-testid="keys-count">{apiKeys.length}</div>
      <div data-testid="daemons-count">{daemons.length}</div>
      <button data-testid="invite-btn" onClick={() => inviteUser("bob@example.com", "contributor")}>
        Invite
      </button>
      <button data-testid="create-key-btn" onClick={() => createApiKey({ name: "Test Key", role: "observer" })}>
        Create Key
      </button>
      <button data-testid="revoke-key-btn" onClick={() => (apiKeys[0] ? revokeApiKey(apiKeys[0].id) : undefined)}>
        Revoke Key
      </button>
      <button data-testid="challenge-btn" onClick={() => createPairingChallenge()}>
        Challenge
      </button>
    </div>
  );
}

describe("ControlPlane React Hooks", () => {
  it("useSessions loads sessions and selects detail", async () => {
    const mockClient = new ControlPlaneClient({ baseUrl: "https://control-plane.example.com" });

    vi.spyOn(mockClient, "listOrganizations").mockResolvedValue([
      {
        id: "org-1",
        slug: "acme",
        displayName: "Acme Corp",
        maxPublicationClass: "metadata-shared",
        maxClassification: "internal",
        policyVersion: 1,
        createdAt: "",
        dataResidency: null,
        retentionDays: null,
      },
    ]);
    vi.spyOn(mockClient, "listRepositories").mockResolvedValue([]);
    vi.spyOn(mockClient, "listTeams").mockResolvedValue([]);

    vi.spyOn(mockClient, "listSharedSessions").mockResolvedValue({
      items: [
        {
          id: "sess-1",
          organizationId: "org-1",
          repositoryId: "repo-1",
          daemonId: "daemon-1",
          remoteSessionKey: "key-123",
          class: "metadata-shared",
          title: null,
          state: "running",
          startedAt: "2026-08-17T10:00:00Z",
          lastActivityAt: null,
          tombstonedAt: null,
          updatedAt: "",
        },
      ],
      cursor: null,
      hasMore: false,
    });

    vi.spyOn(mockClient, "getSharedSession").mockResolvedValue({
      id: "sess-1",
      organizationId: "org-1",
      repositoryId: "repo-1",
      daemonId: "daemon-1",
      remoteSessionKey: "key-123",
      class: "metadata-shared",
      title: null,
      state: "running",
      startedAt: "2026-08-17T10:00:00Z",
      lastActivityAt: null,
      tombstonedAt: null,
      updatedAt: "",
      steps: [
        {
          id: "step-1",
          stepIndex: 0,
          kind: "planning",
          title: "Plan execution",
          status: "completed",
          startedAt: "2026-08-17T10:00:01Z",
        },
      ],
    });

    render(
      <ControlPlaneProvider client={mockClient} initialOrganizationId="org-1">
        <SessionsComponent />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(screen.getByTestId("sessions-count").textContent).toBe("1");

    // Click session to load details
    await act(async () => {
      screen.getByTestId("session-sess-1").click();
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(screen.getByTestId("selected-session-steps").textContent).toBe("1");
  });

  it("useApprovals decides an approval", async () => {
    const mockClient = new ControlPlaneClient({ baseUrl: "https://control-plane.example.com" });
    vi.spyOn(mockClient, "listOrganizations").mockResolvedValue([]);
    vi.spyOn(mockClient, "listRepositories").mockResolvedValue([]);
    vi.spyOn(mockClient, "listTeams").mockResolvedValue([]);

    vi.spyOn(mockClient, "listPendingApprovals").mockResolvedValue([
      {
        id: "appr-1",
        organizationId: "org-1",
        repositoryId: "repo-1",
        daemonId: "d-1",
        action: "file_edit",
        targetKind: "file",
        targetId: "src/index.ts",
        actionDigest: "digest-abc",
        description: "Modify src/index.ts",
        requestedAt: "2026-08-17T10:00:00Z",
        expiresAt: null,
        status: "pending",
        decidedBy: null,
        decidedAt: null,
        decisionReason: null,
      },
    ]);

    const decideSpy = vi.spyOn(mockClient, "decideApproval").mockResolvedValue({
      approvalId: "appr-1",
      status: "approved",
      decidedAt: "2026-08-17T10:05:00Z",
      receiptId: "r-1",
    });

    render(
      <ControlPlaneProvider client={mockClient} initialOrganizationId="org-1">
        <ApprovalsComponent />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(screen.getByTestId("approvals-count").textContent).toBe("1");

    await act(async () => {
      screen.getByTestId("approve-appr-1").click();
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(decideSpy).toHaveBeenCalledWith("org-1", "appr-1", {
      decision: "approve",
      actionDigest: "digest-abc",
      reason: "LGTM",
    });
    expect(screen.getByTestId("approvals-count").textContent).toBe("0");
  });

  it("useAuditLogs verifies hash chain", async () => {
    const mockClient = new ControlPlaneClient({ baseUrl: "https://control-plane.example.com" });
    vi.spyOn(mockClient, "listOrganizations").mockResolvedValue([]);
    vi.spyOn(mockClient, "listRepositories").mockResolvedValue([]);
    vi.spyOn(mockClient, "listTeams").mockResolvedValue([]);

    vi.spyOn(mockClient, "listAuditRecords").mockResolvedValue({
      items: [
        {
          id: "rec-1",
          organizationId: "org-1",
          actorKind: "user",
          actorId: "u-1",
          action: "org.create",
          targetKind: "org",
          targetId: "org-1",
          actionDigest: "d1",
          correlationId: null,
          prevHash: null,
          recordHash: "hash-1",
          detail: {},
          occurredAt: "2026-08-17T10:00:00Z",
        },
      ],
      cursor: null,
      hasMore: false,
    });

    vi.spyOn(mockClient, "verifyAuditChain").mockResolvedValue({
      valid: true,
      totalRecordsChecked: 1,
      message: "Audit chain cryptographically verified: 1 records intact",
    });

    render(
      <ControlPlaneProvider client={mockClient} initialOrganizationId="org-1">
        <AuditComponent />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(screen.getByTestId("audit-count").textContent).toBe("1");

    await act(async () => {
      screen.getByTestId("verify-btn").click();
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(screen.getByTestId("verify-status").textContent).toBe("verified");
  });

  it("useInbox marks items as read", async () => {
    const mockClient = new ControlPlaneClient({ baseUrl: "https://control-plane.example.com" });
    vi.spyOn(mockClient, "listOrganizations").mockResolvedValue([]);
    vi.spyOn(mockClient, "listRepositories").mockResolvedValue([]);
    vi.spyOn(mockClient, "listTeams").mockResolvedValue([]);

    vi.spyOn(mockClient, "listInbox").mockResolvedValue({
      items: [
        {
          id: "inbox-1",
          organizationId: "org-1",
          userId: null,
          kind: "notification",
          state: "unread",
          title: "Session Finished",
          body: "Session run completed successfully",
          sourceKind: "daemon",
          sourceId: "d-1",
          createdAt: "2026-08-17T10:00:00Z",
          readAt: null,
          actedAt: null,
        },
      ],
      cursor: null,
      hasMore: false,
    });

    const mutateSpy = vi.spyOn(mockClient, "mutateInbox").mockResolvedValue(undefined);

    render(
      <ControlPlaneProvider client={mockClient} initialOrganizationId="org-1">
        <InboxComponent />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(screen.getByTestId("inbox-count").textContent).toBe("1");
    expect(screen.getByTestId("unread-count").textContent).toBe("1");

    await act(async () => {
      screen.getByTestId("read-inbox-1").click();
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(mutateSpy).toHaveBeenCalledWith("org-1", "inbox-1", { state: "read" });
    expect(screen.getByTestId("unread-count").textContent).toBe("0");
  });

  it("useUsers, useApiKeys, and useDaemons handle management actions", async () => {
    const mockClient = new ControlPlaneClient({ baseUrl: "https://control-plane.example.com" });
    vi.spyOn(mockClient, "listOrganizations").mockResolvedValue([]);
    vi.spyOn(mockClient, "listRepositories").mockResolvedValue([]);
    vi.spyOn(mockClient, "listTeams").mockResolvedValue([]);

    vi.spyOn(mockClient, "listUsers").mockResolvedValue([
      {
        id: "u-1",
        displayName: "Alice",
        primaryEmail: "alice@example.com",
        state: "active",
        createdAt: "",
        updatedAt: "",
      },
    ]);
    vi.spyOn(mockClient, "listRoleGrants").mockResolvedValue([]);

    vi.spyOn(mockClient, "listApiKeys").mockResolvedValue([
      {
        id: "k-1",
        organizationId: "org-1",
        name: "Key 1",
        keyPrefix: "cody_test",
        role: "contributor",
        createdAt: "",
        lastUsedAt: null,
        expiresAt: null,
        revokedAt: null,
      },
    ]);

    vi.spyOn(mockClient, "listDaemons").mockResolvedValue([
      {
        id: "d-1",
        organizationId: "org-1",
        pairedBy: "u-1",
        displayName: "Alice MacBook",
        consentManifestHash: "h".repeat(64),
        maxPublicationClass: "metadata-shared",
        acceptsRemoteApprovals: true,
        acceptsRunnerDispatch: false,
        state: "active",
        pairedAt: "",
        revokedAt: null,
        lastSeenAt: "",
        createdAt: "",
      },
    ]);

    const inviteSpy = vi.spyOn(mockClient, "inviteUser").mockResolvedValue({
      id: "u-2",
      displayName: "Bob",
      primaryEmail: "bob@example.com",
      state: "active",
      createdAt: "",
      updatedAt: "",
    });

    const createKeySpy = vi.spyOn(mockClient, "createApiKey").mockResolvedValue({
      apiKey: {
        id: "k-2",
        organizationId: "org-1",
        name: "Test Key",
        keyPrefix: "cody_test2",
        role: "observer",
        createdAt: "",
        lastUsedAt: null,
        expiresAt: null,
        revokedAt: null,
      },
      token: "secret_12345",
    });

    const revokeKeySpy = vi.spyOn(mockClient, "revokeApiKey").mockResolvedValue(undefined);

    const challengeSpy = vi.spyOn(mockClient, "createPairingChallenge").mockResolvedValue({
      code: "PAIR-123456",
      organizationId: "org-1",
      requestedScope: {
        maxPublicationClass: "metadata-shared",
        acceptsRemoteApprovals: true,
        acceptsRunnerDispatch: false,
      },
      expiresAt: "2026-08-17T11:00:00Z",
    });

    render(
      <ControlPlaneProvider client={mockClient} initialOrganizationId="org-1">
        <UsersAndApiKeysComponent />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(screen.getByTestId("members-count").textContent).toBe("1");
    expect(screen.getByTestId("keys-count").textContent).toBe("1");
    expect(screen.getByTestId("daemons-count").textContent).toBe("1");

    await act(async () => {
      screen.getByTestId("invite-btn").click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(inviteSpy).toHaveBeenCalledWith("org-1", "bob@example.com", "contributor");

    await act(async () => {
      screen.getByTestId("create-key-btn").click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(createKeySpy).toHaveBeenCalledWith("org-1", {
      organizationId: "org-1",
      name: "Test Key",
      role: "observer",
    });

    await act(async () => {
      screen.getByTestId("revoke-key-btn").click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(revokeKeySpy).toHaveBeenCalledWith("org-1", "k-1");

    await act(async () => {
      screen.getByTestId("challenge-btn").click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(challengeSpy).toHaveBeenCalledWith({
      organizationId: "org-1",
    });
  });
});
