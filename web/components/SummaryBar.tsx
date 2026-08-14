'use client';

/**
 * The computed summary bar shown under an editor.
 *
 * Flows and load profiles both preview continuously and both end in this bar:
 * the resolved figures in mono, a badge for how much of the line they claim,
 * and a muted meta row of the derived numbers. It lives here so the two read
 * identically — including the badge tones, which turn `warn` above 90% of
 * line and `crit` above 100%.
 */

import { Fragment, type ReactNode } from 'react';

import { Badge, Skeleton } from '@/components/ui';

/** One figure in the bar. */
export interface SummaryItem {
  /** Optional muted caption rendered before the value. */
  label?: string;
  /** The figure itself, monospaced. */
  value: ReactNode;
  /**
   * Entries for the small meta row under the figures. Pass sibling `<span>`s
   * (in a fragment when there are several); the row supplies the gap and the
   * `.note` sizing.
   */
  meta?: ReactNode;
}

/** The tone every "% of line" badge carries: warn above 90, crit above 100. */
function linePctTone(pct: number): 'ok' | 'warn' | 'crit' {
  return pct > 100 ? 'crit' : pct > 90 ? 'warn' : 'ok';
}

/**
 * The bar itself.
 *
 * `linePct` renders the shared "% of line" badge; `badge` replaces it for the
 * states a percentage cannot express (rate exceeds the port, speed unknown).
 */
export function SummaryBar({
  items = [],
  linePct,
  badge,
  loading = false,
}: {
  items?: SummaryItem[];
  /** Percent of line rate, badged with the shared tone rule. */
  linePct?: number;
  /** A custom badge shown instead of the percentage. */
  badge?: ReactNode;
  /** Draws a placeholder while the first preview is in flight. */
  loading?: boolean;
}) {
  if (loading) {
    return (
      <div className="summary-bar">
        <Skeleton height={16} width={340} />
      </div>
    );
  }

  const metas = items.filter((item) => item.meta != null);

  return (
    <div className="summary-bar">
      <div className="row gap-14" style={{ flexWrap: 'wrap' }}>
        {items.map((item, index) => (
          <span key={index} className="mono">
            {item.label ? <span className="muted">{item.label} </span> : null}
            {item.value}
          </span>
        ))}
        {badge ??
          (linePct != null ? (
            <Badge tone={linePctTone(linePct)}>{linePct.toFixed(1)}% of line</Badge>
          ) : null)}
      </div>

      {metas.length > 0 ? (
        <div className="row gap-14 mono note">
          {metas.map((item, index) => (
            <Fragment key={index}>{item.meta}</Fragment>
          ))}
        </div>
      ) : null}
    </div>
  );
}
