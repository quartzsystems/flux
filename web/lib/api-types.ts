/**
 * Zod mirrors of the Rust API types.
 *
 * Every type here corresponds to a `serde` struct in `fluxd`, and the two are
 * kept in sync by hand. Each schema names its Rust counterpart so a change on
 * one side has an obvious place to land on the other.
 *
 * Responses are parsed rather than cast. An appliance can be running a `fluxd`
 * newer or older than the UI bundle a browser has cached, and a silent shape
 * mismatch there surfaces as `undefined` deep inside a render — a parse failure
 * at the boundary says what actually went wrong.
 */

import { z } from 'zod';

// ---------------------------------------------------------------------------
// Enumerations — flux_core::types
// ---------------------------------------------------------------------------

/** `flux_core::types::Role` */
export const roleSchema = z.enum(['viewer', 'operator', 'admin']);
export type Role = z.infer<typeof roleSchema>;

/** Privilege order, matching `Role`'s discriminants in Rust. */
const ROLE_RANK: Record<Role, number> = { viewer: 0, operator: 1, admin: 2 };

/** True when `role` is at least as privileged as `required`. */
export function hasRole(role: Role | undefined, required: Role): boolean {
  return role !== undefined && ROLE_RANK[role] >= ROLE_RANK[required];
}

/** `flux_core::types::PortMode` */
export const portModeSchema = z.enum(['kernel', 'dpdk']);
export type PortMode = z.infer<typeof portModeSchema>;

/** `flux_core::types::LinkState` */
export const linkStateSchema = z.enum(['up', 'down', 'unknown']);
export type LinkState = z.infer<typeof linkStateSchema>;

/** `flux_core::types::EngineMode` */
export const engineModeSchema = z.enum(['stl', 'astf']);
export type EngineMode = z.infer<typeof engineModeSchema>;

/** `flux_core::types::PortGroupState` */
export const portGroupStateSchema = z.enum(['stopped', 'starting', 'ready', 'error']);
export type PortGroupState = z.infer<typeof portGroupStateSchema>;

/** `flux_core::port::HugepageSize` */
export const hugepageSizeSchema = z.enum(['2M', '1G']);
export type HugepageSize = z.infer<typeof hugepageSizeSchema>;

// ---------------------------------------------------------------------------
// Errors — fluxd::api::error
// ---------------------------------------------------------------------------

/** `flux_core::config::FieldError` */
export const fieldErrorSchema = z.object({
  path: z.string(),
  msg: z.string(),
});
export type FieldError = z.infer<typeof fieldErrorSchema>;

/** `fluxd::api::error::ErrorBody` */
export const errorBodySchema = z.object({
  code: z.string(),
  message: z.string(),
  errors: z.array(fieldErrorSchema).default([]),
});
export type ErrorBody = z.infer<typeof errorBodySchema>;

// ---------------------------------------------------------------------------
// Auth — fluxd::api::auth
// ---------------------------------------------------------------------------

/** `fluxd::api::auth::MeResponse` */
export const meSchema = z.object({
  id: z.string(),
  username: z.string(),
  role: roleSchema,
});
export type Me = z.infer<typeof meSchema>;

/** `fluxd::api::auth::LoginRequest` */
export interface LoginRequest {
  username: string;
  password: string;
}

// ---------------------------------------------------------------------------
// Users — fluxd::store::models::UserView
// ---------------------------------------------------------------------------

/** `fluxd::store::models::UserView` */
export const userSchema = z.object({
  id: z.string(),
  username: z.string(),
  role: roleSchema,
  createdAt: z.string(),
  lastLoginAt: z.string().nullable(),
});
export type User = z.infer<typeof userSchema>;

/** `fluxd::api::users::CreateUser` */
export interface CreateUserRequest {
  username: string;
  password: string;
  role: Role;
}

/** `fluxd::api::users::UpdateUser` */
export interface UpdateUserRequest {
  role?: Role;
  password?: string;
}

