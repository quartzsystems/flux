# Flux architecture

Kept current as the product is built. Each section states what exists today and,
where relevant, what a later milestone changes.

---

## 1. Shape of the system

Flux is one appliance running four local processes:

| Process | Privilege | Role |
|---|---|---|
| `fluxd` | unprivileged (`flux` user) | REST + WebSocket API, static UI, orchestrator, engine supervisor |
| `flux-portd` | root | NIC driver binding and hugepage allocation, nothing else |
| `postgres` | `postgres` | Configuration objects, users, runs, results |
| `victoria-metrics` | `victoriametrics` | Time series |

Plus zero or more TRex instances, spawned and supervised by `fluxd`, one per
port group.

Nothing listens on a network interface except `fluxd`. Postgres and
VictoriaMetrics bind loopback; `flux-portd` uses a unix socket.

### Why a separate privileged helper

`fluxd` is a large surface: an HTTP server, a JSON parser, a database client, a
ZMQ client. Running it as root would mean a bug anywhere in that surface is a
root compromise. Instead the two operations that genuinely need root — writing to
`/sys/bus/pci/.../driver_override` and to
`/sys/kernel/mm/hugepages/*/nr_hugepages` — live in a binary small enough to read
in one sitting, behind an allowlist that cannot express "the management NIC".

---

## 2. Crate layout

```
crates/
├── flux-core/       Shared domain types and service traits. No I/O.
├── fluxd/           The daemon.
└── flux-portd/      The privileged helper.
```

### `flux-core`

The vocabulary every other module speaks, and the trait boundaries between
`fluxd`'s internal modules.

| Module | Contents |
|---|---|
| `types` | `Role`, `PortMode`, `LinkState`, `EngineMode`, `PortGroupState`, `RunState`, `TestType` |
| `port` | `PciAddr`, `NicInfo`, `HugepagesStatus`, the `PortController` trait, the `flux-portd` wire protocol |
| `engine` | `PortStats`, `PgidStats`, `LatencyStats`, the `Engine` trait |
| `config` | Declarative configuration documents and their validation |

This crate has no database, HTTP, ZMQ, or DPDK dependency. That is what lets the
orchestrator and collector be written and tested against a mock with none of that
machinery present — and it is why any module behind one of these traits could be
lifted into its own service later without touching a call site.

Two conventions run through it:

- **Enum tokens are the database tokens.** `Role::Admin.as_str()` is `"admin"`,
  which is exactly what the `CHECK` constraint on `users.role` permits. A unit
  test asserts the JSON form and the `as_str` form agree, so they cannot drift.
- **Validated newtypes at trust boundaries.** `PciAddr` is not a `String`. Its
  value reaches a root process that shells out to `dpdk-devbind.py`, so the only
  constructor enforces the exact hex layout and there is no way to build one
  containing a shell metacharacter or a path.

### `fluxd`

```
src/
├── main.rs         Startup, janitor task, graceful shutdown
├── config.rs       Environment-derived configuration
├── state.rs        AppState — cheap to clone, shared by every handler
├── bootstrap.rs    First-boot administrator creation
├── api/            axum routers, extractors, error type, static serving
├── auth/           Argon2id, session tokens, the Identity principal
├── portmgr/        Port model; mock and unix PortController implementations
└── store/          sqlx repositories and migrations
```

Milestone 2 adds `engine/` (mock and TRex implementations) and `collector/`;
milestone 3 adds `orch/`.

---

## 3. Concurrency model

One tokio runtime. The rules that matter:

- **Engine access is serialised per instance.** A ZMQ socket cannot be shared
  across tasks, so each engine instance is owned by an actor task reached through
  an `mpsc` command channel with `oneshot` replies. The `Engine` trait is
  `Send + Sync` so a handle can live in shared state — not so that concurrent
  calls are legal.
