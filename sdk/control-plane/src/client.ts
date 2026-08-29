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
  AddOrganizationMemberRequest,
  AddOrganizationMemberResponse,
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
  PublishedObject,
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
import {
  parseApiError,
  ControlPlaneError,
  UnsupportedControlPlaneCapabilityError,
  ValidationError,
} from "./errors.js";
import { verifyAuditHashChain } from "./utils/audit-verifier.js";
import type {
  AuthTokenResponse as WireAuthTokenResponse,
  RefreshTokenRequest as WireRefreshTokenRequest,
  Repository as WireRepository,
  SharedSession as WireSharedSession,
  AuditRecord as WireAuditRecord,
  AuditQuery as WireAuditQuery,
  InitiatePairingRequest as WireInitiatePairingRequest,
  InitiatePairingResponse as WireInitiatePairingResponse,
  PublishedObject as WirePublishedObject,
} from "./generated/index.js";
import {
  auditRecordFromWire,
  authTokensFromWire,
  createOrganizationToWire,
  organizationFromWire,
  publishedObjectFromWire,
  registerRepositoryToWire,
  repositoryFromWire,
  sharedSessionFromWire,
  type OrganizationWireResponse,
} from "./wire-adapters.js";

interface PresignWireRequest {
  key: string;
  method: "GET";
  expiry_secs?: number | undefined;
}

