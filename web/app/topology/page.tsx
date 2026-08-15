'use client';

/**
 * The topology view: what is cabled to what, and what is moving across it.
 *
 * The layout is derived rather than drawn. Flux already knows which port
 * transmits and which receives for every flow, so asking an operator to place
 * nodes by hand would be asking them to re-enter something the appliance can
 * work out — and to keep it in step by hand every time a flow changes.
 *
 * The derivation is a two-colouring of the flow graph. In a device-under-test
 * setup every flow crosses the device, which makes the graph bipartite, and the
 * two colours are exactly the two sides of it. A graph that will not two-colour
 * still draws: the colouring degrades to breadth-first parity and the offending
 * flow is simply an edge within a column, which is a true picture of an unusual
 * cabling rather than a wrong picture of a normal one.
 */

import '@xyflow/react/dist/style.css';

import { Monitor, Plus } from 'lucide-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  Background,
  BackgroundVariant,
  BaseEdge,
  Controls,
  EdgeLabelRenderer,
  Handle,
  MarkerType,
  Position,
  ReactFlow,
  getBezierPath,
  type Edge,
  type EdgeProps,
  type Node,
  type NodeProps,
} from '@xyflow/react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { AppShell } from '@/components/AppShell';
import {
  Alert,
  Badge,
  Empty,
  LinkBadge,
  Page,
  PageBody,
  PageHeader,
  Skeleton,
  Surface,
} from '@/components/ui';
import { ApiError, api, queryKeys } from '@/lib/api';
import {
  DUT_FIELDS,
  flowConfigSchema,
  type Dut,
  type Flow,
  type Port,
} from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import { formatPps } from '@/lib/format';
import { useStatsStream } from '@/lib/stream';

/** Horizontal position of each column, in canvas units. */
const COLUMN_X = { left: 0, dut: 340, right: 680 } as const;

/** Vertical spacing between ports in a column. */
const ROW_HEIGHT = 92;

/** Canvas height. Fixed, because a viewport that grows with the port count
 *  would push the device editor off the screen on a chassis with many ports. */
const CANVAS_HEIGHT = 460;

export default function TopologyPage() {
  return (
    <AppShell>
      <Topology />
    </AppShell>
  );
}

/** A flow reduced to what the diagram needs. */
interface Link {
  id: string;
  name: string;
  txPort: string;
  rxPort: string;
}

/** The live numbers for one flow. */
interface LiveFlow {
  txPps: number;
  rxPps: number;
  lossPct: number;
}

function Topology() {
  const { can } = useAuth();

  const ports = useQuery({
    queryKey: queryKeys.ports,
    queryFn: ({ signal }) => api.ports.list(signal),
  });

  const flows = useQuery({
    queryKey: queryKeys.flows,
    queryFn: ({ signal }) => api.flows.list(signal),
  });

  const dut = useQuery({
    queryKey: queryKeys.dut,
    queryFn: ({ signal }) => api.topology.dut(signal),
  });

  const links = useMemo(() => toLinks(flows.data ?? []), [flows.data]);
  const live = useLiveFlows();

  const { nodes, edges, unplaced } = useMemo(
    () => build(ports.data ?? [], links, dut.data ?? {}, live),
    [ports.data, links, dut.data, live],
  );

  const loading = ports.isLoading || flows.isLoading;

  return (
    <Page>
      <PageHeader
        title="Topology"
        subtitle="Ports, the device under test, and the flows between them"
      />
      <PageBody>
        {flows.error ? (
          <Alert tone="danger">
            {flows.error instanceof ApiError ? flows.error.message : 'Flows could not be loaded.'}
          </Alert>
        ) : null}

        <Surface title="Diagram" padded={loading}>
          {loading ? (
            <Skeleton height={CANVAS_HEIGHT - 36} />
          ) : (ports.data ?? []).length === 0 ? (
            <Empty icon={<Monitor size={28} />}>
              No ports are known yet. Discover them from the ports page and the diagram fills in.
            </Empty>
          ) : links.length === 0 ? (
            <Empty icon={<Monitor size={28} />}>
              No flows are defined, so there is nothing crossing the device yet. Every port is
              shown below as unattached.
            </Empty>
          ) : null}

          {!loading && (ports.data ?? []).length > 0 ? (
            <div style={{ height: CANVAS_HEIGHT }}>
              <ReactFlow
                nodes={nodes}
                edges={edges}
                nodeTypes={NODE_TYPES}
                edgeTypes={EDGE_TYPES}
                fitView
                fitViewOptions={{ padding: 0.18 }}
                minZoom={0.3}
                maxZoom={1.6}
                // The diagram is derived, so letting it be dragged would invite an
                // operator to arrange something the next render throws away.
                nodesDraggable={false}
                nodesConnectable={false}
                edgesFocusable={false}
                proOptions={{ hideAttribution: true }}
              >
                <Background variant={BackgroundVariant.Dots} gap={20} size={1} color="#252830" />
                <Controls showInteractive={false} />
              </ReactFlow>
            </div>
          ) : null}

          {unplaced.length > 0 ? (
            <div
              className="row gap-8"
              style={{
                padding: '12px 18px',
                borderTop: '1px solid var(--qz-divider)',
                flexWrap: 'wrap',
              }}
            >
              <span className="field-label">Not in any flow</span>
              {unplaced.map((port) => (
                <Badge key={port.id} tone="muted">
                  {port.name}
                </Badge>
              ))}
            </div>
          ) : null}
        </Surface>

        <DutPanel dut={dut.data ?? {}} editable={can('operator')} />
      </PageBody>
    </Page>
  );
}

