/**
 * The REST client.
 *
 * One `request` function does all the work: it sends the session cookie, parses
 * the response through its zod schema, and converts every failure into an
 * `ApiError` carrying the field-level detail the forms need. Callers get either
 * a parsed value or a typed throw — never a half-checked object.
 */

import { z } from 'zod';

import {
  errorBodySchema,
  flowPreviewSchema,
  flowSchema,
  healthSchema,
  hugepagesStatusSchema,
  meSchema,
  portGroupSchema,
  portSchema,
  reservationSchema,
  runDetailSchema,
  runPageSchema,
  testSchema,
  userSchema,
  type CreateUserRequest,
  type FieldError,
  type FlowInput,
  type HugepageSize,
  type LoginRequest,
  type PortUpdate,
  type ReserveRequest,
  type RunState,
  type TestInput,
  type UpdateUserRequest,
} from './api-types';

/** Prefix every endpoint shares. Same origin in production, proxied in dev. */
const BASE = '/api/v1';

/**
 * A failed request.
 *
 * `fieldErrors` is what a form reads to attach a message to the input that
 * caused it; `status` is what routing decisions read (401 means log in again).
 */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly fieldErrors: FieldError[];

  constructor(status: number, code: string, message: string, fieldErrors: FieldError[] = []) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.fieldErrors = fieldErrors;
  }

  /** True when the session is missing or expired. */
  get isUnauthorized(): boolean {
    return this.status === 401;
  }

  /** The message for `path`, if the server reported one. */
  fieldError(path: string): string | undefined {
    return this.fieldErrors.find((e) => e.path === path)?.msg;
  }
}

/** Options for a single request. */
interface RequestOptions<T> {
  method?: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';
  body?: unknown;
  /** Schema the response is parsed through. Omit to ignore the body. */
  schema?: z.ZodType<T>;
  signal?: AbortSignal;
}

/**
 * Issues one request and parses its response.
 *
 * `credentials: 'same-origin'` rather than `'include'`: the session cookie is
 * first-party, and `'include'` would also send it to any cross-origin URL a
 * future caller passed in.
 */
async function request<T>(path: string, options: RequestOptions<T> = {}): Promise<T> {
  const { method = 'GET', body, schema, signal } = options;

  let response: Response;
  try {
    response = await fetch(`${BASE}${path}`, {
      method,
      credentials: 'same-origin',
      headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal,
    });
  } catch (cause) {
    // fetch only rejects for transport failures, which on an appliance almost
    // always means the daemon restarted or the network path dropped.
    if (cause instanceof DOMException && cause.name === 'AbortError') throw cause;
    throw new ApiError(0, 'network', 'Could not reach the appliance.');
  }

  if (!response.ok) {
    throw await toApiError(response);
  }

  if (!schema) {
    return undefined as T;
  }

  const payload: unknown = await response.json();
  const parsed = schema.safeParse(payload);
  if (!parsed.success) {
    // Reaching here means fluxd and this bundle disagree about a response shape.
    console.error(`Unexpected response shape from ${path}`, parsed.error.issues, payload);
    throw new ApiError(
      response.status,
      'schema_mismatch',
      'The appliance returned data this interface does not understand. It may be running a different version.',
    );
  }
  return parsed.data;
}

/** Builds an `ApiError` from a non-2xx response. */
async function toApiError(response: Response): Promise<ApiError> {
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    // A non-JSON error body means something upstream of the handler failed.
    return new ApiError(response.status, 'unknown', response.statusText || 'Request failed.');
  }

  const parsed = errorBodySchema.safeParse(body);
  if (!parsed.success) {
    return new ApiError(response.status, 'unknown', response.statusText || 'Request failed.');
  }

  return new ApiError(response.status, parsed.data.code, parsed.data.message, parsed.data.errors);
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

