/**
 * Root layout.
 *
 * Fonts are imported from `@fontsource`, which ships the woff2 files inside
 * `node_modules` and emits plain `@font-face` rules. Nothing is fetched from
 * Google at build time or at run time — the appliance may never have seen the
 * internet, and a font request that silently fails would drop the whole UI onto
 * a system fallback.
 *
 * Weight 800 is loaded because the wordmark is set in Manrope ExtraBold; the
 * others cover body, labels, and headings. Only the weights actually used are
 * imported, since every one is a separate file the browser has to fetch.
 */

import '@fontsource/manrope/400.css';
import '@fontsource/manrope/500.css';
import '@fontsource/manrope/600.css';
import '@fontsource/manrope/700.css';
import '@fontsource/manrope/800.css';
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import '@fontsource/jetbrains-mono/600.css';

import './globals.css';

import type { Metadata, Viewport } from 'next';

import { TAGLINE } from '@/components/Brand';
import { Providers } from './providers';

export const metadata: Metadata = {
  title: {
    default: 'Flux',
    template: '%s · Flux',
  },
  description: TAGLINE,
  applicationName: 'Flux',
  icons: {
    icon: [{ url: '/favicon.svg', type: 'image/svg+xml' }],
    apple: [{ url: '/brand/flux-mark.svg', type: 'image/svg+xml' }],
  },
  manifest: '/site.webmanifest',
  // An appliance UI has nothing a crawler should index, and it is usually on a
  // management network a crawler cannot reach anyway.
  robots: { index: false, follow: false },
};

export const viewport: Viewport = {
  colorScheme: 'dark',
  themeColor: '#0b0d12',
  width: 'device-width',
  initialScale: 1,
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
