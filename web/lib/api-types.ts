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
});
export type Health = z.infer<typeof healthSchema>;
