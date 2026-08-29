import React, {
  createContext,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  ControlPlaneClient,
  ControlPlaneStreamClient,
  type Organization,
  type Repository,
  type Team,
  type User,
} from "@codypendent/control-plane";

export interface ControlPlaneContextValue {
  client: ControlPlaneClient;
  streamClient: ControlPlaneStreamClient;

  // Active Scope
  organizations: Organization[];
  activeOrganization: Organization | null;
  activeOrganizationId: string | null;
  setActiveOrganizationId: (id: string | null) => void;

  teams: Team[];
  activeTeam: Team | null;
  activeTeamId: string | null;
  setActiveTeamId: (id: string | null) => void;

  repositories: Repository[];
  activeRepository: Repository | null;
  activeRepositoryId: string | null;
  setActiveRepositoryId: (id: string | null) => void;

  // Auth State
  currentUser: User | null;
  isAuthenticated: boolean;
  setAuthToken: (token: string | null) => void;
  /// The access token currently in use, or null when there is none.
  ///
  /// Exposed so a host application can persist it across reloads. Without it
  /// the only way to observe a token is to have set it, which misses the one
  /// that matters most: the rotated token `onTokenRefresh` installs.
  token: string | null;

  // Refresh Helpers
  refreshOrganizations: () => Promise<void>;
  refreshTeams: () => Promise<void>;
  refreshRepositories: () => Promise<void>;
  refreshCurrentUser: () => Promise<void>;

  isLoading: boolean;
  error: Error | null;
}

export const ControlPlaneContext = createContext<ControlPlaneContextValue | null>(null);

export interface ControlPlaneProviderProps {
  children: ReactNode;
  client?: ControlPlaneClient | undefined;
  streamClient?: ControlPlaneStreamClient | undefined;
  baseUrl?: string | undefined;
  token?: string | null | undefined;
  apiKey?: string | null | undefined;
  initialOrganizationId?: string | undefined;
  initialTeamId?: string | undefined;
  initialRepositoryId?: string | undefined;
  onAuthError?: ((error: Error) => void) | undefined;
}