interface PresignWireResponse {
  url: string;
  key: string;
  method: "GET";
}

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
      rawBody?: BodyInit | undefined;
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

    if (options.body !== undefined && options.rawBody !== undefined) {
      throw new TypeError("request body cannot be both JSON and raw bytes");
    }

    let requestBody: BodyInit | undefined;
    if (options.body !== undefined) {
      headers["Content-Type"] = "application/json";
      requestBody = JSON.stringify(options.body);
    } else if (options.rawBody !== undefined) {
      requestBody = options.rawBody;
    }

    let abortSignal = options.signal;
    if (!abortSignal && this.timeoutMs > 0 && typeof AbortSignal !== "undefined" && typeof AbortSignal.timeout === "function") {
      abortSignal = AbortSignal.timeout(this.timeoutMs);
    }

    const initObj: RequestInit = {
      method: options.method ?? "GET",
      headers,
    };
    if (requestBody !== undefined) {
      initObj.body = requestBody;
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

  private unsupported<T>(capability: string): Promise<T> {
    return Promise.reject(new UnsupportedControlPlaneCapabilityError(capability));
  }

  /* -------------------------------------------------------------------------- */
  /*                                    AUTH                                    */
  /* -------------------------------------------------------------------------- */

  public async getGitHubLoginUrl(redirectUri?: string | undefined): Promise<LoginResponse> {
    void redirectUri;
    return this.unsupported("interactive GitHub login");
  }

  public async getOidcLoginUrl(redirectUri?: string | undefined): Promise<LoginResponse> {
    void redirectUri;
    return this.unsupported("interactive OIDC login");
  }

  public async handleOAuthCallback(data: OAuthCallbackRequest): Promise<AuthSession> {
    void data;
    return this.unsupported("OAuth callback exchange");
  }

  public async refreshToken(data: RefreshTokenRequest): Promise<AuthTokens> {
    const body: WireRefreshTokenRequest = { refresh_token: data.refreshToken };
    const wire = await this.request<WireAuthTokenResponse>("/v1/auth/refresh", {
      method: "POST",
      body,
    });
    const tokens = authTokensFromWire(wire);
    if (tokens.accessToken) {
      this.setToken(tokens.accessToken);
    }
    if (this.onTokenRefresh) {
      this.onTokenRefresh(tokens);
    }
    return tokens;
  }

  public async getCurrentUser(options?: RequestOptions | undefined): Promise<User> {
    void options;
    return this.unsupported("current-user lookup");
  }

  public async logout(options?: RequestOptions | undefined): Promise<void> {
    void options;
    // The server has no logout/revocation route. Clearing the local credential
    // is still useful and is the only honest behavior this method can provide.
    this.setToken(null);
    this.setApiKey(null);
  }

  public async createPairingChallenge(
    data: CreatePairingChallengeRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<PairingChallenge> {
    const body: WireInitiatePairingRequest = {
      organization_id: data.organizationId,
      requested_scope: {
        max_publication_class: data.maxPublicationClass ?? "metadata-shared",
        accepts_remote_approvals: data.acceptsRemoteApprovals ?? false,
        accepts_runner_dispatch: data.acceptsRunnerDispatch ?? false,
      },
    };
    const wire = await this.request<WireInitiatePairingResponse>(
      "/v1/auth/pairing/challenge",
      {
        method: "POST",
        body,
        // The current server does not consume this header yet. A caller may
        // still pass one for forward compatibility, but the SDK must not imply
        // an idempotency guarantee by silently inventing it.
        idempotencyKey: options?.idempotencyKey,
        headers: options?.headers,
        signal: options?.signal,
      },
    );
    return {
      code: wire.challenge_code,
      organizationId: data.organizationId,
      requestedScope: {
        maxPublicationClass: data.maxPublicationClass ?? "metadata-shared",
        acceptsRemoteApprovals: data.acceptsRemoteApprovals ?? false,
        acceptsRunnerDispatch: data.acceptsRunnerDispatch ?? false,
      },
      expiresAt: wire.expires_at,
    };
  }

  /* -------------------------------------------------------------------------- */
  /*                                ORGANIZATIONS                               */
  /* -------------------------------------------------------------------------- */

  public async listOrganizations(options?: RequestOptions | undefined): Promise<Organization[]> {
    const wire = await this.request<OrganizationWireResponse[]>("/v1/organizations", {
      headers: options?.headers,
      signal: options?.signal,
    });
    return wire.map(organizationFromWire);
  }

  public async getOrganization(id: string, options?: RequestOptions | undefined): Promise<Organization> {
    const wire = await this.request<OrganizationWireResponse>(`/v1/organizations/${id}`, {
      headers: options?.headers,
      signal: options?.signal,
    });
    return organizationFromWire(wire);
  }

  public async createOrganization(
    data: CreateOrganizationRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<Organization> {
    if (data.dataResidency !== undefined || data.retentionDays !== undefined) {
      throw new UnsupportedControlPlaneCapabilityError(
        "organization data-residency and retention settings during creation",
      );
    }
    const wire = await this.request<OrganizationWireResponse>("/v1/organizations", {
      method: "POST",
      body: createOrganizationToWire(data),
      idempotencyKey: options?.idempotencyKey,
      headers: options?.headers,
      signal: options?.signal,
    });
    return organizationFromWire(wire);
  }

  public async updateOrganizationPolicy(
    id: string,
    data: UpdateOrganizationPolicyRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<Organization> {
    void id;
    void data;
    void options;
    return this.unsupported("organization policy updates");
  }

  public async deleteOrganization(id: string, options?: RequestOptions | undefined): Promise<void> {
    void id;
    void options;
    return this.unsupported("organization deletion");
  }

  public async addOrganizationMember(
    organizationId: string,
    data: AddOrganizationMemberRequest,
    options?: IdempotentRequestOptions | undefined,
  ): Promise<AddOrganizationMemberResponse> {
    return this.request<AddOrganizationMemberResponse>(
      `/v1/organizations/${organizationId}/members`,
      {
        method: "POST",
        body: { user_id: data.userId, role: data.role },
        idempotencyKey: options?.idempotencyKey,
        headers: options?.headers,
        signal: options?.signal,
      },
    );
  }

  /* -------------------------------------------------------------------------- */
  /*                                    TEAMS                                   */
  /* -------------------------------------------------------------------------- */

  public async listTeams(organizationId: string, options?: RequestOptions | undefined): Promise<Team[]> {
    void organizationId;
    void options;
    return this.unsupported("team listing");
  }

  public async getTeam(organizationId: string, teamId: string, options?: RequestOptions | undefined): Promise<Team> {
    void organizationId;
    void teamId;
    void options;
    return this.unsupported("team lookup");
  }

  public async createTeam(
    organizationId: string,
    data: CreateTeamRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<Team> {
    void organizationId;
    void data;
    void options;
    return this.unsupported("team creation");
  }

  public async listTeamMembers(
    organizationId: string,
    teamId: string,
    options?: RequestOptions | undefined
  ): Promise<TeamMember[]> {
    void organizationId;
    void teamId;
    void options;
    return this.unsupported("team member listing");
  }

  public async addTeamMember(
    organizationId: string,
    teamId: string,
    data: AddTeamMemberRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<TeamMember> {
    void organizationId;
    void teamId;
    void data;
    void options;
    return this.unsupported("team member addition");
  }

  public async removeTeamMember(
    organizationId: string,
    teamId: string,
    userId: string,
    options?: RequestOptions | undefined
  ): Promise<void> {
    void organizationId;
    void teamId;
    void userId;
    void options;
    return this.unsupported("team member removal");
  }

  /* -------------------------------------------------------------------------- */
  /*                                REPOSITORIES                                */
  /* -------------------------------------------------------------------------- */

  public async listRepositories(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<Repository[]> {
    const wire = await this.request<WireRepository[]>(`/v1/organizations/${organizationId}/repositories`, {
      headers: options?.headers,
      signal: options?.signal,
    });
    return wire.map(repositoryFromWire);
  }

  public async getRepository(
    organizationId: string,
    repositoryId: string,
    options?: RequestOptions | undefined
  ): Promise<Repository> {
    const wire = await this.request<WireRepository>(
      `/v1/organizations/${organizationId}/repositories/${repositoryId}`,
      {
        headers: options?.headers,
        signal: options?.signal,
      }
    );
    return repositoryFromWire(wire);
  }

  public async registerRepository(
    organizationId: string,
    data: RegisterRepositoryRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<Repository> {
    const wire = await this.request<WireRepository>(`/v1/organizations/${organizationId}/repositories`, {
      method: "POST",
      body: registerRepositoryToWire(data),
      idempotencyKey: options?.idempotencyKey,
      headers: options?.headers,
      signal: options?.signal,
    });
    return repositoryFromWire(wire);
  }

  public async updateRepositoryPolicy(
    organizationId: string,
    repositoryId: string,
    data: UpdateRepositoryPolicyRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<Repository> {
    void organizationId;
    void repositoryId;
    void data;
    void options;
    return this.unsupported("repository policy updates");
  }

  public async deleteRepository(
    organizationId: string,
    repositoryId: string,
    options?: RequestOptions | undefined
  ): Promise<void> {
    void organizationId;
    void repositoryId;
    void options;
    return this.unsupported("repository deletion");
  }

  /* -------------------------------------------------------------------------- */
  /*                                 ROLE GRANTS                                */
  /* -------------------------------------------------------------------------- */

  public async listRoleGrants(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<RoleGrant[]> {
    void organizationId;
    void options;
    return this.unsupported("role-grant listing");
  }

  public async grantRole(
    organizationId: string,
    data: GrantRoleRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<RoleGrant> {
    void organizationId;
    void data;
    void options;
    return this.unsupported("role granting");
  }

  public async revokeRoleGrant(
    organizationId: string,
    grantId: string,
    options?: RequestOptions | undefined
  ): Promise<void> {
    void organizationId;
    void grantId;
    void options;
    return this.unsupported("role-grant revocation");
  }

  /* -------------------------------------------------------------------------- */
  /*                              SESSIONS & RUNS                               */
  /* -------------------------------------------------------------------------- */

  public async listSharedSessions(
    organizationId: string,
    query: SessionListQuery = {},
    options?: RequestOptions | undefined
  ): Promise<SessionListPage> {
    if (!query.repositoryId) {
      throw new ValidationError("repositoryId is required when listing shared sessions");
    }
    if (
      query.state !== undefined ||
      query.since !== undefined ||
      query.until !== undefined ||
      query.search !== undefined ||
      query.cursor !== undefined ||
      query.direction !== undefined
    ) {
      throw new UnsupportedControlPlaneCapabilityError(
        "shared-session filtering or pagination beyond repositoryId and limit",
      );
    }
    const wire = await this.request<WireSharedSession[]>(
      `/v1/organizations/${organizationId}/sessions`,
      {
        params: {
          repository_id: query.repositoryId,
          limit: query.limit,
        },
        headers: options?.headers,
        signal: options?.signal,
      }
    );
    const items = wire.map(sharedSessionFromWire);
    return { items, cursor: null, hasMore: false, total: items.length };
  }

  public async getSharedSession(
    organizationId: string,
    sessionId: string,
    options?: RequestOptions | undefined
  ): Promise<SharedSessionDetail> {
    void organizationId;
    void sessionId;
    void options;
    return this.unsupported("shared-session detail lookup");
  }

  /* -------------------------------------------------------------------------- */
  /*                                    INBOX                                   */
  /* -------------------------------------------------------------------------- */

  public async listInbox(
    organizationId: string,
    query: InboxListQuery = {},
    options?: RequestOptions | undefined
  ): Promise<InboxPage> {
    void organizationId;
    void query;
    void options;
    return this.unsupported("inbox listing");
  }

  public async mutateInbox(
    organizationId: string,
    entryId: string,
    data: InboxMutationRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<void> {
    void organizationId;
    void entryId;
    void data;
    void options;
    return this.unsupported("inbox mutation");
  }

  /* -------------------------------------------------------------------------- */
  /*                                  APPROVALS                                 */
  /* -------------------------------------------------------------------------- */

  public async listPendingApprovals(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<PendingApproval[]> {
    void organizationId;
    void options;
    return this.unsupported("approval listing");
  }

  public async decideApproval(
    organizationId: string,
    approvalId: string,
    data: ApprovalDecisionRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<ApprovalDecisionResponse> {
    void organizationId;
    void approvalId;
    void data;
    void options;
    return this.unsupported("approval decisions");
  }

  /* -------------------------------------------------------------------------- */
  /*                                 AUDIT LOGS                                 */
  /* -------------------------------------------------------------------------- */

  public async listAuditRecords(
    organizationId: string,
    query: AuditQuery = {},
    options?: RequestOptions | undefined
  ): Promise<AuditPage> {
    if (
      query.actorKind !== undefined ||
      query.actorId !== undefined ||
      query.action !== undefined ||
      query.targetKind !== undefined ||
      query.targetId !== undefined ||
      query.correlationId !== undefined ||
      query.since !== undefined ||
      query.until !== undefined ||
      query.cursor !== undefined ||
      query.direction !== undefined
    ) {
      throw new UnsupportedControlPlaneCapabilityError(
        "audit filtering or pagination beyond limit",
      );
    }
    const params: Pick<WireAuditQuery, "limit"> =
      query.limit === undefined ? {} : { limit: query.limit };
    const wire = await this.request<WireAuditRecord[]>(`/v1/organizations/${organizationId}/audit`, {
      params,
      headers: options?.headers,
      signal: options?.signal,
    });
    const items = wire.map(auditRecordFromWire);
    return { items, cursor: null, hasMore: false, total: items.length };
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
    void organizationId;
    void options;
    return this.unsupported("organization member listing");
  }

  public async inviteUser(
    organizationId: string,
    email: string,
    role: string,
    options?: IdempotentRequestOptions | undefined
  ): Promise<User> {
    void organizationId;
    void email;
    void role;
    void options;
    return this.unsupported("organization member invitation by email");
  }

  public async removeUser(
    organizationId: string,
    userId: string,
    options?: RequestOptions | undefined
  ): Promise<void> {
    void organizationId;
    void userId;
    void options;
    return this.unsupported("organization member removal");
  }

  /* -------------------------------------------------------------------------- */
  /*                            API KEYS & DAEMONS                              */
  /* -------------------------------------------------------------------------- */

  public async listApiKeys(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<ApiKey[]> {
    void organizationId;
    void options;
    return this.unsupported("API-key listing");
  }

  public async createApiKey(
    organizationId: string,
    data: CreateApiKeyRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<ApiKeyCreatedResponse> {
    void organizationId;
    void data;
    void options;
    return this.unsupported("API-key creation");
  }

  public async revokeApiKey(
    organizationId: string,
    apiKeyId: string,
    options?: RequestOptions | undefined
  ): Promise<void> {
    void organizationId;
    void apiKeyId;
    void options;
    return this.unsupported("API-key revocation");
  }

  public async listDaemons(
    organizationId: string,
    options?: RequestOptions | undefined
  ): Promise<Daemon[]> {
    void organizationId;
    void options;
    return this.unsupported("daemon listing");
  }

  public async revokeDaemon(
    organizationId: string,
    daemonId: string,
    reason?: string | undefined,
    options?: IdempotentRequestOptions | undefined
  ): Promise<void> {
    void organizationId;
    void daemonId;
    void reason;
    void options;
    return this.unsupported("daemon revocation");
  }

  /* -------------------------------------------------------------------------- */
  /*                             PUBLISHED OBJECTS                              */
  /* -------------------------------------------------------------------------- */

  public async initiateObjectUpload(
    organizationId: string,
    data: ObjectUploadRequest,
    options?: IdempotentRequestOptions | undefined
  ): Promise<ObjectUploadReceipt> {
    return this.uploadObject(organizationId, data, options);
  }

  /** Upload bytes through the server's verified, content-addressed write path. */
  public async uploadObject(
    organizationId: string,
    data: ObjectUploadRequest,
    options?: IdempotentRequestOptions | undefined,
  ): Promise<PublishedObject> {
    const headers: Record<string, string> = { ...options?.headers };
    headers["Content-Type"] = data.mediaType ?? "application/octet-stream";
    if (data.expectedContentHash !== undefined) {
      headers["X-Content-Sha256"] = data.expectedContentHash;
    }
    const wire = await this.request<WirePublishedObject>(
      `/v1/organizations/${organizationId}/objects/upload`,
      {
        method: "POST",
        rawBody: data.body,
        idempotencyKey: options?.idempotencyKey,
        headers,
        signal: options?.signal,
      },
    );
    return publishedObjectFromWire(wire);
  }

  public async getPresignedDownloadUrl(
    organizationId: string,
    contentHash: string,
    options?: RequestOptions | undefined
  ): Promise<PresignedUrlResponse> {
    const body: PresignWireRequest = { key: contentHash, method: "GET" };
    return this.request<PresignWireResponse>(
      `/v1/organizations/${organizationId}/objects/presign`,
      {
        method: "POST",
        body,
        headers: options?.headers,
        signal: options?.signal,
      },
    );
  }

  public async getObjectMetadata(
    organizationId: string,
    contentHash: string,
    options?: RequestOptions | undefined,
  ): Promise<PublishedObject> {
    const wire = await this.request<WirePublishedObject>(
      `/v1/organizations/${organizationId}/objects/${contentHash}/metadata`,
      {
        headers: options?.headers,
        signal: options?.signal,
      },
    );
    return publishedObjectFromWire(wire);
  }
}
