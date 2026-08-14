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
import { Eye, EyeOff } from 'lucide-react';
import { useRouter } from 'next/navigation';
import { useEffect, useState, type FormEvent } from 'react';

import { FluxMark, TAGLINE } from '@/components/Brand';
import { ApiError, api, queryKeys } from '@/lib/api';
import { useAuth } from '@/lib/auth';

export default function LoginPage() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { user, loading } = useAuth();

  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [showPassword, setShowPassword] = useState(false);

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
        <div className="login-card">
          <div className="login-brand">
            <FluxMark size={36} />
            <span
              style={{
                fontSize: 22,
                fontWeight: 700,
                color: 'var(--qz-fg-1)',
                letterSpacing: '-0.02em',
              }}
            >
              Flux
            </span>
          </div>

          <form className="stack gap-12" onSubmit={onSubmit}>
            <label className="field">
              <span className="field-label">Username</span>
              <input
                className="input"
                name="username"
                placeholder="Enter your username"
                autoComplete="username"
                autoFocus
                required
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                disabled={login.isPending}
              />
            </label>

            <label className="field">
              <span className="field-label">Password</span>
              <div className="relative">
                <input
                  className="input pr-10"
                  name="password"
                  type={showPassword ? 'text' : 'password'}
                  placeholder="Enter your password"
                  autoComplete="current-password"
                  required
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  disabled={login.isPending}
                />
                <button
                  type="button"
                  onClick={() => setShowPassword((v) => !v)}
                  aria-label={showPassword ? 'Hide password' : 'Show password'}
                  className="absolute right-3 top-1/2 -translate-y-1/2 text-[var(--qz-fg-4)] hover:text-[var(--qz-fg-2)] transition-colors cursor-pointer bg-transparent border-0 p-0"
                >
                  {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                </button>
              </div>
            </label>

            {login.error ? (
              <p className="field-error m-0" role="alert">
                {loginMessage(login.error)}
              </p>
            ) : null}

            <button
              type="submit"
              className="btn btn-primary w-full"
              disabled={login.isPending || !username || !password}
            >
              {login.isPending ? 'Signing in…' : 'Sign in'}
            </button>
          </form>

          <p className="login-tagline">{TAGLINE}</p>
        </div>
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