// ---------------------------------------------------------------------------
// Ports — fluxd::store::models
// ---------------------------------------------------------------------------

/** `fluxd::store::models::PortGroupRef` */
export const portGroupRefSchema = z.object({
  id: z.string(),
  name: z.string(),
  engineMode: engineModeSchema,
  state: portGroupStateSchema,
  index: z.number(),
});
export type PortGroupRef = z.infer<typeof portGroupRefSchema>;

/** `fluxd::store::models::ReservationView` */
export const reservationSchema = z.object({
  id: z.string(),
  portId: z.string(),
  userId: z.string(),
  username: z.string(),
  note: z.string(),
  expiresAt: z.string(),
});
export type Reservation = z.infer<typeof reservationSchema>;

/** `fluxd::store::models::PortView` */
export const portSchema = z.object({
  id: z.string(),
  name: z.string(),
  pciAddr: z.string(),
  description: z.string(),
  driver: z.string().nullable(),
  ifname: z.string().nullable(),
  mac: z.string().nullable(),
  speedMbps: z.number().nullable(),
  numaNode: z.number().nullable(),
  mode: portModeSchema,
  linkState: linkStateSchema,
  present: z.boolean(),
  group: portGroupRefSchema.nullable(),
  reservation: reservationSchema.nullable(),
  updatedAt: z.string(),
});
export type Port = z.infer<typeof portSchema>;

/** `fluxd::api::ports::PortUpdate` */
export interface PortUpdate {
  name?: string;
  mode?: PortMode;
}

/** `fluxd::api::ports::ReserveRequest` */
export interface ReserveRequest {
  note: string;
  hours?: number;
}

// ---------------------------------------------------------------------------
// Port groups — fluxd::api::port_groups
// ---------------------------------------------------------------------------

/** `fluxd::api::port_groups::PortGroupView` */
export const portGroupSchema = z.object({
  id: z.string(),
  name: z.string(),
  engineMode: engineModeSchema,
  state: portGroupStateSchema,
  trexCfg: z.unknown(),
  error: z.string().nullable(),
  createdAt: z.string(),
  updatedAt: z.string(),
  portIds: z.array(z.string()),
});
export type PortGroup = z.infer<typeof portGroupSchema>;

// ---------------------------------------------------------------------------
// System — fluxd::api::system
// ---------------------------------------------------------------------------

/** `flux_core::port::HugepagePool` */
export const hugepagePoolSchema = z.object({
  size: hugepageSizeSchema,
  node: z.number().nullable(),
  total: z.number(),
  free: z.number(),
});
export type HugepagePool = z.infer<typeof hugepagePoolSchema>;

/** `flux_core::port::HugepagesStatus` */
export const hugepagesStatusSchema = z.object({
  pools: z.array(hugepagePoolSchema),
  sufficient: z.boolean(),
});
export type HugepagesStatus = z.infer<typeof hugepagesStatusSchema>;

/** `fluxd::api::system::SubsystemHealth` */
export const subsystemHealthSchema = z.object({
  backend: z.string(),
  ok: z.boolean(),
  detail: z.string().nullable(),
});
export type SubsystemHealth = z.infer<typeof subsystemHealthSchema>;

/** `fluxd::api::system::PortCounts` */
export const portCountsSchema = z.object({
  total: z.number(),
  up: z.number(),
  down: z.number(),
  unknown: z.number(),
});
export type PortCounts = z.infer<typeof portCountsSchema>;

/** `fluxd::api::system::DiskUsage` */
export const diskUsageSchema = z.object({
  mount: z.string(),
  totalBytes: z.number(),
  availableBytes: z.number(),
});
export type DiskUsage = z.infer<typeof diskUsageSchema>;

