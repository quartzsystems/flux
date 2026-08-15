'use client';

/**
 * Port Groups: group definitions and engine lifecycle.
 *
 * A group is the unit an engine instance is launched over, so this page is
 * where one is created, its membership edited, and its engine started — the
 * last of which is normally on-demand when a run starts, but an operator who
 * has just rebound a NIC wants to know the engine comes back before committing
 * a test to it.
 *
 * Anyone signed in can see the groups; changing them is admin work, and the
 * controls are disabled rather than hidden for everyone else.
 */

import { Play, Plus, Square } from 'lucide-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState, type FormEvent } from 'react';

import { AppShell } from '@/components/AppShell';
import {
  Alert,
  Badge,
  Dash,
  EmptyRow,
  GroupStateBadge,
  Page,
  PageBody,
  PageHeader,
  Surface,
  TableSkeleton,
} from '@/components/ui';
import { ModalHeader, ModalShell } from '@/components/ui/Modal';
import { RowActions } from '@/components/ui/RowActions';
import {
  CheckList,
  CheckRow,
  ErrorText,
  Field,
  ModalFooter,
  SelectInput,
  TextInput,
} from '@/components/ui/formkit';
import { ApiError, api, queryKeys } from '@/lib/api';
import { type EngineMode, type Port, type PortGroup } from '@/lib/api-types';
import { useAuth } from '@/lib/auth';

export default function GroupsPage() {
  return (
    <AppShell>
      <Groups />
    </AppShell>
  );
}