- **Each active run is a spawned task** holding a `RunHandle`, cancelled through
  a `CancellationToken`. *(Milestone 3.)*
- **The collector is one polling task per active engine instance**, on a 1 s
  `tokio::time::interval`, publishing normalised samples onto a `broadcast`
  channel. WebSocket sessions subscribe and filter by their subscription set.
  *(Milestone 2.)*
- **Everything else is per-request.** Handlers clone `AppState` and touch the
  pool directly; there is no global lock on the request path.

---

## 4. Data model

Postgres holds configuration and results; VictoriaMetrics holds time series.

Every configuration object is a JSONB document plus the typed columns actually
queried or constrained on. The document is the serialised form of a Rust struct,
so the shape is defined exactly once, in Rust.

Enumerations are `TEXT` with a `CHECK` constraint rather than Postgres `ENUM`
types: adding a variant is then a one-line migration instead of an `ALTER TYPE`
that cannot run inside a transaction.

Tables, in dependency order: `users`, `sessions`, `port_groups`, `ports`,
`reservations`, `devices`, `flows`, `load_profiles`, `tests`, `runs`,
`run_results`, `settings`. The full schema with its rationale is
`crates/fluxd/migrations/0001_initial.sql`.

### On compile-time query checking

sqlx's `query!` macros verify SQL against a live database at compile time. Flux
uses the runtime-checked `query_as::<_, T>()` form instead, because `cargo build`
must not require a running Postgres — bootstrapping the appliance image, CI, and
a developer's first clone all happen before any database exists.

What is kept from the macro form is the part that matters: every query
deserialises into an explicit `FromRow` struct with domain types (`Role`,
`PciAddr`), so a schema/struct mismatch surfaces as a clear decode error on the
first query rather than as a silently wrong value.

To switch: point `DATABASE_URL` at a migrated database, change `query_as` to
`query_as!`, run `cargo sqlx prepare`, and commit the generated `.sqlx/`.

### Port reconciliation

`ports` rows carry two kinds of column, and they are treated differently:

| Kind | Columns | On inventory refresh |
|---|---|---|
| Hardware truth | `pci_addr`, `driver`, `ifname`, `mac`, `speed_mbps`, `numa_node`, `mode`, `link_state` | Overwritten |
| Operator intent | `name`, `group_id`, `group_index` | Never touched |

A card that is rebound, or briefly disappears, must not lose the label an
operator gave it or the group it belongs to. Rows for devices that stop appearing
are flagged `present = false` rather than deleted, for the same reason.

`mac` and `speed_mbps` are `COALESCE`d rather than overwritten: a DPDK-bound card
stops reporting them through the kernel, and blanking the table would lose
information the operator can still use.

---

## 5. HTTP surface

One `Router` serves both `/api/v1` and the exported UI at `/`.

### Authentication and authorisation

Authentication is one middleware that resolves the session cookie into an
`Identity` in the request extensions. It never rejects — deciding what an
anonymous request may do belongs to the extractors, which know what the handler
requires.

Authorisation is the type an argument is declared as:

| Extractor | Minimum role |
|---|---|
| `Auth` | viewer |
| `OperatorAuth` | operator |
| `AdminAuth` | admin |

A handler taking `AdminAuth` cannot be routed without an admin check, and a
handler taking no auth extractor is visibly public at its definition. That is
stronger than a per-route middleware table, which drifts the moment a route is
added.

`viewer = GET only` falls out of this: every mutating handler takes
`OperatorAuth` or `AdminAuth`.

401 and 403 are kept distinct because the UI does different things with them —
401 sends the operator to the login page, 403 tells them their account cannot do
this and signing in again will not help.

### Errors

One `ApiError` enum implementing `IntoResponse`. Every failure returns the same
body shape:

```json
{ "code": "validation", "message": "one or more fields are invalid",
  "errors": [{ "path": "rate.value", "msg": "exceeds port line rate" }] }
```

