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

Milestone 4 extends `engine/` with the stateful (ASTF) mode.

---

## 2b. The traffic pipeline

A flow travels through four representations, and each boundary exists for a
reason.

```
FlowConfig            what the operator configured        (flux-core::flow)
    │  orch::translate — resolves symbolic fields to offsets
    ▼
Vec<StreamSpec>       engine-agnostic programmed streams  (flux-core::engine)
    │  engine::trex::stream — renders TRex's JSON
    ▼                 engine::mock — simulates the rate
TRex stream JSON      what goes over ZMQ
    ▼
frames on the wire
```

`StreamSpec` is the join point. Passing engine-native JSON straight through
would have been less code, but the mock would then have no idea what rate it had
been asked for — which is the one thing it exists to simulate.

### The frame builder

`flux_core::frame` turns a header stack into bytes, deriving EtherTypes,
lengths, and three checksums. It lives in Rust rather than being duplicated in
the browser: those derived fields are exactly what goes subtly wrong in a second
implementation, and a preview that disagrees with what the engine transmits is
worse than no preview. The IPv4 checksum is tested against the RFC 1071 worked
example, and every checksum is also tested by the property a receiver actually
checks — that the header sums to zero.

Two conventions matter:

- **Frame size includes the FCS**, per RFC 2544. The builder emits four bytes
  fewer, because the NIC appends it.
- **Rates are quoted at layer 1**, including the 7-byte preamble, the SFD, and
  the 12-byte interframe gap. That is what makes 64-byte frames on a 10G link
  resolve to 14,880,952 pps rather than 19,531,250.

### Modifier resolution

The flow document names fields symbolically (`ipv4_src`); `orch::translate`
resolves them to an offset and a width by walking the same header stack the
builder walks. A modifier pointed at the wrong offset does not fail — it quietly
corrupts a different field — so a test reads the bytes each modifier points at
and asserts they are the field it claims.

Widths are 1, 2, or 4 bytes because TRex flow variables come in those sizes
only. Address modifiers deliberately target the low bytes: the top of a MAC is
the OUI and the top of an IPv4 address is the network, and a host-emulation
modifier that walked those would generate traffic for a different network than
the operator configured.

---

## 2c. Engines

`Engine` has two implementations, selected by `FLUX_ENGINE`.

**`MockEngine`** derives counters from elapsed wall-clock time against the
configured rate. A 60-second trial takes 60 seconds — the orchestrator's timing,
the collector's cadence, and the UI's countdown are all worth exercising at
their real speed — with `FLUX_MOCK_TIMESCALE` for tests that cannot wait. Loss
is injectable through `/api/v1/debug`, and latency is drawn from a log-normal
distribution because real forwarding latency is bounded below by the wire and
tailed above by queueing, which a normal distribution cannot represent.

What the mock does *not* do is model a device under test: received counters are
transmitted counters minus injected loss. It exercises the pipeline; it does not
predict a result.

**`TrexEngine`** speaks JSON-RPC over ZeroMQ. Per the milestone plan it is
structured and unit-tested against a fake transport but not runtime-verified —
that needs DPDK-capable NICs. Every field name taken from documentation rather
than from a live instance is marked `TODO(trex-verify)`, and all of them are in
`engine/trex/`.

Two details worth knowing:

- **Statistics are relative.** TRex counters are cumulative from process start
  and there is no reset RPC, so `clear_stats` records a baseline and later reads
  subtract it. RFC 2544 depends on this: a trial must measure the trial.
- **Calls are batched.** Programming a hundred streams as a hundred round trips
  is the slowest thing a naive client does, and a REQ socket cannot pipeline.

### Why the actor

A ZeroMQ REQ socket is strictly alternating and cannot be shared. Each instance
is therefore owned by one task, reached through an `EngineHandle` that sends a
command and awaits a `oneshot` reply. The `Engine` trait's `Send + Sync` bound
exists so the *handle* can live in shared state — not so concurrent calls are
legal.

---

## 2d. The collector

One task per instance, polling at 1 Hz. Counters are differenced against the
*actual* elapsed time between samples rather than an assumed one second: under
load it will not be one second, and assuming otherwise makes the charts disagree
with the totals.

Samples fan out on a `broadcast` channel. WebSocket sessions subscribe to it and
filter locally — nothing is polled per subscriber. A ring buffer holds the last
600 samples so a client that connects mid-run renders a full chart immediately
rather than drawing itself in from the right.

A subscriber that falls behind is dropped rather than buffered, and told why.
Buffering for a client that cannot keep up with one message a second would grow
without bound.

Time series go to VictoriaMetrics through the Prometheus exposition line format.
Failures there are logged and dropped: a gap in a historical graph is a far
better outcome than stalling the collector, which also feeds the live stream.

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

## 2e. RFC 2544

The benchmark is split in two, and the split is the point.

`orch::rfc2544` is **pure**. Given the configuration and the trials run so far,
it returns the next action. It touches no engine, no clock, and no database, so
it is table-tested exhaustively - convergence against a simulated device,
all-pass, all-fail, boundary tolerance, iteration cutoff, never repeating a
rate, terminating at an absurdly fine resolution - in milliseconds rather than
by running hour-long tests against hardware.

`orch::statemachine` is the async half: reprogram per frame size, move the
multiplier per trial, record every one, publish progress. A bug in "which rate
next" is caught by a table test; a bug here shows up as a run that does not
progress, which is far more visible.