function Groups() {
  const queryClient = useQueryClient();
  const { can } = useAuth();
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState<PortGroup | null>(null);

  const admin = can('admin');

  const groups = useQuery({
    queryKey: queryKeys.portGroups,
    queryFn: ({ signal }) => api.portGroups.list(signal),
    // Bring-up is not instant; poll only while something is mid-transition.
    refetchInterval: (query) =>
      (query.state.data ?? []).some((g) => g.state === 'starting') ? 2_000 : false,
  });

  const ports = useQuery({
    queryKey: queryKeys.ports,
    queryFn: ({ signal }) => api.ports.list(signal),
  });

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.portGroups });
    void queryClient.invalidateQueries({ queryKey: queryKeys.ports });
  };

  const start = useMutation({
    mutationFn: (id: string) => api.portGroups.start(id),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const stop = useMutation({
    mutationFn: (id: string) => api.portGroups.stop(id),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.portGroups.remove(id),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const rows = groups.data ?? [];
  const busy = start.isPending || stop.isPending || remove.isPending;

  return (
    <Page>
      <PageHeader
        title="Port Groups"
        subtitle={
          groups.data ? `${rows.length} defined` : 'The sets of ports engine instances drive'
        }
        actions={
          <button
            type="button"
            className="btn btn-primary btn-sm"
            onClick={() => {
              setError(null);
              setCreating(true);
            }}
            disabled={!admin}
            title={admin ? 'Create a port group' : 'Requires an admin account'}
          >
            <Plus size={15} />
            New group
          </button>
        }
      />

      <PageBody>
        {error ? <Alert tone="danger">{error}</Alert> : null}

        <Surface padded={false}>
          <div className="qz-table-wrap">
            <table className="qz-table">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Mode</th>
                  <th>State</th>
                  <th className="num">Ports</th>
                  <th>Detail</th>
                  <th className="right">Actions</th>
                </tr>
              </thead>
              {groups.isLoading ? (
                <TableSkeleton columns={6} rows={2} />
              ) : (
                <tbody>
                  {rows.length === 0 ? (
                    <EmptyRow columns={6}>
                      No port groups. Create one to give an engine something to drive.
                    </EmptyRow>
                  ) : (
                    rows.map((group) => (
                      <PortGroupRow
                        key={group.id}
                        group={group}
                        busy={busy}
                        admin={admin}
                        onStart={() => {
                          setError(null);
                          start.mutate(group.id);
                        }}
                        onStop={() => {
                          setError(null);
                          stop.mutate(group.id);
                        }}
                        onEdit={() => {
                          setError(null);
                          setEditing(group);
                        }}
                        onDelete={() => {
                          setError(null);
                          return remove.mutateAsync(group.id).catch(() => undefined);
                        }}
                      />
                    ))
                  )}
                </tbody>
              )}
            </table>
          </div>
        </Surface>

        <p className="note">
          An engine takes its ports in transmit/receive pairs, so a group holds an even number of
          them. Load profiles need a group in <span className="mono">astf</span> (stateful) mode.
        </p>
      </PageBody>

      {creating || editing ? (
        <PortGroupDialog
          group={editing}
          ports={ports.data ?? []}
          onClose={() => {
            setCreating(false);
            setEditing(null);
          }}
          onSaved={() => {
            setCreating(false);
            setEditing(null);
            invalidate();
          }}
        />
      ) : null}
    </Page>
  );
}

/** One port group row. */
function PortGroupRow({
  group,
  busy,
  admin,
  onStart,
  onStop,
  onEdit,
  onDelete,
}: {
  group: PortGroup;
  busy: boolean;
  admin: boolean;
  onStart: () => void;
  onStop: () => void;
  onEdit: () => void;
  onDelete: () => Promise<unknown>;
}) {
  const up = group.state === 'ready' || group.state === 'starting';
  const empty = group.portIds.length === 0;

  return (
    <tr>
      <td className="mono">{group.name}</td>
      <td>
        <Badge tone={group.engineMode === 'astf' ? 'info' : 'muted'}>{group.engineMode}</Badge>
      </td>
      <td>
        <GroupStateBadge state={group.state} />
      </td>
      <td className="num">{group.portIds.length}</td>
      <td className="dim" style={{ maxWidth: 320 }}>
        {group.error ?? <Dash />}
      </td>
      <td>
        <div className="row gap-6 end">
          {up ? (
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              disabled={!admin || busy}
              title={admin ? 'Stop the engine' : 'Requires an admin account'}
              onClick={onStop}
            >
              <Square size={13} />
              Stop
            </button>
          ) : (
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              disabled={!admin || busy || empty}
              title={
                !admin
                  ? 'Requires an admin account'
                  : empty
                    ? 'This group has no member ports'
                    : 'Bring the engine up'
              }
              onClick={onStart}
            >
              <Play size={13} />
              Start
            </button>
          )}
          <RowActions
            label={`group ${group.name}`}
            onEdit={onEdit}
            onDelete={onDelete}
            editDisabled={!admin || busy || up}
            editTitle={
              !admin
                ? 'Editing groups requires an admin account'
                : up
                  ? 'Stop the engine before editing this group'
                  : undefined
            }
            deleteDisabled={!admin || busy || up}
            deleteTitle={
              !admin
                ? 'Deleting groups requires an admin account'
                : up
                  ? 'Stop the engine before deleting this group'
                  : undefined
            }
          />
        </div>
      </td>
    </tr>
  );
}

/** Dialog for creating a group or replacing one's definition and membership. */
function PortGroupDialog({
  group,
  ports,
  onClose,
  onSaved,
}: {
  /** The group being edited, or null to create one. */
  group: PortGroup | null;
  ports: Port[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const [name, setName] = useState(group?.name ?? '');
  const [engineMode, setEngineMode] = useState<EngineMode>(group?.engineMode ?? 'stl');
  // Order matters — the engine numbers ports in list order — so membership is
  // an array that keeps click order, not a set.
  const [portIds, setPortIds] = useState<string[]>(group?.portIds ?? []);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [general, setGeneral] = useState('');

  const save = useMutation({
    mutationFn: () => {
      const body = { name: name.trim(), engineMode, portIds };
      return group ? api.portGroups.update(group.id, body) : api.portGroups.create(body);
    },
    onSuccess: onSaved,
    onError: (e) => {
      if (e instanceof ApiError && e.fieldErrors.length > 0) {
        setGeneral('');
        setFieldErrors(Object.fromEntries(e.fieldErrors.map((f) => [f.path, f.msg])));
      } else {
        setFieldErrors({});
        setGeneral(describe(e));
      }
    },
  });

  const toggle = (id: string) => {
    setPortIds((ids) => (ids.includes(id) ? ids.filter((p) => p !== id) : [...ids, id]));
  };

  // Every path under portIds collapses to one line — the picker is one
  // control, and "port 3 of the list" means nothing once boxes are ticked
  // and unticked.
  const membershipError = Object.entries(fieldErrors).find(([path]) =>
    path.startsWith('portIds'),
  )?.[1];

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!save.isPending) save.mutate();
  };

  return (
    <ModalShell onClose={onClose}>
      <ModalHeader
        title={group ? `Edit ${group.name}` : 'New port group'}
        subtitle="The set of ports one engine instance drives."
        onClose={onClose}
      />
      <form onSubmit={submit} className="stack gap-14">
        <Field label="Name" htmlFor="group-name" required error={fieldErrors.name}>
          <TextInput
            id="group-name"
            value={name}
            onChange={setName}
            mono
            autoFocus
            invalid={Boolean(fieldErrors.name)}
          />
        </Field>

        <Field
          label="Engine mode"
          htmlFor="group-mode"
          hint="Stateless generates frames; stateful emulates connections for load profiles."
        >
          <SelectInput
            id="group-mode"
            value={engineMode}
            onChange={(v) => setEngineMode(v as EngineMode)}
          >
            <option value="stl">stl — stateless</option>
            <option value="astf">astf — stateful</option>
          </SelectInput>
        </Field>

        <Field
          label="Member ports"
          error={membershipError}
          hint="An even number, in transmit/receive pairs. A port already in another group cannot be taken."
        >
          {ports.length === 0 ? (
            <p className="note">No ports have been discovered yet.</p>
          ) : (
            <CheckList>
              {ports.map((port) => {
                const heldElsewhere =
                  port.group !== null && (!group || port.group.id !== group.id);
                return (
                  <CheckRow
                    key={port.id}
                    checked={portIds.includes(port.id)}
                    onChange={() => {
                      if (!heldElsewhere) toggle(port.id);
                    }}
                  >
                    <span className="mono" style={{ fontSize: 12.5 }}>
                      {port.name}
                    </span>
                    <span className="note">{port.pciAddr}</span>
                    {heldElsewhere ? (
                      <span className="note">in {port.group?.name}</span>
                    ) : null}
                  </CheckRow>
                );
              })}
            </CheckList>
          )}
        </Field>

        <ErrorText msg={general} />
        <ModalFooter
          onCancel={onClose}
          saving={save.isPending}
          disabled={!name.trim()}
          savingLabel="Saving…"
          submitLabel={group ? 'Save group' : 'Create group'}
        />
      </form>
    </ModalShell>
  );
}

/** Turns any thrown value into a line an operator can act on. */
function describe(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.fieldErrors.length > 0) {
      return error.fieldErrors.map((e) => `${e.path}: ${e.msg}`).join('; ');
    }
    if (error.status === 403) return 'Your account is not permitted to do that.';
    return error.message;
  }
  return 'The request failed.';
}