export const api = {
  auth: {
    /** Exchanges credentials for a session cookie. */
    login: (body: LoginRequest) =>
      request('/auth/login', { method: 'POST', body, schema: meSchema }),

    /** Ends the current session. */
    logout: () => request('/auth/logout', { method: 'POST' }),

    /** Reports the signed-in account. Throws 401 when there is none. */
    me: (signal?: AbortSignal) => request('/auth/me', { schema: meSchema, signal }),
  },

  ports: {
    /** Every port with its group and reservation. */
    list: (signal?: AbortSignal) =>
      request('/ports', { schema: z.array(portSchema), signal }),

    /** Applies a name or driver change to one port. */
    update: (id: string, body: PortUpdate) =>
      request(`/ports/${id}`, { method: 'PATCH', body, schema: portSchema }),

    /** Re-reads the hardware inventory. */
    refresh: () =>
      request('/ports/refresh', { method: 'POST', schema: z.array(portSchema) }),

    /** Takes or extends a hold on a port. */
    reserve: (id: string, body: ReserveRequest) =>
      request(`/ports/${id}/reserve`, { method: 'PUT', body, schema: reservationSchema }),

    /** Releases a hold. */
    release: (id: string) => request(`/ports/${id}/reserve`, { method: 'DELETE' }),
  },

  portGroups: {
    /** Every port group with its membership. */
    list: (signal?: AbortSignal) =>
      request('/port-groups', { schema: z.array(portGroupSchema), signal }),
  },

  users: {
    /** Every account. Admin only. */
    list: (signal?: AbortSignal) =>
      request('/users', { schema: z.array(userSchema), signal }),

    /** Creates an account. */
    create: (body: CreateUserRequest) =>
      request('/users', { method: 'POST', body, schema: userSchema }),

    /** Changes a role or resets a password. */
    update: (id: string, body: UpdateUserRequest) =>
      request(`/users/${id}`, { method: 'PATCH', body, schema: userSchema }),

    /** Deletes an account. */
    remove: (id: string) => request(`/users/${id}`, { method: 'DELETE' }),
  },

  flows: {
    /** Every flow. */
    list: (signal?: AbortSignal) =>
      request('/flows', { schema: z.array(flowSchema), signal }),

    /** One flow. */
    get: (id: string, signal?: AbortSignal) =>
      request(`/flows/${id}`, { schema: flowSchema, signal }),

    /** Creates a flow. */
    create: (body: FlowInput) =>
      request('/flows', { method: 'POST', body, schema: flowSchema }),

    /** Replaces a flow. */
    update: (id: string, body: FlowInput) =>
      request(`/flows/${id}`, { method: 'PUT', body, schema: flowSchema }),

    /** Deletes a flow. */
    remove: (id: string) => request(`/flows/${id}`, { method: 'DELETE' }),

    /**
     * Renders what a flow would put on the wire, without saving it.
     *
     * The editor calls this on every change, so it is deliberately a read: it
     * touches no state and a failure means the flow is invalid, not that
     * anything broke.
     */
    preview: (body: FlowInput, signal?: AbortSignal) =>
      request('/flows/preview', {
        method: 'POST',
        body,
        schema: flowPreviewSchema,
        signal,
      }),
  },

  tests: {
    /** Every test. */
    list: (signal?: AbortSignal) =>
      request('/tests', { schema: z.array(testSchema), signal }),

    /** One test. */
    get: (id: string, signal?: AbortSignal) =>
      request(`/tests/${id}`, { schema: testSchema, signal }),

    /** Creates a test. */
    create: (body: TestInput) =>
      request('/tests', { method: 'POST', body, schema: testSchema }),

    /** Replaces a test. */
    update: (id: string, body: TestInput) =>
      request(`/tests/${id}`, { method: 'PUT', body, schema: testSchema }),

    /** Deletes a test. */
    remove: (id: string) => request(`/tests/${id}`, { method: 'DELETE' }),

    /** Starts a run and returns its id. */
    run: (id: string, dutMeta?: Record<string, unknown>) =>
      request(`/tests/${id}/run`, {
        method: 'POST',
        body: { dutMeta: dutMeta ?? {} },
        schema: z.object({ runId: z.string() }),
      }),
  },

  runs: {
    /** Run history, newest first. */
    list: (
      params: { state?: RunState; testId?: string; limit?: number; offset?: number } = {},
      signal?: AbortSignal,
    ) => {
      const query = new URLSearchParams();
      if (params.state) query.set('state', params.state);
      if (params.testId) query.set('testId', params.testId);
      if (params.limit !== undefined) query.set('limit', String(params.limit));
      if (params.offset !== undefined) query.set('offset', String(params.offset));

      const suffix = query.toString();
      return request(`/runs${suffix ? `?${suffix}` : ''}`, {
        schema: runPageSchema,
        signal,
      });
    },

    /** One run with its trials. */
    get: (id: string, signal?: AbortSignal) =>
      request(`/runs/${id}`, { schema: runDetailSchema, signal }),

    /** Asks an in-flight run to stop. */
    stop: (id: string) => request(`/runs/${id}/stop`, { method: 'POST' }),
  },

  debug: {
    /** Injects loss into a simulated engine. Mock mode only. */
    setLoss: (groupId: string, lossPct: number) =>
      request(`/debug/engines/${groupId}/loss`, {
        method: 'POST',
        body: { lossPct },
        schema: z.object({ lossPct: z.number() }),
      }),

    /** Sets a simulated engine's latency distribution. Mock mode only. */
    setLatency: (groupId: string, medianUs: number, sigma: number) =>
      request(`/debug/engines/${groupId}/latency`, {
        method: 'POST',
        body: { medianUs, sigma },
      }),
  },

  system: {
    /** The appliance health report. */
    health: (signal?: AbortSignal) =>
      request('/system/health', { schema: healthSchema, signal }),

    /** Current hugepage allocation. */
    hugepages: (signal?: AbortSignal) =>
      request('/system/hugepages', { schema: hugepagesStatusSchema, signal }),

    /** Requests a hugepage allocation. */
    setupHugepages: (count: number, size: HugepageSize) =>
      request('/system/hugepages', {
        method: 'POST',
        body: { count, size },
        schema: hugepagesStatusSchema,
      }),
  },
};

/** Query keys, centralised so an invalidation cannot miss a cache entry. */
export const queryKeys = {
  me: ['me'] as const,
  ports: ['ports'] as const,
  portGroups: ['port-groups'] as const,
  users: ['users'] as const,
  health: ['health'] as const,
  flows: ['flows'] as const,
  flow: (id: string) => ['flows', id] as const,
  tests: ['tests'] as const,
  test: (id: string) => ['tests', id] as const,
  runs: ['runs'] as const,
  run: (id: string) => ['runs', id] as const,
};