/** `fluxd::api::system::Health` */
export const healthSchema = z.object({
  version: z.string(),
  uptimeSecs: z.number(),
  healthy: z.boolean(),
  mocked: z.boolean(),
  engine: subsystemHealthSchema,
  portd: subsystemHealthSchema,
  database: subsystemHealthSchema,
  hugepages: hugepagesStatusSchema.nullable(),
  ports: portCountsSchema,
  disks: z.array(diskUsageSchema),
  memoryTotalBytes: z.number(),
  memoryAvailableBytes: z.number(),
  engineInstances: z.number(),
  collectorsActive: z.number(),
  activeRuns: z.number(),
});
export type Health = z.infer<typeof healthSchema>;

// ---------------------------------------------------------------------------
// Flows — flux_core::flow
// ---------------------------------------------------------------------------

/** `flux_core::flow::EthernetFields` */
export const ethernetFieldsSchema = z.object({
  src: z.string(),
  dst: z.string(),
  ethertype: z.number().optional(),
});

/** `flux_core::flow::VlanFields` */
export const vlanFieldsSchema = z.object({
  id: z.number(),
  pcp: z.number().default(0),
  dei: z.boolean().default(false),
  tpid: z.number().default(0x8100),
});

/** `flux_core::flow::Ipv4Fields` */
export const ipv4FieldsSchema = z.object({
  src: z.string(),
  dst: z.string(),
  ttl: z.number().default(64),
  dscp: z.number().default(0),
  ecn: z.number().default(0),
  identification: z.number().default(0),
  dontFragment: z.boolean().default(false),
  protocol: z.number().optional(),
});

/** `flux_core::flow::Ipv6Fields` */
export const ipv6FieldsSchema = z.object({
  src: z.string(),
  dst: z.string(),
  hopLimit: z.number().default(64),
  trafficClass: z.number().default(0),
  flowLabel: z.number().default(0),
  nextHeader: z.number().optional(),
});

/** `flux_core::flow::TcpFields` */
export const tcpFieldsSchema = z.object({
  srcPort: z.number(),
  dstPort: z.number(),
  seq: z.number().default(0),
  ack: z.number().default(0),
  flags: z.number().default(2),
  window: z.number().default(8192),
});

/** `flux_core::flow::UdpFields` */
export const udpFieldsSchema = z.object({
  srcPort: z.number(),
  dstPort: z.number(),
});

/** `flux_core::flow::CustomFields` */
export const customFieldsSchema = z.object({ hex: z.string() });

/**
 * `flux_core::flow::HeaderLayer`
 *
 * A discriminated union on `proto`, matching serde's `tag`/`content` encoding.
 */
export const headerLayerSchema = z.discriminatedUnion('proto', [
  z.object({ proto: z.literal('ethernet'), fields: ethernetFieldsSchema }),
  z.object({ proto: z.literal('vlan'), fields: vlanFieldsSchema }),
  z.object({ proto: z.literal('ipv4'), fields: ipv4FieldsSchema }),
  z.object({ proto: z.literal('ipv6'), fields: ipv6FieldsSchema }),
  z.object({ proto: z.literal('tcp'), fields: tcpFieldsSchema }),
  z.object({ proto: z.literal('udp'), fields: udpFieldsSchema }),
  z.object({ proto: z.literal('custom'), fields: customFieldsSchema }),
]);
export type HeaderLayer = z.infer<typeof headerLayerSchema>;

/** The protocols a header layer can be. */
export type HeaderProto = HeaderLayer['proto'];

/** `flux_core::flow::ImixPreset` */
export const imixPresetSchema = z.enum(['simple', 'tolly']);
export type ImixPreset = z.infer<typeof imixPresetSchema>;

/** `flux_core::flow::FrameSize` */
export const frameSizeSchema = z.discriminatedUnion('type', [
  z.object({ type: z.literal('fixed'), bytes: z.number() }),
  z.object({ type: z.literal('imix'), preset: imixPresetSchema }),
  z.object({ type: z.literal('random'), min: z.number(), max: z.number() }),
]);
export type FrameSize = z.infer<typeof frameSizeSchema>;

