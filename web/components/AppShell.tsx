'use client';

/**
 * The application shell: 240px sidebar, 56px topbar, scrolling main region.
 *
 * The shell also owns the authentication gate. Every page under it renders only
 * once `/auth/me` has answered, so no page needs its own guard and none can
 * accidentally ship without one.
 */

import {
  IconActivity,
  IconAdjustments,
  IconChartHistogram,
  IconChevronLeft,
  IconChevronRight,
  IconFileAnalytics,
  IconLayoutDashboard,
  IconLogout,
  IconPlugConnected,
  IconRoute,
  IconServerBolt,
  IconTopologyRing3,
  type Icon,
} from '@tabler/icons-react';
import { useQuery } from '@tanstack/react-query';
import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { useEffect, useState, type ReactNode } from 'react';

import { FluxLockup, FluxMark } from '@/components/Brand';
import { Badge, Skeleton } from '@/components/ui';
import { api, queryKeys } from '@/lib/api';
import { useAuth, useRequireAuth } from '@/lib/auth';
import { type Role } from '@/lib/api-types';

/** One entry in the sidebar. */
interface NavItem {
  href: string;
  label: string;
  icon: Icon;
  /** Minimum role that can reach the page. */
  minRole?: Role;
  /** Marks a destination that exists but has no content until a later milestone. */
  upcoming?: boolean;
}

/** One titled block of navigation. */
interface NavSection {
  title: string;
  items: NavItem[];
}

/**
 * The navigation tree.
 *
 * Milestone 1 implements the dashboard and ports; the rest are listed so the
 * shape of the product is visible from the first screen, and each is marked so
 * nobody mistakes an empty page for a broken one.
 */
const NAV: NavSection[] = [
  {
    title: 'Overview',
    items: [{ href: '/', label: 'Dashboard', icon: IconLayoutDashboard }],
  },
  {
    title: 'Configure',
    items: [
      { href: '/ports', label: 'Ports', icon: IconPlugConnected },
      { href: '/topology', label: 'Topology', icon: IconTopologyRing3, upcoming: true },
      { href: '/flows', label: 'Flows', icon: IconRoute, upcoming: true },
      { href: '/profiles', label: 'Load profiles', icon: IconServerBolt, upcoming: true },
    ],
  },
  {
    title: 'Execute',
    items: [
      { href: '/tests', label: 'Tests', icon: IconActivity, upcoming: true },
      { href: '/runs', label: 'Runs', icon: IconFileAnalytics, upcoming: true },
      { href: '/analytics', label: 'Analytics', icon: IconChartHistogram, upcoming: true },
    ],
  },
  {
    title: 'System',
    items: [
      { href: '/settings', label: 'Settings', icon: IconAdjustments, minRole: 'admin' },
    ],
  },
];

/** Where the collapsed state is remembered between visits. */
const COLLAPSE_KEY = 'flux.sidebar.collapsed';

/** Wraps a page in the shell, gated on an authenticated session. */
export function AppShell({ children }: { children: ReactNode }) {
  const user = useRequireAuth();
  const { loading } = useAuth();
  const [collapsed, setCollapsed] = useState(false);

  // Read on mount rather than during render: `localStorage` does not exist while
  // the static export is being prerendered, and reading it in render would make
  // the first client paint disagree with the server HTML.
  useEffect(() => {
    setCollapsed(window.localStorage.getItem(COLLAPSE_KEY) === 'true');
  }, []);

  const toggleCollapsed = () => {
    setCollapsed((previous) => {
      const next = !previous;
      window.localStorage.setItem(COLLAPSE_KEY, String(next));
      return next;
    });
  };

  if (loading || !user) {
    return <ShellSkeleton />;
  }

  return (
    <div className="app-shell" data-collapsed={collapsed}>
      <Sidebar collapsed={collapsed} onToggle={toggleCollapsed} />
      <Topbar />
      <main className="app-main">{children}</main>
    </div>
  );
}

/** The shell drawn while the session check is in flight. */
function ShellSkeleton() {
  return (
    <div className="app-shell">
      <aside className="app-sidebar">
        <div className="sidebar-header">
          <FluxLockup width={110} />
        </div>
        <div className="sidebar-nav stack gap-8">
          {Array.from({ length: 7 }, (_, i) => (
            <Skeleton key={i} height={30} />
          ))}
        </div>
      </aside>
      <div className="app-topbar" />
      <main className="app-main">
        <div className="page stack gap-14">
          <Skeleton height={36} width={280} />
          <Skeleton height={180} />
        </div>
      </main>
    </div>
  );
}

/** The left navigation rail. */
function Sidebar({ collapsed, onToggle }: { collapsed: boolean; onToggle: () => void }) {
  const pathname = usePathname();
  const { can } = useAuth();

  return (
    <aside className="app-sidebar">
      <div className="sidebar-header">
        <Link href="/" aria-label="Flux home">
          {collapsed ? <FluxMark size={28} /> : <FluxLockup width={110} />}
        </Link>
      </div>

      <nav className="sidebar-nav">
        {NAV.map((section) => {
          const visible = section.items.filter((item) => !item.minRole || can(item.minRole));
          if (visible.length === 0) return null;

          return (
            <div key={section.title}>
              {!collapsed ? <div className="sidebar-section">{section.title}</div> : null}
              {visible.map((item) => (
                <NavLink
                  key={item.href}
                  item={item}
                  collapsed={collapsed}
                  active={isActive(pathname, item.href)}
                />
              ))}
            </div>
          );
        })}
      </nav>

      <div className="sidebar-footer">
        {!collapsed ? <DaemonVersion /> : null}
        <button
          type="button"
          className="btn btn-ghost btn-icon"
          onClick={onToggle}
          aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
          title={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
        >
          {collapsed ? <IconChevronRight size={16} /> : <IconChevronLeft size={16} />}
        </button>
      </div>
    </aside>
  );
}

/**
 * The running daemon's version.
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

  return <span>{data ? `v${data.version}` : ''}</span>;
}

/** One navigation entry. */
function NavLink({
  item,
  collapsed,
  active,
}: {
  item: NavItem;
  collapsed: boolean;
  active: boolean;
}) {
  const Icon = item.icon;
  return (
    <Link
      href={item.href}
      className="nav-item"
      aria-current={active ? 'page' : undefined}
      title={collapsed ? item.label : undefined}
    >
      <Icon size={17} stroke={1.7} />
      {!collapsed ? (
        <>
          <span style={{ flex: 1 }}>{item.label}</span>
          {item.upcoming ? <Badge tone="muted">soon</Badge> : null}
        </>
      ) : null}
    </Link>
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

/** The top bar: appliance identity on the left, account controls on the right. */
function Topbar() {
  const { user, logout } = useAuth();

  return (
    <header className="app-topbar">
      <div style={{ flex: 1 }} />

      {user ? (
        <div className="row gap-14">
          <div className="row gap-8">
            <span className="mono" style={{ fontSize: 12.5 }}>
              {user.username}
            </span>
            <Badge tone={user.role === 'admin' ? 'info' : user.role === 'operator' ? 'ok' : 'muted'}>
              {user.role}
            </Badge>
          </div>
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            onClick={() => void logout()}
            title="Sign out"
          >
            <IconLogout size={15} stroke={1.7} />
            Sign out
          </button>
        </div>
      ) : null}
    </header>
  );
}