### Why a fold over the whole history

The search takes the complete trial list rather than carrying mutable state.
That makes it replayable: the orchestrator can reconstruct exactly where a
search was from the `run_results` rows alone, which is what a resumable run
needs and what makes a stored result auditable after the fact.

### The four tests

| | Section | Searches | At |
|---|---|---|---|
| Throughput | 26.1 | rate, binary | the configured tolerance |
| Latency | 26.2 | rate, then one timestamped trial | the rate the search found |
| Frame loss | 26.3 | a descending ladder | each rung |
| Back-to-back | 26.4 | burst length, binary | zero loss, full line rate |

Latency measures *at* the throughput rate, which is not known until the search
finishes - so that trial runs after convergence rather than alongside it.

### Conformance is stated, not assumed

A trial shorter than sixty seconds, a non-zero loss tolerance, or an omitted
standard frame size each make a run non-conformant. Flux still runs it - those
are useful while iterating - but says so in the wizard *before* the run and at
the top of the report *after* it. A report that quietly presented a ten-second
trial as an RFC 2544 throughput figure would mislead whoever reads it later,
and they have no other way to know.

`StopReason` is recorded with every result, because "converged at 87.5%" and
"gave up at 87.5% after twenty trials" are different claims and only one of them
is a measurement.

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

### The statistics WebSocket

A client connects, subscribes, and receives one message a second:

```json
{ "subscribe": ["port:*", "stream:run:<runId>", "run:<runId>"] }
```

Selectors it does not recognise are ignored rather than rejected, so a client
built against a newer server still gets the series this build knows about. A run
selector *scopes* everything: a client watching one run does not receive another
run's ports just because it also said `port:*`.

A batch that matches nothing is not sent at all, so an idle subscriber is not
woken every second to be handed an empty object.

### Serving the UI

The UI is a static export (`output: 'export'`, `trailingSlash: true`). `fluxd`
serves it with a directory-index-aware file service falling back to the root
document, so a deep link the export did not pre-render still reaches the
client-side router.

**Dynamic routes.** A run's id does not exist at build time, but a static export
must know every path. The export therefore emits one document per dynamic route
under a placeholder segment (`/runs/__id__/`), and `fluxd` maps
`/runs/<anything>/` onto it by replacing path segments right to left until a
document matches. A path that already resolves to a real file is never
rewritten, so `/runs/` still serves the history table. The client reads the
actual id from `window.location`.

Nested routes resolve before their parents, so `/runs/<id>/report/` finds the
report rather than the run view. The alternative - `/runs/detail?id=...` - needs
no server-side mapping but gives up readable, linkable URLs.

### Reports

`GET /api/v1/runs/{id}/report` renders one self-contained HTML document: no
scripts, no external assets, nothing to fetch. A report is archived, emailed,
and printed months later, so a page that needs the appliance still running to
render itself is not a record.

The UI route `/runs/<id>/report` frames that document in an iframe and prints
the frame rather than the page, so `window.print()` produces the document with
its own print stylesheet and none of the application shell. Duplicating the
layout in React would give two renderings that drift apart.

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
| RFC 2544 search | A pure `(config, trials) -> action` function, exhaustively table-tested: convergence against a simulated device, all-pass, all-fail, boundary tolerance, iteration cutoff, no repeated rates, termination at any resolution |
| State machine | Driven end to end against `MockEngine` with injected loss, against a live Postgres |
| Report | Rendered and asserted on: no scripts, no external references, operator text escaped, caveats present |
| pcap import | Decoded stacks, truncation notes, dropped options, and a fuzz pass that truncates and corrupts a valid capture at every offset |

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
| ZeroMQ crate | `zmq` / `tmq` | `zeromq` (pure Rust) | `zmq` links libzmq, which without a system package must be built from source by cmake — making `cargo build` fail on any machine without a C toolchain, including CI. The protocol on the wire is identical and the choice is isolated behind `engine::trex::transport::RpcTransport`. |
| Frame hex preview | Client-side render | `POST /flows/preview` | The preview needs EtherTypes, lengths, and three checksums. Duplicating that in TypeScript means two implementations that will disagree, and the one that matters is the one the engine uses. |

---

## 11. Milestone status

| | Delivered |
|---|---|
| **1** | Workspace, API, auth and sessions, users, migrations, port model with `flux-portd`, dashboard and ports pages |
| **2** | Flow documents, frame builder, rate maths, `Engine` with both implementations, translator, collector, WebSocket stream, manual test type, run lifecycle, flow editor, tests page, run history, live run view |
| **3** | All four RFC 2544 benchmarks with the search as a pure function, the benchmark state machine, wizards, printable reports, pcap import |
| **4** | ASTF and L4-7 profiles, analytics, TLS and settings, config export/import, port-group relaunch, deployment polish |

### Known gaps

- **`TrexEngine` is unverified against a live TRex.** Structured, unit-tested,
  and marked; running it needs DPDK-capable hardware.
- **`MockEngine` does not model a device under test.** Received counters are
  transmitted counters minus injected loss, so a mock run measures the mock. It
  exercises the pipeline end to end; it does not predict a result. The injected
  loss is flat across rates, so a mock RFC 2544 search either passes at the
  ceiling or fails everywhere - the search's behaviour against a device with a
  real throughput ceiling is covered by the table tests instead.
