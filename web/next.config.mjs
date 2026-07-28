/**
 * Next.js configuration.
 *
 * The appliance serves this UI as a static export from `fluxd` itself — there is
 * no Node runtime on the box and no reverse proxy. That constrains two things:
 *
 *   - `output: 'export'` for the real build, which rules out server components
 *     that read request state, route handlers, and image optimisation.
 *   - `rewrites` cannot exist in an export, so the dev proxy to `fluxd` is
 *     applied only when running `next dev`. In production the UI and the API are
 *     the same origin, so no proxy is needed at all.
 *
 * `trailingSlash` makes the export emit `out/ports/index.html` rather than
 * `out/ports.html`, which is what lets `fluxd` serve it with a plain directory
 * index rather than probing for `.html` extensions.
 */

const isDev = process.env.NODE_ENV === 'development';

/** Where `fluxd` listens during development. */
const API_ORIGIN = process.env.FLUX_API_ORIGIN ?? 'http://127.0.0.1:8080';

/** @type {import('next').NextConfig} */
const config = {
  output: isDev ? undefined : 'export',
  trailingSlash: true,
  reactStrictMode: true,

  // The appliance has no image optimiser, so images ship as authored.
  images: { unoptimized: true },

  // Keeps the build honest: a type error or lint failure fails `make ci`
  // rather than shipping to an appliance nobody can hot-fix.
  eslint: { ignoreDuringBuilds: false },
  typescript: { ignoreBuildErrors: false },

  ...(isDev
    ? {
        async rewrites() {
          return [
            { source: '/api/:path*', destination: `${API_ORIGIN}/api/:path*` },
          ];
        },
      }
    : {}),
};

export default config;
