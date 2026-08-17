import type {
  ApiKey,
  ApiKeyCreatedResponse,
  AuthSession,
  AuthTokens,
  CreateApiKeyRequest,
  CreateOrganizationRequest,
  CreatePairingChallengeRequest,
  CreateTeamRequest,
  AddTeamMemberRequest,
  Daemon,
  GrantRoleRequest,
  IdempotentRequestOptions,
  InboxListQuery,
  InboxMutationRequest,
  InboxPage,
  LoginResponse,
  OAuthCallbackRequest,
  ObjectUploadReceipt,
  ObjectUploadRequest,
  Organization,
  PairingChallenge,
  PendingApproval,
  ApprovalDecisionRequest,
  ApprovalDecisionResponse,
  PresignedUrlResponse,
  RegisterRepositoryRequest,
  RefreshTokenRequest,
  Repository,
  RequestOptions,
  RoleGrant,
  SessionListPage,
  SessionListQuery,
  SharedSessionDetail,
  Team,
  TeamMember,
  UpdateOrganizationPolicyRequest,
  UpdateRepositoryPolicyRequest,
  User,
  AuditQuery,
  AuditPage,
  AuditVerificationResult,
} from "./types/index.js";
import { parseApiError, ControlPlaneError } from "./errors.js";
import { generateIdempotencyKey } from "./utils/idempotency.js";
import { verifyAuditHashChain } from "./utils/audit-verifier.js";

export interface ControlPlaneClientConfig {
  baseUrl: string;
  token?: string | null | undefined;
  apiKey?: string | null | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  timeoutMs?: number | undefined;
  defaultHeaders?: Record<string, string> | undefined;
  onTokenRefresh?: ((tokens: AuthTokens) => void) | undefined;
}

export class ControlPlaneClient {
  private readonly baseUrl: string;
  private token: string | null;
  private apiKey: string | null;
  private readonly customFetch: typeof globalThis.fetch;
  private readonly timeoutMs: number;
  private readonly defaultHeaders: Record<string, string>;
  private readonly onTokenRefresh?: ((tokens: AuthTokens) => void) | undefined;

  constructor(config: ControlPlaneClientConfig) {
    this.baseUrl = config.baseUrl.replace(/\/+$/, "");
    this.token = config.token ?? null;
    this.apiKey = config.apiKey ?? null;
    this.customFetch = config.fetch ?? globalThis.fetch.bind(globalThis);
    this.timeoutMs = config.timeoutMs ?? 30000;
    this.defaultHeaders = config.defaultHeaders ?? {};
    this.onTokenRefresh = config.onTokenRefresh;
  }

  public setToken(token: string | null): void {
    this.token = token;
  }

  public setApiKey(apiKey: string | null): void {
    this.apiKey = apiKey;
  }

  public getToken(): string | null {
    return this.token;
  }

  public getApiKey(): string | null {
    return this.apiKey;
  }

  public getBaseUrl(): string {
    return this.baseUrl;
  }

  private async request<T>(
    path: string,
    options: {
      method?: string | undefined;
      body?: unknown;
      params?: Record<string, string | number | boolean | undefined | null> | undefined;
      headers?: Record<string, string> | undefined;
      idempotencyKey?: string | undefined;
      signal?: AbortSignal | undefined;
    } = {}
  ): Promise<T> {
    const url = new URL(`${this.baseUrl}${path.startsWith("/") ? path : `/${path}`}`);

    if (options.params) {
      for (const [key, value] of Object.entries(options.params)) {
        if (value !== undefined && value !== null) {
          url.searchParams.set(key, String(value));
        }
      }
    }

    const headers: Record<string, string> = {
      Accept: "application/json",
      ...this.defaultHeaders,
      ...options.headers,
    };

    if (this.token) {
      headers.Authorization = `Bearer ${this.token}`;
    } else if (this.apiKey) {
      headers["X-API-Key"] = this.apiKey;
    }

    if (options.idempotencyKey) {
      headers["Idempotency-Key"] = options.idempotencyKey;
    }

    let bodyStr: string | undefined;
    if (options.body !== undefined) {
      headers["Content-Type"] = "application/json";
      bodyStr = JSON.stringify(options.body);
    }

    let abortSignal = options.signal;
    if (!abortSignal && this.timeoutMs > 0 && typeof AbortSignal !== "undefined" && typeof AbortSignal.timeout === "function") {
      abortSignal = AbortSignal.timeout(this.timeoutMs);
    }

    const initObj: RequestInit = {
      method: options.method ?? "GET",
      headers,
    };
    if (bodyStr !== undefined) {
      initObj.body = bodyStr;
    }
    if (abortSignal) {
      initObj.signal = abortSignal;
    }

    let response: Response;
    try {
      response = await this.customFetch(url.toString(), initObj);
    } catch (err) {
      if (err instanceof ControlPlaneError) {
        throw err;
      }
      throw parseApiError(0, { message: (err as Error).message });
    }

    if (!response.ok) {
      let errorData: unknown;
      try {
        errorData = await response.json();
      } catch {
        errorData = { message: response.statusText || `HTTP ${response.status}` };
      }
      throw parseApiError(response.status, errorData);
    }

    if (response.status === 204) {
      return undefined as unknown as T;
    }

    return (await response.json()) as T;
  }