`errors` is present only for validation failures. Validation collects every
problem rather than short-circuiting on the first, so an operator filling in a
form sees all the bad fields highlighted in one round trip.

`ApiError::Internal` deliberately does not put its detail in the response body —
a database error message names tables, columns, and constraints. The client gets
a generic message; the cause goes to the log with its full `anyhow` context
chain.

### Sessions

- Token: 32 bytes from the OS CSPRNG, hex encoded.
- Stored: SHA-256 of the token. A database disclosure yields no usable sessions.
  A fast hash is correct here — the input is already high-entropy, so Argon2
  would add latency to every request and defend against nothing.
- Cookie: `HttpOnly`, `SameSite=Strict`, `Path=/`, `Secure` when
  `FLUX_COOKIE_SECURE=1`.
- Logout deletes the row, so a token copied out of the browser stops working.
- Changing a password deletes every session for that account.

Login spends one Argon2 verification whether or not the username exists, and
returns one message either way, so response latency does not enumerate accounts.

### Serving the UI

The UI is a static export (`output: 'export'`, `trailingSlash: true`). `fluxd`
serves it with a directory-index-aware file service falling back to the root
document, so a deep link the export did not pre-render still reaches the
client-side router.

Cache policy is split: content-hashed output under `/_next/static` is
`immutable`, everything else is `no-store`. Without that split, an upgraded
appliance keeps serving the previous build's markup against the new API.

The CSP allows `'unsafe-inline'` for scripts. A Next.js static export bootstraps
from inline `<script>` tags and there is no server to stamp a per-response nonce.
Combined with `'self'`-only sources this still blocks loading foreign script;
tightening it further requires moving the UI off static export.

---

## 6. The port control boundary

```
fluxd                          flux-portd (root)
  │                                  │
  │  {"op":"bind","pci":"0000:81:00.0","driver":"vfio-pci"}
  ├─────────────── unix socket ─────►│
  │                                  ├─ allowlist check
  │                                  ├─ record original kernel driver
  │                                  ├─ dpdk-devbind.py --bind vfio-pci …
  │                                  └─ re-read sysfs
  │◄──── {"status":"ok","kind":"nic","nic":{…}} ────┤
```

Newline-delimited JSON, one request per line, one response per line, no session
state. Five operations: `list`, `bind`, `unbind`, `hugepages_status`,
`hugepages_setup`.

The allowlist in `/etc/flux/portd.yaml` is allow-only — there is no wildcard and
no deny list to get wrong, because a rule that is absent means refuse. A missing
config file is a hard error rather than an empty allowlist, since silently
starting with "refuse everything" would look like a hardware fault and send the
operator debugging the wrong thing.

`unbind` restores the driver the device had *before* Flux took it, recorded under
`/var/lib/flux/original-drivers/` at bind time. `dpdk-devbind.py` has no
"restore to whatever it was" mode, and guessing wrong leaves a NIC with no
driver.

`fluxd` never calls the helper directly from a handler. Everything goes through
`portmgr::PortManager`, which owns the safety rules the helper has no context
for — the helper knows an address is allowlisted, but only the manager knows the
port is currently in a running port group.

### Mock mode

`PortController` has two implementations: the unix client, and an in-process fake
presenting a four-port 100G chassis with two ports cabled and two spare. Selected
by `FLUX_PORTD=mock`. `Engine` will have the same shape in milestone 2.

The fake simulates one thing real hardware could not: link state stays visible
after a DPDK bind. On a real appliance that information comes from the engine
once it owns the port. Reporting it in the mock keeps the ports page meaningful
before the engine exists.

---

## 7. The web application

Next.js App Router, TypeScript strict, Tailwind v4, exported statically.

Because it is a static export there is no server render that could know who is
signed in. Every mount asks `/auth/me` once and that answer gates the shell; the
cookie is `HttpOnly` and unreadable from script, so asking the API is the only
way to know — and the API is the only authority that matters anyway.