/** `flux_core::flow::Rate` */
export const rateSchema = z.discriminatedUnion('type', [
  z.object({ type: z.literal('pps'), value: z.number() }),
  z.object({ type: z.literal('bps'), value: z.number() }),
  z.object({ type: z.literal('percent'), value: z.number() }),
]);
export type Rate = z.infer<typeof rateSchema>;

/** `flux_core::flow::ModifierField` */
export const modifierFieldSchema = z.enum([
  'eth_src',
  'eth_dst',
  'vlan_id',
  'ipv4_src',
  'ipv4_dst',
  'ipv6_src',
  'ipv6_dst',
  'l4_src_port',
  'l4_dst_port',
]);
export type ModifierField = z.infer<typeof modifierFieldSchema>;

/** `flux_core::flow::ModifierMode` */
export const modifierModeSchema = z.enum(['increment', 'random']);
export type ModifierMode = z.infer<typeof modifierModeSchema>;

/** `flux_core::flow::Modifier` */
export const modifierSchema = z.object({
  field: modifierFieldSchema,
  mode: modifierModeSchema,
  count: z.number(),
  step: z.number().default(1),
});
export type Modifier = z.infer<typeof modifierSchema>;

/** `flux_core::flow::FlowConfig` */
export const flowConfigSchema = z.object({
  txPort: z.string(),
  rxPort: z.string(),
  headers: z.array(headerLayerSchema),
  size: frameSizeSchema,
  rate: rateSchema,
  modifiers: z.array(modifierSchema).default([]),
  durationSecs: z.number().nullable().optional(),
  latencyTrack: z.boolean().default(false),
});
export type FlowConfig = z.infer<typeof flowConfigSchema>;

/** `fluxd::store::models::Flow` */
export const flowSchema = z.object({
  id: z.string(),
  name: z.string(),
  // Parsed leniently: a flow written by a newer daemon should still list.
  config: z.unknown(),
  createdBy: z.string().nullable(),
  createdAt: z.string(),
  updatedAt: z.string(),
});
export type Flow = z.infer<typeof flowSchema>;

/** `fluxd::api::flows::FlowInput` */
export interface FlowInput {
  name: string;
  config: FlowConfig;
}

/** `flux_core::rate::ResolvedRate` */
export const resolvedRateSchema = z.object({
  pps: z.number(),
  bpsL1: z.number(),
  bpsL2: z.number(),
  linePct: z.number(),
});
export type ResolvedRate = z.infer<typeof resolvedRateSchema>;

/** `fluxd::api::flows::FramePreview` */
export const framePreviewSchema = z.object({
  wireLen: z.number(),
  bytes: z.array(z.number()),
  hexDump: z.string(),
});
export type FramePreview = z.infer<typeof framePreviewSchema>;

/** `fluxd::api::flows::FlowPreview` */
export const flowPreviewSchema = z.object({
  frames: z.array(framePreviewSchema),
  headerBytes: z.number(),
  rate: resolvedRateSchema,
  portSpeedMbps: z.number(),
  exceedsLineRate: z.boolean(),
  variantCount: z.number(),
  summary: z.string(),
});
export type FlowPreview = z.infer<typeof flowPreviewSchema>;

// ---------------------------------------------------------------------------
// Tests — fluxd::api::tests
// ---------------------------------------------------------------------------

/** `flux_core::types::TestType` */
export const testTypeSchema = z.enum([
  'manual',
  'rfc2544_throughput',
  'rfc2544_latency',
  'rfc2544_frameloss',
  'rfc2544_b2b',
]);
export type TestType = z.infer<typeof testTypeSchema>;

/** `fluxd::store::models::Test` */
export const testSchema = z.object({
  id: z.string(),
  name: z.string(),
  type: testTypeSchema,
  config: z.unknown(),
  flowIds: z.array(z.string()),
  // A test drives flows or profiles, never both — the two are programmed
  // through different engine calls and an instance is in one mode or the other.
  profileIds: z.array(z.string()).default([]),
  createdBy: z.string().nullable(),
  createdAt: z.string(),
  updatedAt: z.string(),
});
export type Test = z.infer<typeof testSchema>;

