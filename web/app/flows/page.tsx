'use client';

/**
 * The flows page: the full-width configuration table, on the shared DataTable
 * chassis Lumen and Quartz Command use. Editing happens on a flow's own page —
 * `/flows/<id>/` — so a definition is linkable and the table never has to share
 * the viewport with a form.
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
import { flowConfigSchema, type Flow } from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import { formatTimestamp } from '@/lib/format';

export default function FlowsPage() {
  return (
    <AppShell>
      <Flows />
    </AppShell>
  );
}

function Flows() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { can } = useAuth();
  const [error, setError] = useState<string | null>(null);

  const flows = useQuery({
    queryKey: queryKeys.flows,
    queryFn: ({ signal }) => api.flows.list(signal),
  });

  const ports = useQuery({
    queryKey: queryKeys.ports,
    queryFn: ({ signal }) => api.ports.list(signal),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.flows.remove(id),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.flows }),
    onError: (e) => setError(e instanceof ApiError ? e.message : 'The delete failed.'),
  });

  /** Resolves a port id to its name, for the endpoint columns. */
  const portName = useMemo(() => {
    const byId = new Map((ports.data ?? []).map((p) => [p.id, p.name]));
    return (id: string | null) => (id ? (byId.get(id) ?? null) : null);
  }, [ports.data]);

  /** A flow's endpoints, or nulls for one this build cannot read. */
  const endpoints = (flow: Flow): { tx: string | null; rx: string | null } => {
    const parsed = flowConfigSchema.safeParse(flow.config);
    if (!parsed.success) return { tx: null, rx: null };
    return { tx: portName(parsed.data.txPort), rx: portName(parsed.data.rxPort) };
  };

  const openFlow = (flow: Flow) => router.push(`/flows/${flow.id}/`);

  const columns: Column<Flow>[] = [
    {
      key: 'name',
      header: 'Name',
      value: (f) => f.name,
      sortable: true,
      mono: true,
    },
    {
      key: 'tx',
      header: 'Transmit port',
      value: (f) => endpoints(f).tx,
      render: (f) => {
        const tx = endpoints(f).tx;
        return tx ? <span className="mono">{tx}</span> : <Dash />;
      },
      sortable: true,
    },
    {
      key: 'rx',
      header: 'Receive port',
      value: (f) => endpoints(f).rx,
      render: (f) => {
        const rx = endpoints(f).rx;
        return rx ? <span className="mono">{rx}</span> : <Dash />;
      },
      sortable: true,
    },
    {
      key: 'updated',
      header: 'Updated',
      value: (f) => Date.parse(f.updatedAt),
      render: (f) => <span className="dim">{formatTimestamp(f.updatedAt)}</span>,
      sortable: true,
    },
  ];

  return (
    <Page>
      <PageHeader
        title="Flows"
        subtitle={flows.data ? `${flows.data.length} defined` : 'Traffic definitions'}
        actions={
          <button
            type="button"
            className="btn btn-primary btn-sm"
            onClick={() => router.push('/flows/new/')}
            disabled={!can('operator') || (ports.data ?? []).length === 0}
            title={
              !can('operator')
                ? 'Creating flows requires an operator account'
                : (ports.data ?? []).length === 0
                  ? 'No ports have been discovered yet'
                  : 'Create a flow'
            }
          >
            <Plus size={15} />
            New flow
          </button>
        }
      />

      <PageBody>
        {error ? <Alert tone="danger">{error}</Alert> : null}

        <DataTable
          rows={flows.data ?? []}
          columns={columns}
          rowId={(f) => f.id}
          storageKey="flows"
          searchPlaceholder="Search flows…"
          emptyMessage={flows.isLoading ? 'Loading…' : 'No flows yet.'}
          actionsWidth={90}
          onRefresh={() => queryClient.invalidateQueries({ queryKey: queryKeys.flows })}
          onRowOpen={openFlow}
          actions={(f) => (
            <RowActions
              label={`flow ${f.name}`}
              onEdit={() => openFlow(f)}
              onDelete={() => {
                setError(null);
                return remove.mutateAsync(f.id).catch(() => undefined);
              }}
              deleteDisabled={!can('operator')}
              deleteTitle={
                can('operator') ? undefined : 'Deleting flows requires an operator account'
              }
            />
          )}
        />
      </PageBody>
    </Page>
  );
}
