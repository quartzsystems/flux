/**
 * The shared primitives every page is built from.
 *
 * These are thin wrappers over the class names in `globals.css` rather than
 * styled components. The design system is CSS; this file exists so a page never
 * has to remember that a KPI label is `mono 10px uppercase` — it asks for a
 * `<Kpi>` and gets the canonical one.
 */

import type { ReactNode } from 'react';

import type { LinkState, PortGroupState, PortMode, Role } from '@/lib/api-types';

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/** Page title, subtitle, and right-aligned actions. */
export function PageHeader({
  title,
  subtitle,
  actions,
}: {
  title: string;
  subtitle?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <header className="page-header">
      <div>
        <h1>{title}</h1>
        {subtitle ? <p className="subtitle">{subtitle}</p> : null}
      </div>
      {actions ? <div className="actions">{actions}</div> : null}
    </header>
  );
}

/** A card. `title` renders the bordered head; omit it for a bare surface. */
export function Surface({
  title,
  actions,
  children,
  padded = true,
}: {
  title?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  /** Set false when the child manages its own padding, e.g. a full-bleed table. */
  padded?: boolean;
}) {
  return (
    <section className="surface">
      {title ? (
        <div className="surface-head">
          <h3>{title}</h3>
          {actions ? <div className="row gap-8">{actions}</div> : null}
        </div>
      ) : null}
      <div className={padded ? 'surface-body' : undefined}>{children}</div>
    </section>
  );
}

/** A labelled metric. */
export function Kpi({
  label,
  value,
  unit,
  foot,
  icon,
}: {
  label: string;
  value: ReactNode;
  unit?: string;
  foot?: ReactNode;
  icon?: ReactNode;
}) {
  return (
    <div className="kpi">
      <div className="kpi-label">
        {icon}
        {label}
      </div>
      <div className="kpi-value">
        {value}
        {unit ? <span className="kpi-unit">{unit}</span> : null}
      </div>
      {foot ? <div className="kpi-foot">{foot}</div> : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Badges
// ---------------------------------------------------------------------------

/** Visual weight of a status pill. */
export type BadgeTone = 'ok' | 'warn' | 'crit' | 'info' | 'muted';

/** A status pill. */
export function Badge({ tone = 'muted', children }: { tone?: BadgeTone; children: ReactNode }) {
  return <span className={`badge badge-${tone}`}>{children}</span>;
}

/**
 * Link state, shown as a coloured dot plus its name.
 *
 * `unknown` is deliberately muted rather than red: a DPDK-bound port with no
 * engine attached genuinely has no observable carrier, and colouring that as a
 * fault would train operators to ignore real ones.
 */
export function LinkBadge({ state }: { state: LinkState }) {
  const tone: BadgeTone = state === 'up' ? 'ok' : state === 'down' ? 'crit' : 'muted';
  return (
    <Badge tone={tone}>
      <span className={`link-dot link-dot-${state}`} aria-hidden />
      {state}
    </Badge>
  );
}

/** Driver ownership. DPDK is the "armed" state, so it gets the accent. */
export function ModeBadge({ mode }: { mode: PortMode }) {
  return <Badge tone={mode === 'dpdk' ? 'info' : 'muted'}>{mode}</Badge>;
}

/** Engine lifecycle for a port group. */
export function GroupStateBadge({ state }: { state: PortGroupState }) {
  const tone: BadgeTone =
    state === 'ready' ? 'ok' : state === 'error' ? 'crit' : state === 'starting' ? 'warn' : 'muted';
  return <Badge tone={tone}>{state}</Badge>;
}

/** Account access level. */
export function RoleBadge({ role }: { role: Role }) {
  const tone: BadgeTone = role === 'admin' ? 'info' : role === 'operator' ? 'ok' : 'muted';
  return <Badge tone={tone}>{role}</Badge>;
}

// ---------------------------------------------------------------------------
// Feedback
// ---------------------------------------------------------------------------

/** An inline message. */
export function Alert({
  tone = 'info',
  children,
}: {
  tone?: 'info' | 'warn' | 'danger';
  children: ReactNode;
}) {
  return (
    <div className={`alert alert-${tone}`} role={tone === 'danger' ? 'alert' : 'status'}>
      {children}
    </div>
  );
}

/** A placeholder block sized like the content it stands in for. */
export function Skeleton({ height = 16, width }: { height?: number; width?: number | string }) {
  return <div className="skeleton" style={{ height, width: width ?? '100%' }} aria-hidden />;
}

/** Table body shown while the first page of rows is loading. */
export function TableSkeleton({ rows = 4, columns }: { rows?: number; columns: number }) {
  return (
    <tbody>
      {Array.from({ length: rows }, (_, r) => (
        <tr key={r}>
          {Array.from({ length: columns }, (_, c) => (
            <td key={c}>
              <Skeleton height={14} />
            </td>
          ))}
        </tr>
      ))}
    </tbody>
  );
}

/** The message shown in place of rows when a table has none. */
export function EmptyRow({ columns, children }: { columns: number; children: ReactNode }) {
  return (
    <tr>
      <td className="empty" colSpan={columns}>
        {children}
      </td>
    </tr>
  );
}
