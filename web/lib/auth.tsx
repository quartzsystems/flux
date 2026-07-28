'use client';

/**
 * Client-side session state.
 *
 * The UI is a static export, so there is no server render that could know who is
 * signed in. Instead every mount asks `/auth/me` once, and that answer gates the
 * shell. The cookie itself is httpOnly and never readable here — the only way to
 * know whether a session is live is to ask the API, which is also the only
 * authority that matters.
 */

import { useQuery, useQueryClient } from '@tanstack/react-query';
import { usePathname, useRouter } from 'next/navigation';
import { createContext, useCallback, useContext, useEffect, type ReactNode } from 'react';

import { ApiError, api, queryKeys } from './api';
import { hasRole, type Me, type Role } from './api-types';

/** What `useAuth` exposes. */
interface AuthState {
  /** The signed-in account, or null when anonymous. */
  user: Me | null;
  /** True until the first `/auth/me` answer arrives. */
  loading: boolean;
  /** True when `user.role` meets `required`. */
  can: (required: Role) => boolean;
  /** Ends the session and returns to the login page. */
  logout: () => Promise<void>;
}

const AuthContext = createContext<AuthState | null>(null);

/** Provides session state to the tree. */
export function AuthProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const router = useRouter();

  const { data, isLoading } = useQuery({
    queryKey: queryKeys.me,
    queryFn: ({ signal }) => api.auth.me(signal),
    // A 401 is the expected answer for a signed-out visitor, not a transient
    // failure, so retrying it would only delay the redirect to /login.
    retry: (failureCount, error) =>
      !(error instanceof ApiError && error.isUnauthorized) && failureCount < 2,
    staleTime: 60_000,
  });

  const logout = useCallback(async () => {
    try {
      await api.auth.logout();
    } finally {
      // Clear the cache even if the request failed: the local session is over
      // either way, and leaving stale data behind would flash it to the next
      // person to sign in.
      queryClient.clear();
      router.replace('/login');
    }
  }, [queryClient, router]);

  const user = data ?? null;

  return (
    <AuthContext.Provider
      value={{
        user,
        loading: isLoading,
        can: (required: Role) => hasRole(user?.role, required),
        logout,
      }}
    >
      {children}
    </AuthContext.Provider>
  );
}

/** Reads session state. Throws when used outside the provider. */
export function useAuth(): AuthState {
  const context = useContext(AuthContext);
  if (!context) {
    throw new Error('useAuth must be used inside an AuthProvider');
  }
  return context;
}

/**
 * Sends anonymous visitors to the login page.
 *
 * Returns the resolved user, or null while the check is still running, so a
 * caller can render a skeleton rather than a flash of empty shell.
 */
export function useRequireAuth(): Me | null {
  const { user, loading } = useAuth();
  const router = useRouter();
  const pathname = usePathname();

  useEffect(() => {
    if (!loading && !user && pathname !== '/login') {
      router.replace('/login');
    }
  }, [loading, user, pathname, router]);

  return user;
}