/** `fluxd::api::tests::TestInput` */
export interface TestInput {
  name: string;
  type: TestType;
  config: Record<string, unknown>;
  flowIds: string[];
  profileIds?: string[];
}

/** The frame sizes RFC 2544 section 9 names for Ethernet. */
export const STANDARD_FRAME_SIZES = [64, 128, 256, 512, 1024, 1280, 1518] as const;

/** Trial duration RFC 2544 section 24 requires for a reportable result. */
export const REPORTABLE_TRIAL_SECONDS = 60;

/**
 * `flux_core::rfc2544::Rfc2544Config`
 *
 * Defaults mirror the Rust `Default` impl. They are duplicated so a wizard can
 * open populated without a round trip; the Rust side stays authoritative and
 * validates anything saved.
 */
export const rfc2544ConfigSchema = z.object({
  frameSizes: z.array(z.number()).default([...STANDARD_FRAME_SIZES]),
  trialSeconds: z.number().default(REPORTABLE_TRIAL_SECONDS),
  lossTolerancePct: z.number().default(0),
  maxIterations: z.number().default(20),
  initialRatePct: z.number().default(100),
  resolutionPct: z.number().default(0.1),
  ladderStepPct: z.number().default(10),
  minRatePct: z.number().default(10),
  maxBurstFrames: z.number().default(1_000_000),
  burstResolutionFrames: z.number().default(100),
});
export type Rfc2544Config = z.infer<typeof rfc2544ConfigSchema>;

/** A fresh benchmark configuration with the standard frame sizes. */
export function defaultRfc2544Config(): Rfc2544Config {
  return rfc2544ConfigSchema.parse({});
}

/**
 * Why a configuration would not produce a conformant RFC 2544 result.
 *
 * Mirrors `Rfc2544Config::reportability_notes`. Shown in the wizard so an
 * operator learns about it before spending an hour on the run rather than when
 * they read the report.
 */
export function reportabilityNotes(config: Rfc2544Config): string[] {
  const notes: string[] = [];

  if (config.trialSeconds < REPORTABLE_TRIAL_SECONDS) {
    notes.push(
      `Trial duration is ${config.trialSeconds}s; RFC 2544 §24 requires at least ${REPORTABLE_TRIAL_SECONDS}s.`,
    );
  }
  if (config.lossTolerancePct > 0) {
    notes.push(
      `Loss tolerance is ${config.lossTolerancePct}%; RFC 2544 throughput is defined at zero loss.`,
    );
  }

  const missing = STANDARD_FRAME_SIZES.filter((s) => !config.frameSizes.includes(s));
  if (missing.length > 0) {
    notes.push(`Frame sizes ${missing.join(', ')} from RFC 2544 §9 were not selected.`);
  }

  return notes;
}

/** `fluxd::api::flows::PcapImport` */
export const pcapImportSchema = z.object({
  headers: z.array(headerLayerSchema),
  capturedLen: z.number(),
  originalLen: z.number(),
  notes: z.array(z.string()),
});
export type PcapImport = z.infer<typeof pcapImportSchema>;

// ---------------------------------------------------------------------------
// Runs — fluxd::api::runs
// ---------------------------------------------------------------------------

/** `flux_core::types::RunState` */
export const runStateSchema = z.enum([
  'pending',
  'validating',
  'preparing',
  'running',
  'analyzing',
  'complete',
  'failed',
  'cancelled',
]);
export type RunState = z.infer<typeof runStateSchema>;

/** True when a run can no longer change state. */
export function isTerminal(state: RunState): boolean {
  return state === 'complete' || state === 'failed' || state === 'cancelled';
}

/** `fluxd::store::models::Run` */
export const runSchema = z.object({
  id: z.string(),
  testId: z.string().nullable(),
  testName: z.string(),
  type: z.string(),
  state: runStateSchema,
  startedBy: z.string().nullable(),
  startedAt: z.string(),
  finishedAt: z.string().nullable(),
  dutMeta: z.unknown(),
  configSnapshot: z.unknown(),
  error: z.string().nullable(),
});
export type Run = z.infer<typeof runSchema>;