### Types

`web/lib/api-types.ts` holds a zod schema per Rust `serde` struct, each naming
its counterpart. Responses are **parsed, not cast**: an appliance can be running
a `fluxd` newer or older than the bundle a browser has cached, and a silent shape
mismatch surfaces as `undefined` deep inside a render. A parse failure at the
boundary says what actually went wrong.

The two sides are kept in sync by hand. That is a real maintenance cost, accepted
because generating TypeScript from the OpenAPI document would put a code
generator between a Rust change and the compile error that should catch it.

### Design system

`web/app/globals.css` carries the Quartz Systems token block verbatim from
`quartzsystems/design-system`. It is not forked here — a token that needs to
change changes upstream and every Quartz app inherits it. Component classes
(`.surface`, `.qz-table`, `.badge`, `.btn`, `.kpi`) are built on those tokens,
and `web/components/ui.tsx` wraps them so a page never has to remember that a KPI
label is mono 10px uppercase.

Monospace is used for every value read by matching characters against a label or
a specification: MACs, IPs, PCI addresses, hex, counters, and rates. Numeric
columns use `tabular-nums` so a counter updating at 1 Hz does not make the row
twitch sideways.

### Fonts

Manrope and JetBrains Mono ship from `@fontsource`, which places the woff2 files
in `node_modules` and emits plain `@font-face` rules. Nothing is fetched from
Google at build time or run time — the appliance may never have seen the
internet, and a font request that silently fails would drop the whole UI onto a
system fallback.

---

## 8. Startup sequence

1. Initialise tracing. JSON when `FLUX_LOG_FORMAT=json`, human-readable
   otherwise.
2. Read configuration from the environment. Fail loudly on anything malformed.
3. Connect to Postgres and force one connection, so a bad `DATABASE_URL` fails at
   startup rather than as a confusing 500 on the first request.
4. Run migrations.
5. Create the bootstrap administrator if the users table is empty.
6. Build the `PortController` — mock or unix — and refresh the port inventory. A
   failure here is logged, not fatal: the helper may still be coming up under
   systemd, and an appliance that refuses to serve its own UI because it cannot
   see a NIC gives the operator no way to diagnose that.
7. Start the janitor task (expired sessions, lapsed reservations).
8. Serve, with graceful shutdown on `SIGINT` and `SIGTERM`.

Milestone 3 adds a step: runs found in a non-terminal state are marked `failed`
with reason `daemon_restart`. Resuming them safely needs engine state that did
not survive the restart, so failing them is the honest default.

---

## 9. Testing

| Layer | Approach |
|---|---|
| `flux-core` | Pure unit tests: enum round-trips, `PciAddr` rejection cases, validation paths |
| `flux-portd` | Allowlist enforcement, refusal before any hardware call |
| `fluxd` | Error mapping, role checks, validation rules, mock controller behaviour |
| RFC 2544 search | *(Milestone 3.)* The search is a pure `(trial_results) -> next_action` function, separated from the async execution loop, exhaustively table-tested: convergence, all-pass, all-fail, boundary tolerance, max-iteration cutoff |
| State machine | *(Milestone 3.)* Driven against `MockEngine` with injected loss |

The quality bar: `cargo clippy --workspace --all-targets -- -D warnings` clean,
no `unwrap`/`expect` on request or engine control paths, and a tracing span with
`run_id` and `port` fields on every run and engine operation.

---

## 10. Deviations from the original specification

| Decision | Specified | Built | Why |
|---|---|---|---|
| sqlx queries | Compile-time checked (`query!`) | Runtime checked (`query_as`) | `cargo build` must not require a live Postgres. Upgrade path documented in `store/mod.rs`. |
| Next.js version | 14+ | 15.5 | Current stable line; App Router and static export unchanged. |
| Postgres version | 16 | 16+ (developed against 18) | No version-specific features are used. |
