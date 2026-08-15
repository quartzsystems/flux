'use client';

/**
 * The application shell, in the shape Lumen and Quartz Command use: a 240px
 * sidebar on `--qz-ink-0` with the product lockup at the top, the nav tree in
 * the middle, and the signed-in principal in the footer, beside a scrolling
 * main pane. There is deliberately no topbar — identity lives in the sidebar
 * footer so all three consoles read as one product.
 *
 * The shell also owns the authentication gate. Every page under it renders only
 * once `/auth/me` has answered, so no page needs its own guard and none can
 * accidentally ship without one.
 */

import {
  Activity,
  ChartLine,
  ChevronDown,
  ChevronRight,
  EthernetPort,
  Gauge,
  History,
  Layers,
  LayoutDashboard,
  LogOut,
  Network,
  Play,
  Route,
  Settings,
  SlidersHorizontal,
  type LucideIcon,
} from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { useState, type ReactNode } from 'react';

import { FluxMark } from '@/components/Brand';
import { Skeleton } from '@/components/ui';
import { api, queryKeys } from '@/lib/api';
import { useAuth, useRequireAuth } from '@/lib/auth';
import { type Role } from '@/lib/api-types';

/** A leaf destination in the sidebar. */
interface NavChild {
  href: string;
  label: string;
  icon: LucideIcon;
  /** Minimum role that can reach the page. */
  minRole?: Role;
}

/**
 * A top-level entry. Entries with `children` render as an expandable section,
 * exactly the shape Lumen's sidebar uses; entries without are plain links.
 */
interface NavItem {
  id: string;
  label: string;
  icon: LucideIcon;
  href?: string;
  minRole?: Role;
  children?: NavChild[];
}

/**
 * The navigation tree.
 *
 * Grouped by what an operator is doing rather than by which subsystem owns the
 * page: wire it up, then run it.
 */
const NAV: NavItem[] = [
  { id: 'dashboard', label: 'Dashboard', icon: LayoutDashboard, href: '/' },
  {
    id: 'configure',
    label: 'Configure',
    icon: SlidersHorizontal,
    children: [
      { href: '/ports', label: 'Ports', icon: EthernetPort },
      { href: '/groups', label: 'Port Groups', icon: Layers },
      { href: '/topology', label: 'Topology', icon: Network },
      { href: '/flows', label: 'Flows', icon: Route },
      { href: '/profiles', label: 'Load Profiles', icon: Gauge },
    ],
  },
  {
    id: 'execute',
    label: 'Execute',
    icon: Play,
    children: [
      { href: '/tests', label: 'Tests', icon: Activity },
      { href: '/runs', label: 'Runs', icon: History },
      { href: '/analytics', label: 'Analytics', icon: ChartLine },
    ],
  },
  { id: 'settings', label: 'Settings', icon: Settings, href: '/settings', minRole: 'admin' },
];

/** The nav row, in the exact class string Lumen and Quartz Command use. */
const itemClass = (active: boolean) =>
  [
    'flex items-center gap-[10px] px-[10px] py-[8px] rounded-md text-[13.5px] font-medium border transition-all duration-[120ms] no-underline w-full text-left cursor-pointer',
    active
      ? 'bg-[var(--qz-accent-soft)] text-[var(--qz-accent)] border-[color-mix(in_oklab,var(--qz-accent)_30%,transparent)]'
      : 'text-[var(--qz-fg-3)] border-transparent hover:text-[var(--qz-fg-1)] hover:bg-[color-mix(in_oklab,white_4%,transparent)]',
  ].join(' ');

/** Avatar initials — the first two letters of the username, as in Lumen. */
const userInitials = (username: string): string => username.slice(0, 2).toUpperCase();

/** Wraps a page in the shell, gated on an authenticated session. */
export function AppShell({ children }: { children: ReactNode }) {
  const user = useRequireAuth();
  const { loading } = useAuth();

  if (loading || !user) {
    return <ShellSkeleton />;
  }

  return (
    <div
      className="h-screen overflow-hidden"
      style={{
        display: 'grid',
        gridTemplateColumns: '240px 1fr',
        gridTemplateRows: 'minmax(0, 1fr)',
      }}
    >
      <Sidebar />
      <main className="overflow-auto" style={{ background: 'var(--qz-bg)' }}>
        {children}
      </main>
    </div>
  );
}

/** The shell drawn while the session check is in flight. */
function ShellSkeleton() {
  return (
    <div
      className="h-screen overflow-hidden"
      style={{
        display: 'grid',
        gridTemplateColumns: '240px 1fr',
        gridTemplateRows: 'minmax(0, 1fr)',
      }}
    >
      <aside
        className="flex flex-col h-full"
        style={{ borderRight: '1px solid var(--qz-border)', background: 'var(--qz-ink-0)' }}
      >
        <div
          className="flex items-center gap-[10px] px-4 h-14 flex-shrink-0"
          style={{ borderBottom: '1px solid var(--qz-border)' }}
        >
          <FluxMark size={28} />
          <span className="font-bold text-[var(--qz-fg-1)] text-[15px]" style={{ letterSpacing: '-0.01em' }}>
            Flux
          </span>
        </div>
        <div className="flex-1 px-3 pt-3 flex flex-col gap-2">
          {Array.from({ length: 7 }, (_, i) => (
            <Skeleton key={i} height={30} />
          ))}
        </div>
      </aside>
      <main className="overflow-auto" style={{ background: 'var(--qz-bg)' }}>
        <div className="stack gap-14" style={{ padding: '28px 36px' }}>
          <Skeleton height={36} width={280} />
          <Skeleton height={180} />
        </div>
      </main>
    </div>
  );
}

