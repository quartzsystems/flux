'use client';

/**
 * Load profiles: the L4-7 counterpart of the flows page — the same full-width
 * DataTable chassis, with editing on a profile's own page at
 * `/profiles/<id>/`.
 */

import { Plus } from 'lucide-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useRouter } from 'next/navigation';
import { useMemo, useState } from 'react';

import { AppShell } from '@/components/AppShell';
import {
  Alert,
  Dash,
  Page,
  PageBody,
  PageHeader,
} from '@/components/ui';
import { DataTable, type Column } from '@/components/ui/DataTable';
import { RowActions } from '@/components/ui/RowActions';
import { ApiError, api, queryKeys } from '@/lib/api';
import { loadProfileConfigSchema, type LoadProfile } from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import { formatTimestamp } from '@/lib/format';

export default function ProfilesPage() {
  return (
    <AppShell>
      <Profiles />
    </AppShell>
  );
}

function Profiles() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { can } = useAuth();
  const [error, setError] = useState<string | null>(null);

  const profiles = useQuery({
    queryKey: queryKeys.loadProfiles,
    queryFn: ({ signal }) => api.loadProfiles.list(signal),
  });

  const ports = useQuery({
    queryKey: queryKeys.ports,
    queryFn: ({ signal }) => api.ports.list(signal),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.loadProfiles.remove(id),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.loadProfiles }),
    onError: (e) => setError(e instanceof ApiError ? e.message : 'The delete failed.'),
  });

  /** Resolves a port id to its name, for the endpoint columns. */
  const portName = useMemo(() => {
    const byId = new Map((ports.data ?? []).map((p) => [p.id, p.name]));
    return (id: string | null) => (id ? (byId.get(id) ?? null) : null);
  }, [ports.data]);

  /** A profile's endpoints, or nulls for one this build cannot read. */
  const endpoints = (profile: LoadProfile): { client: string | null; server: string | null } => {
    const parsed = loadProfileConfigSchema.safeParse(profile.config);
    if (!parsed.success) return { client: null, server: null };
    return {
      client: portName(parsed.data.clientPort),
      server: portName(parsed.data.serverPort),
    };
  };

  const openProfile = (profile: LoadProfile) => router.push(`/profiles/${profile.id}/`);

  const columns: Column<LoadProfile>[] = [
    {
      key: 'name',
      header: 'Name',
      value: (p) => p.name,
      sortable: true,
      mono: true,
    },
    {
      key: 'client',
      header: 'Client port',
      value: (p) => endpoints(p).client,
      render: (p) => {
        const client = endpoints(p).client;
        return client ? <span className="mono">{client}</span> : <Dash />;
      },
      sortable: true,
    },
    {
      key: 'server',
      header: 'Server port',
      value: (p) => endpoints(p).server,
      render: (p) => {
        const server = endpoints(p).server;
        return server ? <span className="mono">{server}</span> : <Dash />;
      },
      sortable: true,
    },
    {
      key: 'updated',
      header: 'Updated',
      value: (p) => Date.parse(p.updatedAt),
      render: (p) => <span className="dim">{formatTimestamp(p.updatedAt)}</span>,
      sortable: true,
    },
  ];

  return (
    <Page>
      <PageHeader
        title="Load Profiles"
        subtitle={profiles.data ? `${profiles.data.length} defined` : 'Stateful L4-7 traffic'}
        actions={
          <button
            type="button"
            className="btn btn-primary btn-sm"
            onClick={() => router.push('/profiles/new/')}
            disabled={!can('operator') || (ports.data ?? []).length < 2}
            title={
              !can('operator')
                ? 'Creating profiles requires an operator account'
                : (ports.data ?? []).length < 2
                  ? 'A profile needs a client port and a server port'
                  : 'Create a load profile'
            }
          >
            <Plus size={15} />
            New profile
          </button>
        }
      />

      <PageBody>
        {error ? <Alert tone="danger">{error}</Alert> : null}

        <DataTable
          rows={profiles.data ?? []}
          columns={columns}
          rowId={(p) => p.id}
          storageKey="load-profiles"
          searchPlaceholder="Search profiles…"
          emptyMessage={profiles.isLoading ? 'Loading…' : 'No profiles yet.'}
          actionsWidth={90}
          onRefresh={() => queryClient.invalidateQueries({ queryKey: queryKeys.loadProfiles })}
          onRowOpen={openProfile}
          actions={(p) => (
            <RowActions
              label={`profile ${p.name}`}
              onEdit={() => openProfile(p)}
              onDelete={() => {
                setError(null);
                return remove.mutateAsync(p.id).catch(() => undefined);
              }}
              deleteDisabled={!can('operator')}
              deleteTitle={
                can('operator') ? undefined : 'Deleting profiles requires an operator account'
              }
            />
          )}
        />
      </PageBody>
    </Page>
  );
}
