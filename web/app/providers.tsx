'use client';

/**
 * Client-side providers.
 *
 * Split out of the root layout so the layout itself stays a server component and
 * only this subtree ships the provider code.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useState, type ReactNode } from 'react';

import { ApiError } from '@/lib/api';
import { AuthProvider } from '@/lib/auth';

/** Builds the query client with the appliance's retry policy. */
function makeQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        // Never retry an authentication or authorisation failure: the answer
        // will not change without the operator doing something, and retrying
        // just delays the redirect to /login.
        retry: (failureCount, error) => {
          if (error instanceof ApiError && (error.status === 401 || error.status === 403)) {
            return false;
          }
          return failureCount < 2;
        },
        staleTime: 5_000,
        refetchOnWindowFocus: true,
      },
    },
  });
}

/** Wraps the app in its client-side providers. */
export function Providers({ children }: { children: ReactNode }) {
  // Created in state rather than at module scope so the client is per-mount;
  // a module-level client would be shared across tests and hot reloads.
  const [queryClient] = useState(makeQueryClient);

  return (
    <QueryClientProvider client={queryClient}>
      <AuthProvider>{children}</AuthProvider>
    </QueryClientProvider>
  );
}