  /* -------------------------------------------------------------------------- */
  /*                                    AUTH                                    */
  /* -------------------------------------------------------------------------- */

  public async getGitHubLoginUrl(redirectUri?: string | undefined): Promise<LoginResponse> {
    return this.request<LoginResponse>("/v1/auth/github/login", {
      params: { redirect_uri: redirectUri },
    });
  }

  public async getOidcLoginUrl(redirectUri?: string | undefined): Promise<LoginResponse> {
    return this.request<LoginResponse>("/v1/auth/oidc/login", {
      params: { redirect_uri: redirectUri },
    });
  }

  public async handleOAuthCallback(data: OAuthCallbackRequest): Promise<AuthSession> {
    const session = await this.request<AuthSession>("/v1/auth/callback", {
      method: "POST",
      body: data,
    });
    if (session.tokens?.accessToken) {
      this.setToken(session.tokens.accessToken);
    }
    return session;
  }

  public async refreshToken(data: RefreshTokenRequest): Promise<AuthTokens> {
    const tokens = await this.request<AuthTokens>("/v1/auth/refresh", {
      method: "POST",
      body: data,
    });
    if (tokens.accessToken) {
      this.setToken(tokens.accessToken);
    }
    if (this.onTokenRefresh) {
      this.onTokenRefresh(tokens);
    }
    return tokens;
  }