/** `fluxd::store::models::RunResult` */
export const runResultSchema = z.object({
  id: z.string(),
  runId: z.string(),
  iteration: z.number(),
  frameSize: z.number().nullable(),
  params: z.unknown(),
  metrics: z.unknown(),
  passed: z.boolean(),
  createdAt: z.string(),
});
export type RunResult = z.infer<typeof runResultSchema>;

/** `fluxd::api::runs::RunPage` */
export const runPageSchema = z.object({
  runs: z.array(runSchema),
  total: z.number(),
  limit: z.number(),
  offset: z.number(),
});
export type RunPage = z.infer<typeof runPageSchema>;

/** `fluxd::api::runs::RunDetail` */
export const runDetailSchema = runSchema.extend({
  results: z.array(runResultSchema),
  stoppable: z.boolean(),
});
export type RunDetail = z.infer<typeof runDetailSchema>;

/** The metrics object a manual run records per flow. */
export const manualMetricsSchema = z.object({
  txPackets: z.number().default(0),
  rxPackets: z.number().default(0),
  lostPackets: z.number().default(0),
  lossPct: z.number().default(0),
  latMinUs: z.number().nullable().default(null),
  latAvgUs: z.number().nullable().default(null),
  latMaxUs: z.number().nullable().default(null),
  latP50: z.number().nullable().default(null),
  latP99: z.number().nullable().default(null),
  latP999: z.number().nullable().default(null),
  jitterUs: z.number().nullable().default(null),
});
export type ManualMetrics = z.infer<typeof manualMetricsSchema>;

// ---------------------------------------------------------------------------
// Live statistics — fluxd::collector
// ---------------------------------------------------------------------------

/** `flux_core::engine::LatencyStats` */
export const latencyStatsSchema = z.object({
  minUs: z.number().nullable(),
  avgUs: z.number().nullable(),
  maxUs: z.number().nullable(),
  p50Us: z.number().nullable(),
  p99Us: z.number().nullable(),
  p999Us: z.number().nullable(),
  jitterUs: z.number().nullable(),
});
export type LatencyStats = z.infer<typeof latencyStatsSchema>;

/** `fluxd::collector::PortSample` */
export const portSampleSchema = z.object({
  txPps: z.number(),
  rxPps: z.number(),
  txBps: z.number(),
  rxBps: z.number(),
  txPackets: z.number(),
  rxPackets: z.number(),
  txErrors: z.number(),
  rxErrors: z.number(),
});
export type PortSample = z.infer<typeof portSampleSchema>;

/** `fluxd::collector::StreamSample` */
export const streamSampleSchema = z.object({
  txPps: z.number(),
  rxPps: z.number(),
  lossPps: z.number(),
  lossPct: z.number(),
  txPackets: z.number(),
  rxPackets: z.number(),
  latency: latencyStatsSchema,
});
export type StreamSample = z.infer<typeof streamSampleSchema>;

/** `fluxd::collector::RunProgress` */
export const runProgressSchema = z.object({
  runId: z.string(),
  state: z.string(),
  iteration: z.number().optional(),
  frameSize: z.number().optional(),
  trialRatePct: z.number().optional(),
  trialRemainingSecs: z.number().optional(),
  progress: z.number().optional(),
  message: z.string().optional(),
});
export type RunProgress = z.infer<typeof runProgressSchema>;

/** `fluxd::collector::StatsBatch` */
/** `fluxd::collector::ConnectionSample` */
export const connectionSampleSchema = z.object({
  cps: z.number(),
  errorsPerSec: z.number(),
  active: z.number(),
  attempted: z.number(),
  established: z.number(),
  connectErrors: z.number(),
  failurePct: z.number(),
  txBps: z.number(),
  rxBps: z.number(),
});
export type ConnectionSample = z.infer<typeof connectionSampleSchema>;

