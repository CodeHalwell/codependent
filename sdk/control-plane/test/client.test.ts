import { describe, it, expect, vi, beforeEach } from "vitest";
import { ControlPlaneClient } from "../src/client.js";
import {
  NotFoundError,
  UnauthorizedError,
  ValidationError,
  PolicyViolationError,
} from "../src/errors.js";

describe("ControlPlaneClient", () => {
  const baseUrl = "https://control-plane.example.com";
  let client: ControlPlaneClient;
  let mockFetch: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    mockFetch = vi.fn();
    client = new ControlPlaneClient({
      baseUrl,
      token: "test-token-123",
      fetch: mockFetch as unknown as typeof fetch,
    });
  });

  describe("Authentication and headers", () => {
    it("attaches Bearer token to requests", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: async () => ({ id: "u-1", displayName: "Alice", state: "active", primaryEmail: "alice@example.com", createdAt: "", updatedAt: "" }),
      });

      const user = await client.getCurrentUser();
      expect(user.displayName).toBe("Alice");

      expect(mockFetch).toHaveBeenCalledWith(
        "https://control-plane.example.com/v1/auth/me",
        expect.objectContaining({
          headers: expect.objectContaining({
            Authorization: "Bearer test-token-123",
            Accept: "application/json",
          }),
        })
      );
    });

    it("attaches X-API-Key when configured", async () => {
      const apiKeyClient = new ControlPlaneClient({
        baseUrl,
        apiKey: "cody_test_key_xyz",
        fetch: mockFetch as unknown as typeof fetch,
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: async () => ({ id: "u-1", displayName: "Service Account", state: "active", primaryEmail: null, createdAt: "", updatedAt: "" }),
      });

      await apiKeyClient.getCurrentUser();

      expect(mockFetch).toHaveBeenCalledWith(
        "https://control-plane.example.com/v1/auth/me",
        expect.objectContaining({
          headers: expect.objectContaining({
            "X-API-Key": "cody_test_key_xyz",
          }),
        })
      );
    });

    it("generates automatic Idempotency-Key on mutations", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: async () => ({
          id: "org-1",
          slug: "acme",
          displayName: "Acme Corp",
          maxPublicationClass: "metadata-shared",
          maxClassification: "internal",
          policyVersion: 1,
          createdAt: "",
          dataResidency: null,
          retentionDays: null,
        }),
      });

      await client.createOrganization({
        slug: "acme",
        displayName: "Acme Corp",
      });

      const lastCall = mockFetch.mock.calls[0];
      const headers = lastCall[1].headers;
      expect(headers["Idempotency-Key"]).toBeDefined();
      expect(typeof headers["Idempotency-Key"]).toBe("string");
      expect(headers["Idempotency-Key"].length).toBeGreaterThan(10);
    });
  });

  describe("Error handling and non-disclosure", () => {
    it("normalizes 404 response to NotFoundError", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 404,
        json: async () => ({
          type: "not_found",
          resource: "repository",
          message: "no such repository",
        }),
      });

      await expect(client.getRepository("org-1", "repo-nonexistent")).rejects.toThrow(NotFoundError);
    });

    it("treats unauthorized repository access as 404 NotFoundError (non-disclosure)", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 404,
        json: async () => ({
          type: "not_found",
          resource: "repository",
          message: "no such repository",
        }),
      });

      try {
        await client.getRepository("org-1", "repo-forbidden-to-user");
        expect.unreachable("should have thrown");
      } catch (err) {
        expect(err).toBeInstanceOf(NotFoundError);
        const notFound = err as NotFoundError;
        expect(notFound.status).toBe(404);
        expect(notFound.resource).toBe("repository");
        expect(notFound.message).toBe("no such repository");
      }
    });

    it("normalizes 401 to UnauthorizedError", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 401,
        json: async () => ({
          type: "unauthorized",
          message: "no valid credential presented",
        }),
      });

      await expect(client.getCurrentUser()).rejects.toThrow(UnauthorizedError);
    });

    it("normalizes 400 to ValidationError", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 400,
        json: async () => ({
          type: "validation_error",
          message: "invalid request parameters",
          detail: { field: "slug is required" },
        }),
      });

      await expect(
        client.createOrganization({ slug: "", displayName: "test" })
      ).rejects.toThrow(ValidationError);
    });

    it("normalizes 422 to PolicyViolationError", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 422,
        json: async () => ({
          type: "policy_violation",
          message: "max publication class exceeds organization ceiling",
        }),
      });

      await expect(
        client.registerRepository("org-1", {
          federatedId: "a".repeat(64),
          displayName: "Repo",
          maxPublicationClass: "public-marketplace",
        })
      ).rejects.toThrow(PolicyViolationError);
    });
  });

  describe("API endpoints", () => {
    it("lists organizations", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: async () => [
          {
            id: "org-1",
            slug: "codypendent",
            displayName: "Codypendent Org",
            maxPublicationClass: "metadata-shared",
            maxClassification: "internal",
            policyVersion: 1,
            createdAt: "2026-08-17T00:00:00Z",
            dataResidency: null,
            retentionDays: null,
          },
        ],
      });

      const orgs = await client.listOrganizations();
      expect(orgs).toHaveLength(1);
      expect(orgs[0].slug).toBe("codypendent");
    });

    it("lists shared sessions with keyset cursor and query filters", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: async () => ({
          items: [
            {
              id: "sess-1",
              organizationId: "org-1",
              repositoryId: "repo-1",
              daemonId: "daemon-1",
              remoteSessionKey: "sess-local-key",
              class: "metadata-shared",
              title: null,
              state: "running",
              startedAt: "2026-08-17T10:00:00Z",
              lastActivityAt: "2026-08-17T10:15:00Z",
              tombstonedAt: null,
              updatedAt: "2026-08-17T10:15:00Z",
            },
          ],
          cursor: "next-cursor-abc",
          hasMore: true,
          total: 1,
        }),
      });

      const page = await client.listSharedSessions("org-1", {
        repositoryId: "repo-1",
        state: "running",
        limit: 10,
        cursor: "prev-cursor",
      });

      expect(page.items).toHaveLength(1);
      expect(page.cursor).toBe("next-cursor-abc");
      expect(page.hasMore).toBe(true);

      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining("/v1/organizations/org-1/sessions?repository_id=repo-1&state=running&limit=10&cursor=prev-cursor"),
        expect.anything()
      );
    });

    it("submits approval decision with actionDigest", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: async () => ({
          approvalId: "appr-1",
          status: "approved",
          decidedAt: "2026-08-17T10:30:00Z",
          receiptId: "rcpt-1",
        }),
      });

      const response = await client.decideApproval("org-1", "appr-1", {
        decision: "approve",
        actionDigest: "digest-123456",
        reason: "Looks good to deploy",
      });

      expect(response.status).toBe("approved");
      expect(mockFetch).toHaveBeenCalledWith(
        "https://control-plane.example.com/v1/organizations/org-1/approvals/appr-1/decide",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({
            decision: "approve",
            actionDigest: "digest-123456",
            reason: "Looks good to deploy",
          }),
        })
      );
    });

    it("creates API key and returns secret token", async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: async () => ({
          apiKey: {
            id: "key-1",
            organizationId: "org-1",
            name: "CI Key",
            keyPrefix: "cody_live_ci",
            role: "contributor",
            createdAt: "2026-08-17T10:00:00Z",
            lastUsedAt: null,
            expiresAt: null,
            revokedAt: null,
          },
          token: "cody_live_ci_secret987654321",
        }),
      });

      const res = await client.createApiKey("org-1", {
        organizationId: "org-1",
        name: "CI Key",
        role: "contributor",
      });

      expect(res.token).toBe("cody_live_ci_secret987654321");
      expect(res.apiKey.keyPrefix).toBe("cody_live_ci");
    });
  });
});