// ---------------------------------------------------------------------------
// Live numbers
// ---------------------------------------------------------------------------

/**
 * Per-flow rates from the statistics stream.
 *
 * This is the one place where samples are allowed to become React state. The
 * live charts refuse to do it because they redraw six hundred points; here the
 * payload is a handful of edge labels at one hertz, which is cheaper to
 * reconcile than to route around.
 */
function useLiveFlows(): Record<string, LiveFlow> {
  const selectors = useMemo(() => ['stream:*'], []);
  const stream = useStatsStream(selectors);
  const [flows, setFlows] = useState<Record<string, LiveFlow>>({});

  // Held in a ref so the subscription is registered once rather than on every
  // sample, which is what a state-dependent effect would do.
  const latest = useRef<Record<string, LiveFlow>>({});

  useEffect(
    () =>
      stream.subscribe((batch) => {
        const next: Record<string, LiveFlow> = {};
        for (const [id, sample] of Object.entries(batch.streams)) {
          next[id] = {
            txPps: sample.txPps,
            rxPps: sample.rxPps,
            lossPct: sample.lossPct,
          };
        }
        latest.current = next;
        setFlows(next);
      }),
    [stream],
  );

  // A closed stream leaves the last sample on screen forever, which would read
  // as traffic that is still flowing. Clearing on close says "not running".
  useEffect(() => {
    if (stream.status !== 'open') {
      latest.current = {};
      setFlows({});
    }
  }, [stream.status]);

  return flows;
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/** Reduces flows to their endpoints, dropping any whose config will not parse. */
function toLinks(flows: Flow[]): Link[] {
  return flows.flatMap((flow) => {
    const config = flowConfigSchema.safeParse(flow.config);
    if (!config.success) return [];
    return [
      {
        id: flow.id,
        name: flow.name,
        txPort: config.data.txPort,
        rxPort: config.data.rxPort,
      },
    ];
  });
}

/**
 * Two-colours the flow graph.
 *
 * Returns a side per port. Ports in no flow are absent from the map — they have
 * no side, because nothing has said which way they face.
 */
export function assignSides(links: Link[]): Map<string, 'left' | 'right'> {
  const neighbours = new Map<string, Set<string>>();
  const touch = (id: string) => {
    let set = neighbours.get(id);
    if (!set) {
      set = new Set();
      neighbours.set(id, set);
    }
    return set;
  };

  for (const link of links) {
    touch(link.txPort);
    touch(link.rxPort);
    // A loopback flow is its own neighbour, which no colouring can satisfy. It
    // is left out of the adjacency so it does not drag the whole component onto
    // one side; the edge is still drawn.
    if (link.txPort !== link.rxPort) {
      touch(link.txPort).add(link.rxPort);
      touch(link.rxPort).add(link.txPort);
    }
  }

  const sides = new Map<string, 'left' | 'right'>();

  // Seeding from transmitters first makes the common case read the way an
  // operator drew it on the whiteboard: sources on the left.
  const seeds = [...new Set(links.map((l) => l.txPort)), ...neighbours.keys()];

  for (const seed of seeds) {
    if (sides.has(seed)) continue;

    sides.set(seed, 'left');
    const queue = [seed];
    while (queue.length > 0) {
      const current = queue.shift() as string;
      const opposite = sides.get(current) === 'left' ? 'right' : 'left';
      for (const next of neighbours.get(current) ?? []) {
        if (sides.has(next)) continue;
        sides.set(next, opposite);
        queue.push(next);
      }
    }
  }

  return sides;
}

/** Builds the React Flow graph. */
function build(
  ports: Port[],
  links: Link[],
  dut: Dut,
  live: Record<string, LiveFlow>,
): { nodes: Node[]; edges: Edge[]; unplaced: Port[] } {
  const byId = new Map(ports.map((p) => [p.id, p]));
  const sides = assignSides(links);

  const left = ports.filter((p) => sides.get(p.id) === 'left');
  const right = ports.filter((p) => sides.get(p.id) === 'right');
  const unplaced = ports.filter((p) => !sides.has(p.id));

  const tallest = Math.max(left.length, right.length, 1);

  const portNodes: Node[] = [];
  const column = (list: Port[], side: 'left' | 'right') => {
    // Centred against the tallest column, so a two-against-four layout does not
    // hang off the top.
    const offset = ((tallest - list.length) * ROW_HEIGHT) / 2;
    list.forEach((port, i) => {
      portNodes.push({
        id: port.id,
        type: 'port',
        position: { x: COLUMN_X[side], y: offset + i * ROW_HEIGHT },
        data: { port, side },
        draggable: false,
      });
    });
  };
  column(left, 'left');
  column(right, 'right');

  const dutNode: Node = {
    id: '__dut__',
    type: 'dut',
    position: {
      x: COLUMN_X.dut,
      y: ((tallest - 1) * ROW_HEIGHT) / 2,
    },
    data: { dut },
    draggable: false,
    selectable: false,
  };

  const edges: Edge[] = links
    // A flow whose ports have been deleted has nothing to connect.
    .filter((link) => byId.has(link.txPort) && byId.has(link.rxPort))
    .map((link) => {
      const sample = live[link.id];
      const lossy = sample !== undefined && sample.lossPct > 0;

      return {
        id: link.id,
        type: 'flow',
        source: link.txPort,
        target: link.rxPort,
        animated: sample !== undefined && sample.txPps > 0,
        markerEnd: { type: MarkerType.ArrowClosed, color: lossy ? '#ff5d6c' : '#00d992' },
        style: { stroke: lossy ? '#ff5d6c' : '#00d992', strokeWidth: 1.6 },
        data: {
          label: sample
            ? `${formatPps(sample.rxPps)} · ${sample.lossPct.toFixed(2)}%`
            : link.name,
          lossy,
        },
      };
    });

  // Unattached ports are listed below the canvas rather than drawn on it: an
  // unconnected node in a diagram of connections is noise.
  return { nodes: [...portNodes, dutNode], edges, unplaced };
}

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

/** A port. */
function PortNode({ data }: NodeProps) {
  const { port, side } = data as unknown as { port: Port; side: 'left' | 'right' };

  return (
    <div className="topo-node">
      <Handle
        type="target"
        position={side === 'left' ? Position.Right : Position.Left}
        style={{ opacity: 0 }}
      />
      <Handle
        type="source"
        position={side === 'left' ? Position.Right : Position.Left}
        style={{ opacity: 0 }}
      />

      <div className="topo-node-title">{port.name}</div>
      <div className="topo-node-meta mono">{port.pciAddr}</div>
      <div className="row gap-6" style={{ marginTop: 6 }}>
        <LinkBadge state={port.linkState} />
        {port.group ? <Badge tone="info">{port.group.name}</Badge> : null}
      </div>
    </div>
  );
}

/** The box in the middle. */
function DutNode({ data }: NodeProps) {
  const { dut } = data as unknown as { dut: Dut };
  const title = dut.name ?? dut.model ?? 'Device under test';
  const subtitle = [dut.vendor, dut.name ? dut.model : undefined, dut.firmware]
    .filter(Boolean)
    .join(' · ');

  return (
    <div className="topo-node topo-dut">
      <Handle type="target" position={Position.Left} style={{ opacity: 0 }} />
      <Handle type="source" position={Position.Right} style={{ opacity: 0 }} />
      <Handle type="target" position={Position.Right} id="r" style={{ opacity: 0 }} />
      <Handle type="source" position={Position.Left} id="l" style={{ opacity: 0 }} />

      <div className="topo-node-title">{title}</div>
      <div className="topo-node-meta mono">{subtitle || 'not described'}</div>
    </div>
  );
}

const NODE_TYPES = { port: PortNode, dut: DutNode };

/**
 * A flow between two ports.
 *
 * A custom edge only because of where the label goes. The default edge writes
 * its label at the midpoint of the path — which in this diagram is exactly
 * where the device-under-test node sits, so every flow name was printed on top
 * of the DUT box. Drawing the label part-way along the run keeps it over the
 * wire instead.
 */
function FlowEdge({
  id,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  markerEnd,
  style,
  data,
}: EdgeProps) {
  const [path] = getBezierPath({
    sourceX,
    sourceY,
    sourcePosition,
    targetX,
    targetY,
    targetPosition,
  });

  const { label, lossy } = (data ?? {}) as { label?: string; lossy?: boolean };

  // 28% along the straight run: clear of the port node on one side and of the
  // DUT node in the middle. The common case is a horizontal edge, where the
  // straight-line point and the bezier point agree.
  const t = 0.28;
  const labelX = sourceX + (targetX - sourceX) * t;
  const labelY = sourceY + (targetY - sourceY) * t;

  return (
    <>
      <BaseEdge id={id} path={path} markerEnd={markerEnd} style={style} />
      {label ? (
        <EdgeLabelRenderer>
          <div
            className="topo-edge-label"
            style={{
              transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)`,
              color: lossy ? '#ff5d6c' : 'var(--qz-fg-2)',
            }}
          >
            {label}
          </div>
        </EdgeLabelRenderer>
      ) : null}
    </>
  );
}

const EDGE_TYPES = { flow: FlowEdge };

// ---------------------------------------------------------------------------
// The device under test
// ---------------------------------------------------------------------------

/** Written-out labels for the well-known DUT fields. */
const DUT_LABELS: Record<string, string> = {
  name: 'Name',
  vendor: 'Vendor',
  model: 'Model',
  firmware: 'Firmware',
  serial: 'Serial',
  notes: 'Notes',
};

/** Editor for the description carried into every report. */
function DutPanel({ dut, editable }: { dut: Dut; editable: boolean }) {
  const queryClient = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [draft, setDraft] = useState<[string, string][]>([]);

  // Reseeded whenever a new value arrives from the server, and not otherwise,
  // so a background refetch does not overwrite what is being typed.
  const seeded = useRef<Dut | null>(null);
  useEffect(() => {
    if (seeded.current === dut) return;
    seeded.current = dut;

    const extras = Object.entries(dut).filter(
      ([key]) => !(DUT_FIELDS as readonly string[]).includes(key),
    );
    setDraft([...DUT_FIELDS.map((key) => [key, dut[key] ?? ''] as [string, string]), ...extras]);
  }, [dut]);

  const save = useMutation({
    mutationFn: () => api.topology.setDut(Object.fromEntries(draft)),
    onSuccess: () => {
      setError(null);
      setSaved(true);
      void queryClient.invalidateQueries({ queryKey: queryKeys.dut });
    },
    onError: (e) => {
      setSaved(false);
      setError(e instanceof ApiError ? describe(e) : 'The description could not be saved.');
    },
  });

  const set = useCallback((index: number, key: string, value: string) => {
    setSaved(false);
    setDraft((rows) => rows.map((row, i) => (i === index ? [key, value] : row)));
  }, []);

  return (
    <Surface
      title="Device under test"
      actions={
        editable ? (
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            onClick={() => {
              setSaved(false);
              setDraft((rows) => [...rows, ['', '']]);
            }}
          >
            <Plus size={14} />
            Add field
          </button>
        ) : null
      }
    >
      <div className="stack gap-14">
        <p className="prose">
          Copied into every run started from here on, and printed on its report. A run keeps the
          description it was started with, so changing this does not rewrite history.
        </p>

        {error ? <Alert tone="danger">{error}</Alert> : null}

        <div className="stack gap-8">
          {draft.map(([key, value], index) => {
            const named = (DUT_FIELDS as readonly string[]).includes(key);
            const valueId = `dut-field-${index}`;
            return (
              <div
                key={index}
                style={{ display: 'grid', gridTemplateColumns: '180px 1fr', gap: 10 }}
              >
                {named ? (
                  <label
                    className="field-label"
                    htmlFor={valueId}
                    style={{ alignSelf: 'center' }}
                  >
                    {DUT_LABELS[key] ?? key}
                  </label>
                ) : (
                  <input
                    className="input"
                    placeholder="field name"
                    aria-label="Field name"
                    value={key}
                    disabled={!editable}
                    onChange={(e) => set(index, e.target.value, value)}
                  />
                )}
                <input
                  id={valueId}
                  className="input"
                  placeholder={editable ? 'not set' : '—'}
                  aria-label={named ? undefined : 'Field value'}
                  value={value}
                  disabled={!editable}
                  onChange={(e) => set(index, key, e.target.value)}
                />
              </div>
            );
          })}
        </div>

        {editable ? (
          <div className="row gap-12">
            <button
              type="button"
              className="btn btn-primary"
              disabled={save.isPending}
              onClick={() => save.mutate()}
            >
              {save.isPending ? 'Saving…' : 'Save'}
            </button>
            {saved ? <p className="note">Saved.</p> : null}
          </div>
        ) : (
          <p className="note">Operators and administrators can edit this.</p>
        )}
      </div>
    </Surface>
  );
}

/** Turns an API failure into a line an operator can act on. */
function describe(error: ApiError): string {
  if (error.fieldErrors.length > 0) {
    return error.fieldErrors.map((e) => e.msg).join('; ');
  }
  return error.message;
}