export const statsBatchSchema = z.object({
  ts: z.number(),
  ports: z.record(z.string(), portSampleSchema),
  streams: z.record(z.string(), streamSampleSchema),
  run: runProgressSchema.optional(),
  // Present only for a stateful load, which is what tells the run view to chart
  // connections instead of frames.
  connections: connectionSampleSchema.optional(),
});
export type StatsBatch = z.infer<typeof statsBatchSchema>;

/** Control frames the stream sends alongside batches. */
export const streamControlSchema = z.discriminatedUnion('type', [
  z.object({ type: z.literal('subscribed'), selectors: z.array(z.string()), backfill: z.number() }),
  z.object({ type: z.literal('error'), message: z.string() }),
]);
export type StreamControl = z.infer<typeof streamControlSchema>;

// ---------------------------------------------------------------------------
// Load profiles — flux_core::profile
// ---------------------------------------------------------------------------

/** `flux_core::profile::IpPool` */
export const ipPoolSchema = z.object({
  cidr: z.string(),
  portMin: z.number().default(1024),
  portMax: z.number().default(65535),
});
export type IpPool = z.infer<typeof ipPoolSchema>;

/** `flux_core::profile::AppSpec` */
export const appSpecSchema = z.discriminatedUnion('type', [
  z.object({
    type: z.literal('http_get'),
    path: z.string().default('/'),
    responseBytes: z.number().default(32_768),
  }),
  z.object({
    type: z.literal('raw'),
    requestBytes: z.number(),
    responseBytes: z.number(),
  }),
  z.object({ type: z.literal('pcap'), pcapRef: z.string() }),
]);
export type AppSpec = z.infer<typeof appSpecSchema>;

/** `flux_core::profile::Ramp` */
export const rampSchema = z.object({
  warmupSecs: z.number().default(10),
  settleSecs: z.number().default(5),
});
export type Ramp = z.infer<typeof rampSchema>;

/** `flux_core::profile::LoadProfileConfig` */
export const loadProfileConfigSchema = z.object({
  clientPort: z.string(),
  serverPort: z.string(),
  clientPool: ipPoolSchema,
  serverPool: ipPoolSchema,
  app: appSpecSchema,
  targetCps: z.number(),
  maxConcurrent: z.number(),
  ramp: rampSchema.default({ warmupSecs: 10, settleSecs: 5 }),
  durationSecs: z.number().nullable().optional(),
});
export type LoadProfileConfig = z.infer<typeof loadProfileConfigSchema>;

/** `fluxd::store::models::LoadProfile` */
export const loadProfileSchema = z.object({
  id: z.string(),
  name: z.string(),
  config: z.unknown(),
  createdBy: z.string().nullable(),
  createdAt: z.string(),
  updatedAt: z.string(),
});
export type LoadProfile = z.infer<typeof loadProfileSchema>;

/** `fluxd::api::profiles::ProfileInput` */
export interface ProfileInput {
  name: string;
  config: LoadProfileConfig;
}

/** `fluxd::api::profiles::ProfilePreview` */
export const profilePreviewSchema = z.object({
  clientCapacity: z.number(),
  serverAddresses: z.number(),
  bytesPerConnection: z.number(),
  impliedBps: z.number(),
  clientPortSpeedMbps: z.number(),
  exceedsLineRate: z.boolean(),
  measurementStartsAt: z.number(),
  summary: z.string(),
});
export type ProfilePreview = z.infer<typeof profilePreviewSchema>;

/** A new profile with sensible starting values. */
export function defaultLoadProfile(clientPort: string, serverPort: string): LoadProfileConfig {
  return {
    clientPort,
    serverPort,
    clientPool: { cidr: '16.0.0.0/16', portMin: 1024, portMax: 65535 },
    serverPool: { cidr: '48.0.0.0/24', portMin: 80, portMax: 80 },
    app: { type: 'http_get', path: '/', responseBytes: 32_768 },
    // A thousand connections a second is enough to see the pipeline work
    // without saturating anything an operator forgot to check.
    targetCps: 1_000,
    maxConcurrent: 100_000,
    ramp: { warmupSecs: 10, settleSecs: 5 },
    durationSecs: null,
  };
}

