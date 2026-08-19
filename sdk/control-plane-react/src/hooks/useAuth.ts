import { useCallback, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type { OAuthCallbackRequest, User } from "@codypendent/control-plane";

export interface UseAuthResult {
  currentUser: User | null;
  isAuthenticated: boolean;
  isLoading: boolean;
  error: Error | null;
  loginWithGitHub: (redirectUri?: string | undefined) => Promise<string>;
  loginWithOidc: (redirectUri?: string | undefined) => Promise<string>;
  handleCallback: (params: OAuthCallbackRequest) => Promise<User>;
  logout: () => Promise<void>;
  setAuthToken: (token: string | null) => void;
  refreshCurrentUser: () => Promise<void>;
  /// The access token in use, for hosts that persist the session themselves.
  token: string | null;
}

export function useAuth(): UseAuthResult {
  const { client, currentUser, isAuthenticated, setAuthToken, refreshCurrentUser, token } =
    useControlPlaneContext();
  const [isLoading, setIsLoading] = useState<boolean>(false);
  const [error, setError] = useState<Error | null>(null);

  const loginWithGitHub = useCallback(
    async (redirectUri?: string | undefined) => {
      setIsLoading(true);
      setError(null);
      try {
        const res = await client.getGitHubLoginUrl(redirectUri);
        return res.authUrl;
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsLoading(false);
      }
    },
    [client]
  );

  const loginWithOidc = useCallback(
    async (redirectUri?: string | undefined) => {
      setIsLoading(true);
      setError(null);
      try {
        const res = await client.getOidcLoginUrl(redirectUri);
        return res.authUrl;
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsLoading(false);
      }
    },
    [client]
  );

  const handleCallback = useCallback(
    async (params: OAuthCallbackRequest) => {
      setIsLoading(true);
      setError(null);
      try {
        const session = await client.handleOAuthCallback(params);
        setAuthToken(session.tokens.accessToken);
        await refreshCurrentUser();
        return session.user;
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsLoading(false);
      }
    },
    [client, setAuthToken, refreshCurrentUser]
  );

  const logout = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      await client.logout();
      setAuthToken(null);
    } catch (err) {
      setError(err as Error);
      setAuthToken(null);
    } finally {
      setIsLoading(false);
    }
  }, [client, setAuthToken]);

  return {
    currentUser,
    isAuthenticated,
    isLoading,
    error,
    loginWithGitHub,
    loginWithOidc,
    handleCallback,
    logout,
    setAuthToken,
    refreshCurrentUser,
    token,
  };
}
