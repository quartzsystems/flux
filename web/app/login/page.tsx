'use client';

/**
 * The login page.
 *
 * Deliberately outside the app shell: it is the one route that must render
 * without a session. It also redirects away on its own if a session already
 * exists, so following a bookmarked `/login` while signed in lands on the
 * dashboard rather than asking for a password that is not needed.
 */

import { useMutation, useQueryClient } from '@tanstack/react-query';
import { IconAlertTriangle, IconLock, IconUser } from '@tabler/icons-react';
import { useRouter } from 'next/navigation';
import { useEffect, useState, type FormEvent } from 'react';

import { FluxLockup, TAGLINE } from '@/components/Brand';
import { Alert } from '@/components/ui';
import { ApiError, api, queryKeys } from '@/lib/api';
import { useAuth } from '@/lib/auth';

export default function LoginPage() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { user, loading } = useAuth();

  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');

  // Someone who is already signed in has no business on this page.
  useEffect(() => {
    if (!loading && user) {
      router.replace('/');
    }
  }, [loading, user, router]);

  const login = useMutation({
    mutationFn: () => api.auth.login({ username, password }),
    onSuccess: (me) => {
      // Seed the cache so the shell does not flash a skeleton while it refetches
      // an answer we already have.
      queryClient.setQueryData(queryKeys.me, me);
      router.replace('/');
    },
  });

  const onSubmit = (event: FormEvent) => {
    event.preventDefault();
    if (!login.isPending) login.mutate();
  };

  return (
    <div className="login-page">
      <div className="login-box">
        <div className="login-brand">
          <FluxLockup width={200} />
          <p className="login-tagline">{TAGLINE}</p>
        </div>

        <section className="surface">
          <form className="surface-body stack gap-14" onSubmit={onSubmit}>
            {login.error ? (
              <Alert tone="danger">
                <IconAlertTriangle size={16} stroke={1.8} />
                <span>{loginMessage(login.error)}</span>
              </Alert>
            ) : null}

            <label className="field">
              <span className="field-label">
                <IconUser size={13} stroke={1.8} style={{ verticalAlign: -2, marginRight: 5 }} />
                Username
              </span>
              <input
                className="input"
                name="username"
                autoComplete="username"
                autoFocus
                required
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                disabled={login.isPending}
              />
            </label>

            <label className="field">
              <span className="field-label">
                <IconLock size={13} stroke={1.8} style={{ verticalAlign: -2, marginRight: 5 }} />
                Password
              </span>
              <input
                className="input"
                name="password"
                type="password"
                autoComplete="current-password"
                required
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                disabled={login.isPending}
              />
            </label>

            <button
              type="submit"
              className="btn btn-primary"
              disabled={login.isPending || !username || !password}
            >
              {login.isPending ? 'Signing in…' : 'Sign in'}
            </button>
          </form>
        </section>
      </div>
    </div>
  );
}

/**
 * The message shown for a failed login.
 *
 * A 401 is reported as one generic line regardless of whether the username or
 * the password was wrong — the API deliberately does not distinguish them, and
 * saying more here would leak what it withheld.
 */
function loginMessage(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.isUnauthorized) return 'Incorrect username or password.';
    if (error.code === 'network') return 'Could not reach the appliance.';
    return error.message;
  }
  return 'Sign-in failed.';
}