/** The metrics a load run records. */
export const loadMetricsSchema = z.object({
  attempted: z.number().default(0),
  established: z.number().default(0),
  closed: z.number().default(0),
  active: z.number().default(0),
  connectErrors: z.number().default(0),
  resets: z.number().default(0),
  failurePct: z.number().default(0),
  txBytes: z.number().default(0),
  rxBytes: z.number().default(0),
});
export type LoadMetrics = z.infer<typeof loadMetricsSchema>;

// ---------------------------------------------------------------------------
// Analytics — fluxd::api::analytics
// ---------------------------------------------------------------------------

/** `fluxd::api::analytics::MetricInfo` */
export const metricInfoSchema = z.object({
  name: z.string(),
  label: z.string(),
  unit: z.string(),
});
export type MetricInfo = z.infer<typeof metricInfoSchema>;

/** `fluxd::api::analytics::Series` */
export const analyticsSeriesSchema = z.object({
  labels: z.record(z.string(), z.string()),
  timestamps: z.array(z.number()),
  values: z.array(z.number().nullable()),
});
export type AnalyticsSeries = z.infer<typeof analyticsSeriesSchema>;

/** `fluxd::api::analytics::QueryResult` */
export const analyticsResultSchema = z.object({
  metric: z.string(),
  unit: z.string(),
  step: z.number(),
  series: z.array(analyticsSeriesSchema),
});
export type AnalyticsResult = z.infer<typeof analyticsResultSchema>;

// ---------------------------------------------------------------------------
// Settings — fluxd::api::settings
// ---------------------------------------------------------------------------

/** `fluxd::store::models::Setting` */
export const settingSchema = z.object({
  key: z.string(),
  value: z.unknown(),
  updatedAt: z.string(),
});
export type Setting = z.infer<typeof settingSchema>;

/** The `tls` setting's payload. */
export const tlsSettingSchema = z.object({
  enabled: z.boolean().default(false),
  certPath: z.string().nullable().default(null),
  keyPath: z.string().nullable().default(null),
  subject: z.string().nullable().default(null),
  notAfter: z.string().nullable().default(null),
});
export type TlsSetting = z.infer<typeof tlsSettingSchema>;

/** The `retention` setting's payload. */
export const retentionSettingSchema = z.object({
  runDays: z.number().default(90),
  seriesDays: z.number().default(30),
});
export type RetentionSetting = z.infer<typeof retentionSettingSchema>;

/** The `appliance` setting's payload. */
export const applianceSettingSchema = z.object({
  hostname: z.string().nullable().default(null),
  location: z.string().nullable().default(null),
  contact: z.string().nullable().default(null),
});
export type ApplianceSetting = z.infer<typeof applianceSettingSchema>;

/** `fluxd::api::settings::ImportSummary` */
export const importSummarySchema = z.object({
  flowsCreated: z.number(),
  flowsSkipped: z.number(),
  profilesCreated: z.number(),
  profilesSkipped: z.number(),
  testsCreated: z.number(),
  testsSkipped: z.number(),
  problems: z.array(z.string()),
});
export type ImportSummary = z.infer<typeof importSummarySchema>;

/**
 * `fluxd::api::topology::Dut`
 *
 * Free-form pairs rather than named fields, so that what identifies a device
 * can be recorded in the operator's own terms. The topology page offers the
 * conventional ones and lets anything else be added.
 */
export const dutSchema = z.record(z.string(), z.string());
export type Dut = z.infer<typeof dutSchema>;

/** The fields the DUT editor offers by name, in the order it shows them. */
export const DUT_FIELDS = ['name', 'vendor', 'model', 'firmware', 'serial', 'notes'] as const;