  public async getCurrentUser(options?: RequestOptions | undefined): Promise<User> {
    return this.request<User>("/v1/auth/me", {
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async logout(options?: RequestOptions | undefined): Promise<void> {
    await this.request<void>("/v1/auth/logout", {
      method: "POST",
      headers: options?.headers,
      signal: options?.signal,
    });
    this.setToken(null);
  }

  public async createPairingChallenge(
    data: CreatePairingChallengeRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<PairingChallenge> {
    return this.request<PairingChallenge>("/v1/workloads/pairing/challenge", {
      method: "POST",
      body: data,
      idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  /* -------------------------------------------------------------------------- */
  /*                                ORGANIZATIONS                               */
  /* -------------------------------------------------------------------------- */

  public async listOrganizations(options?: RequestOptions | undefined): Promise<Organization[]> {
    return this.request<Organization[]>("/v1/organizations", {
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async getOrganization(id: string, options?: RequestOptions | undefined): Promise<Organization> {
    return this.request<Organization>(`/v1/organizations/${id}`, {
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async createOrganization(
    data: CreateOrganizationRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<Organization> {
    return this.request<Organization>("/v1/organizations", {
      method: "POST",
      body: data,
      idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async updateOrganizationPolicy(
    id: string,
    data: UpdateOrganizationPolicyRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<Organization> {
    return this.request<Organization>(`/v1/organizations/${id}/policy`, {
      method: "PATCH",
      body: data,
      idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async deleteOrganization(id: string, options?: RequestOptions | undefined): Promise<void> {
    return this.request<void>(`/v1/organizations/${id}`, {
      method: "DELETE",
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  /* -------------------------------------------------------------------------- */
  /*                                    TEAMS                                   */
  /* -------------------------------------------------------------------------- */

  public async listTeams(organizationId: string, options?: RequestOptions | undefined): Promise<Team[]> {
    return this.request<Team[]>(`/v1/organizations/${organizationId}/teams`, {
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async getTeam(organizationId: string, teamId: string, options?: RequestOptions | undefined): Promise<Team> {
    return this.request<Team>(`/v1/organizations/${organizationId}/teams/${teamId}`, {
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async createTeam(
    organizationId: string,
    data: CreateTeamRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<Team> {
    return this.request<Team>(`/v1/organizations/${organizationId}/teams`, {
      method: "POST",
      body: data,
      idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async listTeamMembers(
    organizationId: string,
    teamId: string,
    options?: RequestOptions | undefined
  ): Promise<TeamMember[]> {
    return this.request<TeamMember[]>(
      `/v1/organizations/${organizationId}/teams/${teamId}/members`,
      {
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  public async addTeamMember(
    organizationId: string,
    teamId: string,
    data: AddTeamMemberRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<TeamMember> {
    return this.request<TeamMember>(
      `/v1/organizations/${organizationId}/teams/${teamId}/members`,
      {
        method: "POST",
        body: data,
        idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  public async removeTeamMember(
    organizationId: string,
    teamId: string,
    userId: string,
    options?: RequestOptions | undefined
  ): Promise<void> {
    return this.request<void>(
      `/v1/organizations/${organizationId}/teams/${teamId}/members/${userId}`,
      {
        method: "DELETE",
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  /* -------------------------------------------------------------------------- */
  /*                                REPOSITORIES                                */
  /* -------------------------------------------------------------------------- */

  public async listRepositories(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<Repository[]> {
    return this.request<Repository[]>(`/v1/organizations/${organizationId}/repositories`, {
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async getRepository(
    organizationId: string,
    repositoryId: string,
    options?: RequestOptions | undefined
  ): Promise<Repository> {
    return this.request<Repository>(
      `/v1/organizations/${organizationId}/repositories/${repositoryId}`,
      {
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  public async registerRepository(
    organizationId: string,
    data: RegisterRepositoryRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<Repository> {
    return this.request<Repository>(`/v1/organizations/${organizationId}/repositories`, {
      method: "POST",
      body: data,
      idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async updateRepositoryPolicy(
    organizationId: string,
    repositoryId: string,
    data: UpdateRepositoryPolicyRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<Repository> {
    return this.request<Repository>(
      `/v1/organizations/${organizationId}/repositories/${repositoryId}/policy`,
      {
        method: "PATCH",
        body: data,
        idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  public async deleteRepository(
    organizationId: string,
    repositoryId: string,
    options?: RequestOptions | undefined
  ): Promise<void> {
    return this.request<void>(
      `/v1/organizations/${organizationId}/repositories/${repositoryId}`,
      {
        method: "DELETE",
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  /* -------------------------------------------------------------------------- */
  /*                                 ROLE GRANTS                                */
  /* -------------------------------------------------------------------------- */

  public async listRoleGrants(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<RoleGrant[]> {
    return this.request<RoleGrant[]>(`/v1/organizations/${organizationId}/roles`, {
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async grantRole(
    organizationId: string,
    data: GrantRoleRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<RoleGrant> {
    return this.request<RoleGrant>(`/v1/organizations/${organizationId}/roles`, {
      method: "POST",
      body: data,
      idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async revokeRoleGrant(
    organizationId: string,
    grantId: string,
    options?: RequestOptions | undefined
  ): Promise<void> {
    return this.request<void>(`/v1/organizations/${organizationId}/roles/${grantId}`, {
      method: "DELETE",
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  /* -------------------------------------------------------------------------- */
  /*                              SESSIONS & RUNS                               */
  /* -------------------------------------------------------------------------- */

  public async listSharedSessions(
    organizationId: string,
    query: SessionListQuery = {},
    options?: RequestOptions | undefined
  ): Promise<SessionListPage> {
    return this.request<SessionListPage>(
      `/v1/organizations/${organizationId}/sessions`,
      {
        params: {
          repository_id: query.repositoryId,
          state: query.state,
          since: query.since,
          until: query.until,
          search: query.search,
          limit: query.limit,
          cursor: query.cursor,
          direction: query.direction,
        },
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  public async getSharedSession(
    organizationId: string,
    sessionId: string,
    options?: RequestOptions | undefined
  ): Promise<SharedSessionDetail> {
    return this.request<SharedSessionDetail>(
      `/v1/organizations/${organizationId}/sessions/${sessionId}`,
      {
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  /* -------------------------------------------------------------------------- */
  /*                                    INBOX                                   */
  /* -------------------------------------------------------------------------- */

  public async listInbox(
    organizationId: string,
    query: InboxListQuery = {},
    options?: RequestOptions | undefined
  ): Promise<InboxPage> {
    return this.request<InboxPage>(`/v1/organizations/${organizationId}/inbox`, {
      params: {
        state: query.state,
        kind: query.kind,
        limit: query.limit,
        cursor: query.cursor,
      },
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async mutateInbox(
    organizationId: string,
    entryId: string,
    data: InboxMutationRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<void> {
    return this.request<void>(
      `/v1/organizations/${organizationId}/inbox/${entryId}`,
      {
        method: "PATCH",
        body: data,
        idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  /* -------------------------------------------------------------------------- */
  /*                                  APPROVALS                                 */
  /* -------------------------------------------------------------------------- */

  public async listPendingApprovals(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<PendingApproval[]> {
    return this.request<PendingApproval[]>(
      `/v1/organizations/${organizationId}/approvals`,
      {
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  public async decideApproval(
    organizationId: string,
    approvalId: string,
    data: ApprovalDecisionRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<ApprovalDecisionResponse> {
    return this.request<ApprovalDecisionResponse>(
      `/v1/organizations/${organizationId}/approvals/${approvalId}/decide`,
      {
        method: "POST",
        body: data,
        idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  /* -------------------------------------------------------------------------- */
  /*                                 AUDIT LOGS                                 */
  /* -------------------------------------------------------------------------- */

  public async listAuditRecords(
    organizationId: string,
    query: AuditQuery = {},
    options?: RequestOptions | undefined
  ): Promise<AuditPage> {
    return this.request<AuditPage>(`/v1/organizations/${organizationId}/audit`, {
      params: {
        actor_kind: query.actorKind,
        actor_id: query.actorId,
        action: query.action,
        target_kind: query.targetKind,
        target_id: query.targetId,
        correlation_id: query.correlationId,
        since: query.since,
        until: query.until,
        limit: query.limit,
        cursor: query.cursor,
      },
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async verifyAuditChain(
    organizationId: string,
    limit: number = 100,
    options?: RequestOptions | undefined
  ): Promise<AuditVerificationResult> {
    const page = await this.listAuditRecords(organizationId, { limit }, options);
    return verifyAuditHashChain(page.items);
  }

  /* -------------------------------------------------------------------------- */
  /*                              USERS / MEMBERS                               */
  /* -------------------------------------------------------------------------- */

  public async listUsers(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<User[]> {
    return this.request<User[]>(`/v1/organizations/${organizationId}/members`, {
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async inviteUser(
    organizationId: string,
    email: string,
    role: string,
    options?: IdempotentRequestOptions | undefined
  ): Promise<User> {
    return this.request<User>(`/v1/organizations/${organizationId}/members/invite`, {
      method: "POST",
      body: { email, role },
      idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async removeUser(
    organizationId: string,
    userId: string,
    options?: RequestOptions | undefined
  ): Promise<void> {
    return this.request<void>(
      `/v1/organizations/${organizationId}/members/${userId}`,
      {
        method: "DELETE",
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  /* -------------------------------------------------------------------------- */
  /*                            API KEYS & DAEMONS                              */
  /* -------------------------------------------------------------------------- */

  public async listApiKeys(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<ApiKey[]> {
    return this.request<ApiKey[]>(`/v1/organizations/${organizationId}/api-keys`, {
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async createApiKey(
    organizationId: string,
    data: CreateApiKeyRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<ApiKeyCreatedResponse> {
    return this.request<ApiKeyCreatedResponse>(
      `/v1/organizations/${organizationId}/api-keys`,
      {
        method: "POST",
        body: data,
        idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  public async revokeApiKey(
    organizationId: string,
    apiKeyId: string,
    options?: RequestOptions | undefined
  ): Promise<void> {
    return this.request<void>(
      `/v1/organizations/${organizationId}/api-keys/${apiKeyId}`,
      {
        method: "DELETE",
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  public async listDaemons(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<Daemon[]> {
    return this.request<Daemon[]>(`/v1/organizations/${organizationId}/daemons`, {
      headers: options?.headers,
      signal: options?.signal,
    });
  }

  public async revokeDaemon(
    organizationId: string,
    daemonId: string,
    reason?: string | undefined,
    options?: IdempotentRequestOptions | undefined
  ): Promise<void> {
    const bodyObj = reason !== undefined ? { reason } : {};
    return this.request<void>(
      `/v1/organizations/${organizationId}/daemons/${daemonId}/revoke`,
      {
        method: "POST",
        body: bodyObj,
        idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  /* -------------------------------------------------------------------------- */
  /*                             PUBLISHED OBJECTS                              */
  /* -------------------------------------------------------------------------- */

  public async initiateObjectUpload(
    organizationId: string,
    data: ObjectUploadRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<ObjectUploadReceipt> {
    return this.request<ObjectUploadReceipt>(
      `/v1/organizations/${organizationId}/objects/upload`,
      {
        method: "POST",
        body: data,
        idempotencyKey: options?.idempotencyKey ?? generateIdempotencyKey(),
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }

  public async getPresignedDownloadUrl(
    organizationId: string,
    objectId: string,
    options?: RequestOptions | undefined
  ): Promise<PresignedUrlResponse> {
    return this.request<PresignedUrlResponse>(
      `/v1/organizations/${organizationId}/objects/${objectId}/download`,
      {
        headers: options?.headers,
        signal: options?.signal,
      }
    );
  }
}