/** The left navigation rail: lockup, nav tree, signed-in principal. */
function Sidebar() {
  const pathname = usePathname();
  const { can, user, logout } = useAuth();

  // Expandable sections: open when explicitly toggled, else default-open on
  // the active subtree — the same rule Lumen's sidebar applies.
  const [openMenus, setOpenMenus] = useState<Record<string, boolean>>({});
  const isOpen = (item: NavItem) =>
    openMenus[item.id] ??
    (item.children ?? []).some((child) => isActive(pathname, child.href));

  return (
    <aside
      className="flex flex-col h-full"
      style={{ borderRight: '1px solid var(--qz-border)', background: 'var(--qz-ink-0)' }}
    >
      {/* Logo */}
      <div
        className="flex items-center gap-[10px] px-4 h-14 flex-shrink-0"
        style={{ borderBottom: '1px solid var(--qz-border)' }}
      >
        <Link href="/" aria-label="Flux home" className="flex items-center gap-[10px] no-underline">
          <FluxMark size={28} />
          <span className="font-bold text-[var(--qz-fg-1)] text-[15px]" style={{ letterSpacing: '-0.01em' }}>
            Flux
          </span>
        </Link>
      </div>

      {/* Nav */}
      <nav className="flex-1 min-h-0 overflow-auto px-3 pt-3 pb-4 flex flex-col gap-[2px]">
        {NAV.map((item) => {
          if (item.minRole && !can(item.minRole)) return null;
          const Icon = item.icon;

          if (item.children) {
            const visible = item.children.filter((c) => !c.minRole || can(c.minRole));
            if (visible.length === 0) return null;
            const open = isOpen(item);

            // The parent never shows the green "active" state — only its
            // children light up.
            return (
              <div key={item.id}>
                <button
                  type="button"
                  onClick={() => setOpenMenus((p) => ({ ...p, [item.id]: !open }))}
                  className={itemClass(false)}
                >
                  <Icon size={16} />
                  <span className="flex-1">{item.label}</span>
                  {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                </button>
                {open ? (
                  <div className="flex flex-col gap-[2px] mt-[2px] ml-[26px]">
                    {visible.map((child) => {
                      const ChildIcon = child.icon;
                      const active = isActive(pathname, child.href);
                      return (
                        <Link
                          key={child.href}
                          href={child.href}
                          className={[
                            'flex items-center gap-[9px] px-[10px] py-[7px] rounded-md text-[13px] font-medium border transition-all duration-[120ms] no-underline',
                            active
                              ? 'bg-[var(--qz-accent-soft)] text-[var(--qz-accent)] border-[color-mix(in_oklab,var(--qz-accent)_30%,transparent)]'
                              : 'text-[var(--qz-fg-3)] border-transparent hover:text-[var(--qz-fg-1)] hover:bg-[color-mix(in_oklab,white_4%,transparent)]',
                          ].join(' ')}
                          aria-current={active ? 'page' : undefined}
                        >
                          <ChildIcon size={15} />
                          <span>{child.label}</span>
                        </Link>
                      );
                    })}
                  </div>
                ) : null}
              </div>
            );
          }

          const active = isActive(pathname, item.href ?? '');
          return (
            <Link
              key={item.id}
              href={item.href ?? '/'}
              className={itemClass(active)}
              aria-current={active ? 'page' : undefined}
            >
              <Icon size={16} />
              <span>{item.label}</span>
            </Link>
          );
        })}
        <DaemonVersion />
      </nav>

      {/* Footer */}
      <div
        className="flex-shrink-0 px-4 py-3 flex items-center gap-[10px]"
        style={{ borderTop: '1px solid var(--qz-border)' }}
      >
        <div
          className="w-7 h-7 rounded-full grid place-items-center text-[var(--qz-fg-on-accent)] font-bold text-xs flex-shrink-0"
          style={{ background: 'linear-gradient(135deg, var(--qz-green-700), var(--qz-green-500))' }}
        >
          {user ? userInitials(user.username) : '…'}
        </div>
        <div className="flex-1 min-w-0 flex flex-col">
          <span className="text-[var(--qz-fg-1)] font-semibold text-[13px] truncate leading-tight">
            {user?.username ?? ''}
          </span>
          {user && (
            <span className="text-[var(--qz-fg-4)] text-[11px] truncate leading-tight">{user.role}</span>
          )}
        </div>
        <button
          type="button"
          title="Sign out"
          onClick={() => void logout()}
          className="flex-shrink-0 text-[var(--qz-fg-4)] hover:text-[var(--qz-fg-1)] transition-colors cursor-pointer bg-transparent border-0 p-0"
        >
          <LogOut size={15} />
        </button>
      </div>
    </aside>
  );
}

/**
 * The running daemon's version, at the quiet end of the nav column.
 *
 * Read from `/system/health` rather than baked into the bundle at build time:
 * the browser may be holding a cached bundle from a previous release, and a
 * version string that reports the bundle rather than the daemon is worse than
 * none at all. The query is already cached by the dashboard, so this is free.
 */
function DaemonVersion() {
  const { data } = useQuery({
    queryKey: queryKeys.health,
    queryFn: ({ signal }) => api.system.health(signal),
    staleTime: 60_000,
  });

  if (!data) return null;
  return (
    <div
      className="mt-auto px-[10px] pt-[14px] text-[10.5px]"
      style={{ fontFamily: 'var(--qz-font-mono)', color: 'var(--qz-fg-4)' }}
    >
      v{data.version}
    </div>
  );
}

/**
 * Whether `href` is the section the current path belongs to.
 *
 * The root is matched exactly — every path starts with "/", so a prefix test
 * would light up the dashboard on every page.
 */
function isActive(pathname: string, href: string): boolean {
  if (href === '/') return pathname === '/';
  return pathname === href || pathname.startsWith(`${href}/`);
}
