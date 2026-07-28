'use client';

/**
 * The placeholder for a route that exists in the navigation but whose content
 * lands in a later milestone.
 *
 * These routes are listed in the sidebar from day one so the shape of the
 * product is visible, which means clicking one must land somewhere honest
 * rather than on a 404 that reads as a broken build.
 */

import { IconTools } from '@tabler/icons-react';

import { AppShell } from '@/components/AppShell';
import { PageHeader, Surface } from '@/components/ui';

/** Renders a titled page explaining what will live here and when. */
export function Upcoming({
  title,
  subtitle,
  milestone,
  children,
}: {
  title: string;
  subtitle: string;
  /** Which milestone delivers this page. */
  milestone: number;
  /** What the page will do, as a short list. */
  children: React.ReactNode;
}) {
  return (
    <AppShell>
      <div className="page stack gap-18">
        <PageHeader title={title} subtitle={subtitle} />
        <Surface>
          <div className="row gap-14" style={{ alignItems: 'flex-start' }}>
            <IconTools
              size={20}
              stroke={1.6}
              style={{ color: 'var(--qz-fg-4)', marginTop: 2, flexShrink: 0 }}
            />
            <div className="stack gap-8">
              <p style={{ margin: 0, fontSize: 14, color: 'var(--qz-fg-1)' }}>
                Arriving in milestone {milestone}.
              </p>
              <div className="dim" style={{ fontSize: 13 }}>
                {children}
              </div>
            </div>
          </div>
        </Surface>
      </div>
    </AppShell>
  );
}