export function ControlPlaneProvider({
  children,
  client: customClient,
  streamClient: customStreamClient,
  baseUrl = "http://localhost:8080",
  token: initialToken = null,
  apiKey: initialApiKey = null,
  initialOrganizationId,
  initialTeamId,
  initialRepositoryId,
  onAuthError,
}: ControlPlaneProviderProps): React.JSX.Element {
  const [token, setTokenState] = useState<string | null>(initialToken);
  const [apiKey, setApiKeyState] = useState<string | null>(initialApiKey);

  const client = useMemo(() => {
    if (customClient) return customClient;
    return new ControlPlaneClient({
      baseUrl,
      token,
      apiKey,
      onTokenRefresh: (tokens) => {
        setTokenState(tokens.accessToken);
      },
    });
  }, [customClient, baseUrl, token, apiKey]);

  const streamClient = useMemo(() => {
    if (customStreamClient) return customStreamClient;
    return new ControlPlaneStreamClient({
      baseUrl,
      token,
      apiKey,
    });
  }, [customStreamClient, baseUrl, token, apiKey]);

  const [organizations, setOrganizations] = useState<Organization[]>([]);
  const [activeOrganizationId, setActiveOrganizationIdState] = useState<string | null>(
    initialOrganizationId ?? null
  );

  const [teams, setTeams] = useState<Team[]>([]);
  const [activeTeamId, setActiveTeamId] = useState<string | null>(initialTeamId ?? null);

  const [repositories, setRepositories] = useState<Repository[]>([]);
  const [activeRepositoryId, setActiveRepositoryId] = useState<string | null>(
    initialRepositoryId ?? null
  );

  const [currentUser, setCurrentUser] = useState<User | null>(null);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const [error, setError] = useState<Error | null>(null);
  // Every scope-dependent read is generation-qualified. Switching orgs while
  // the previous tenant's request is in flight must never let that late reply
  // repopulate the new tenant's selectors.
  const organizationRequest = useRef(0);
  const teamRequest = useRef(0);
  const repositoryRequest = useRef(0);
  const currentUserRequest = useRef(0);
  const activeOrganizationIdRef = useRef<string | null>(initialOrganizationId ?? null);

  const setActiveOrganizationId = useCallback((id: string | null) => {
    // Update the request authority synchronously with the user's choice. A
    // promise can settle before React runs the dependent effect, so generation
    // checks alone otherwise leave a one-render cross-tenant race window.
    activeOrganizationIdRef.current = id;
    teamRequest.current += 1;
    repositoryRequest.current += 1;
    setActiveOrganizationIdState(id);
    setTeams([]);
    setRepositories([]);
    setActiveTeamId(null);
    setActiveRepositoryId(null);
  }, []);

  const setAuthToken = useCallback(
    (newToken: string | null) => {
      currentUserRequest.current += 1;
      organizationRequest.current += 1;
      teamRequest.current += 1;
      repositoryRequest.current += 1;
      setTokenState(newToken);
      client.setToken(newToken);
      streamClient.setToken(newToken);
      if (!newToken) {
        // `setAuthToken(null)` is the provider's public sign-out primitive.
        // Clear an initial API key as well so the context, HTTP client and
        // stream client cannot disagree about whether logout succeeded.
        setApiKeyState(null);
        client.setApiKey(null);
        streamClient.setApiKey(null);
        setCurrentUser(null);
        setOrganizations([]);
        setActiveOrganizationId(null);
      }
    },
    [client, streamClient, setActiveOrganizationId]
  );

  const refreshCurrentUser = useCallback(async () => {
    const request = ++currentUserRequest.current;
    if (!token && !apiKey) {
      setCurrentUser(null);
      return;
    }
    try {
      const user = await client.getCurrentUser();
      if (request !== currentUserRequest.current) return;
      setCurrentUser(user);
    } catch (err) {
      if (request !== currentUserRequest.current) return;
      setCurrentUser(null);
      onAuthError?.(err as Error);
    }
  }, [client, token, apiKey, onAuthError]);

  const refreshOrganizations = useCallback(async () => {
    const request = ++organizationRequest.current;
    try {
      const orgs = await client.listOrganizations();
      if (request !== organizationRequest.current) return;
      setOrganizations(orgs);
      if (orgs.length > 0 && !activeOrganizationIdRef.current) {
        setActiveOrganizationId(orgs[0].id);
      }
    } catch (err) {
      if (request !== organizationRequest.current) return;
      setError(err as Error);
    }
  }, [client, setActiveOrganizationId]);

  const refreshTeams = useCallback(async () => {
    const request = ++teamRequest.current;
    const organizationId = activeOrganizationId;
    if (!organizationId) {
      setTeams([]);
      setActiveTeamId(null);
      return;
    }
    try {
      const teamList = await client.listTeams(organizationId);
      if (
        request !== teamRequest.current ||
        organizationId !== activeOrganizationIdRef.current
      )
        return;
      setTeams(teamList);
      if (teamList.length > 0) {
        setActiveTeamId((currentId) => {
          if (!currentId || !teamList.some((t) => t.id === currentId)) {
            return teamList[0].id;
          }
          return currentId;
        });
      } else {
        setActiveTeamId(null);
      }
    } catch {
      if (
        request !== teamRequest.current ||
        organizationId !== activeOrganizationIdRef.current
      )
        return;
      setTeams([]);
      setActiveTeamId(null);
    }
  }, [client, activeOrganizationId]);

  const refreshRepositories = useCallback(async () => {
    const request = ++repositoryRequest.current;
    const organizationId = activeOrganizationId;
    if (!organizationId) {
      setRepositories([]);
      setActiveRepositoryId(null);
      return;
    }
    try {
      const repos = await client.listRepositories(organizationId);
      if (
        request !== repositoryRequest.current ||
        organizationId !== activeOrganizationIdRef.current
      )
        return;
      setRepositories(repos);
      if (repos.length > 0) {
        setActiveRepositoryId((currentId) => {
          if (!currentId || !repos.some((r) => r.id === currentId)) {
            return repos[0].id;
          }
          return currentId;
        });
      } else {
        setActiveRepositoryId(null);
      }
    } catch {
      if (
        request !== repositoryRequest.current ||
        organizationId !== activeOrganizationIdRef.current
      )
        return;
      setRepositories([]);
      setActiveRepositoryId(null);
    }
  }, [client, activeOrganizationId]);

  // Initial load
  useEffect(() => {
    let mounted = true;
    const init = async () => {
      setIsLoading(true);
      setError(null);
      try {
        await refreshCurrentUser();
        await refreshOrganizations();
      } catch (err) {
        if (mounted) setError(err as Error);
      } finally {
        if (mounted) setIsLoading(false);
      }
    };
    init();
    return () => {
      mounted = false;
    };
  }, [refreshCurrentUser, refreshOrganizations]);

  // When active organization changes, refresh teams and repositories
  useEffect(() => {
    if (activeOrganizationId) {
      refreshTeams();
      refreshRepositories();
    } else {
      setTeams([]);
      setRepositories([]);
      setActiveTeamId(null);
      setActiveRepositoryId(null);
    }
  }, [activeOrganizationId, refreshTeams, refreshRepositories]);

  const activeOrganization = useMemo(
    () => organizations.find((o) => o.id === activeOrganizationId) ?? null,
    [organizations, activeOrganizationId]
  );

  const activeTeam = useMemo(
    () => teams.find((t) => t.id === activeTeamId) ?? null,
    [teams, activeTeamId]
  );

  const activeRepository = useMemo(
    () => repositories.find((r) => r.id === activeRepositoryId) ?? null,
    [repositories, activeRepositoryId]
  );

  const value = useMemo<ControlPlaneContextValue>(
    () => ({
      client,
      streamClient,
      organizations,
      activeOrganization,
      activeOrganizationId,
      setActiveOrganizationId,
      teams,
      activeTeam,
      activeTeamId,
      setActiveTeamId,
      repositories,
      activeRepository,
      activeRepositoryId,
      setActiveRepositoryId,
      currentUser,
      // Credentials gate protected routes. Current-user lookup enriches the
      // header when supported, but the server intentionally has no `/me`
      // route yet and that capability gap must not discard a bearer session.
      isAuthenticated: !!token || !!apiKey,
      setAuthToken,
      token,
      refreshOrganizations,
      refreshTeams,
      refreshRepositories,
      refreshCurrentUser,
      isLoading,
      error,
    }),
    [
      client,
      streamClient,
      organizations,
      activeOrganization,
      activeOrganizationId,
      teams,
      activeTeam,
      activeTeamId,
      repositories,
      activeRepository,
      activeRepositoryId,
      currentUser,
      apiKey,
      setAuthToken,
      token,
      refreshOrganizations,
      refreshTeams,
      refreshRepositories,
      refreshCurrentUser,
      isLoading,
      error,
    ]
  );

  return <ControlPlaneContext.Provider value={value}>{children}</ControlPlaneContext.Provider>;
}
