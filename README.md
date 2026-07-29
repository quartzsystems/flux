<div align="center">
  <img src="web/public/brand/flux-lockup.svg" alt="Flux" width="260">
  <p><em>Traffic generation and load testing that moves at line rate.</em></p>
</div>

---

Flux is a self-contained network traffic and load generator appliance. It wraps
[Cisco TRex](https://trex-tgn.cisco.com/) as its packet engine and presents a
complete dark-themed web interface for configuring test traffic, running
standardised suites (RFC 2544), and reviewing live and historical results.

One appliance is one web endpoint and one complete system. There is no cloud
controller and no external management plane.

## Status

**Milestone 2 of 4 — flows, engines, and live statistics.**

| | |
|---|---|
| ✅ **Milestone 1** | Workspace, API, login and sessions, user administration, migrations, port inventory with driver binding and reservations, dashboard and ports pages, `flux-portd` privileged helper |
| ✅ **Milestone 2** | Flow documents with a full editor, frame builder and hex preview, rate maths, `Engine` with mock and TRex implementations, statistics collector, WebSocket stream, manual test type, run history and the live run view |
| 🚧 **Milestone 3** | RFC 2544 throughput / latency / frame-loss / back-to-back, reports, pcap import |
| 🚧 **Milestone 4** | Stateful L4-7 profiles, analytics, TLS and settings, deployment polish |

Routes for later milestones are already in the navigation and each says which
milestone delivers it, so the shape of the product is visible from the first
screen.

Two things are not yet verified against reality, and are called out in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#known-gaps): `TrexEngine` has never
talked to a live TRex, and no run has been recorded against a real Postgres.

## Architecture at a glance

```
┌─────────────────────────────────────────────────────┐
│  Next.js static UI  (served by fluxd at /)          │
└──────────────┬──────────────────────┬───────────────┘
               │ REST /api/v1         │ WS /api/v1/stream
┌──────────────▼──────────────────────▼───────────────┐
│  fluxd (Rust, unprivileged)                         │
│  ├─ api/        axum routers, auth, validation      │
│  ├─ orch/       test-run state machine, wizards     │
│  ├─ engine/     TRex lifecycle + ZMQ RPC client     │
│  ├─ collector/  1s polling, ring buffer, fan-out    │
│  ├─ store/      sqlx repos, VM remote-write         │
│  └─ portmgr/    port model, talks to flux-portd     │
└───────┬─────────────────────┬───────────────────────┘
        │ unix socket         │ spawn + ZMQ (localhost)
┌───────▼────────┐   ┌────────▼───────────────────────┐
│ flux-portd     │   │ TRex instance(s)               │
│ (root): NIC    │   │ one per port-group,            │
│ bind, hugepages│   │ mode = stl | astf              │
└────────────────┘   └────────────────────────────────┘
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the detail.

## Development

Flux runs fully mocked on an ordinary workstation — no DPDK, no NIC, no root.
The mock presents a plausible four-port chassis and honours binding, so the
entire UI and API are exercisable.

### Prerequisites

- Rust stable (1.82 or newer)
- Node.js 20 or newer
- PostgreSQL 16 or newer, reachable locally

### First run

```bash
# 1. Create the flux role and database (once, as a Postgres superuser).
psql "postgres://postgres@127.0.0.1:5432/postgres" -f deploy/sql/bootstrap.sql

# 2. Configure.
cp .env.example .env      # edit DATABASE_URL if your setup differs

# 3. Install web dependencies.
make web-install

# 4. Run the API and the UI dev server together.
make dev
```

`make dev` starts `fluxd` on <http://127.0.0.1:8080> and the Next.js dev server
on <http://127.0.0.1:3000>, with `/api` proxied to the daemon. Open the dev
server.

On the very first start, `fluxd` finds an empty users table and creates an
`admin` account. If `FLUX_BOOTSTRAP_ADMIN_PASSWORD` is unset it generates a
passphrase and prints it to the log **once** — copy it before you lose the
scrollback.

To serve everything from the daemon alone, exactly as the appliance does:

```bash
make serve      # builds the static export and points fluxd at it
```

### Driving a run in mock mode

The mock presents a four-port 100G chassis with the first pair cabled together,
so a complete run works end to end without hardware:

1. **Ports** — group two ports, then bind them to DPDK.
2. **Flows** — create a flow between them. The editor previews the exact frame
   the engine will transmit, byte for byte.
3. **Tests** — create a manual test naming that flow, and press run.
4. **Runs** — watch the live charts. A 60-second flow really does take 60
   seconds; set `FLUX_MOCK_TIMESCALE=10` to speed the clock up in tests.

To see what loss looks like on the charts, inject some while a run is in flight:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/debug/engines/<groupId>/loss \
     -H 'Content-Type: application/json' -b flux_session=<cookie> \
     -d '{"lossPct": 2.5}'
```

The `/debug` routes exist only when `FLUX_ENGINE=mock`; on a real appliance the
whole router is absent.

### Common targets

| Target | What it does |
|---|---|
| `make dev` | `fluxd` (mocked) plus the Next.js dev server |
| `make dev-api` | Just `fluxd`, mocked |
| `make serve` | Build the UI and serve it from `fluxd` |
| `make test` | Rust test suite |
| `make lint` | `cargo clippy -- -D warnings` |
| `make web-lint` | ESLint plus `tsc --noEmit` |
| `make ci` | Everything the pipeline runs |

## Layout

```
flux/
├── crates/
│   ├── flux-core/     Shared domain types and service traits
│   ├── fluxd/         The daemon: API, orchestrator, engine supervisor
│   └── flux-portd/    Privileged helper: NIC binding and hugepages
├── web/               Next.js UI, exported statically and served by fluxd
├── deploy/            systemd units, SQL bootstrap, configuration examples
└── docs/              Architecture notes
```

## Configuration

Every setting is an environment variable, documented in
[`.env.example`](.env.example). On an appliance they live in
`/etc/flux/fluxd.env`, referenced by the systemd unit.

The two that decide what Flux actually drives:

| Variable | Values | Meaning |
|---|---|---|
| `FLUX_ENGINE` | `mock`, `trex` | Simulated packet engine, or real TRex processes |
| `FLUX_PORTD` | `mock`, `unix` | Simulated chassis, or the privileged helper |

`FLUX_PORTD` follows `FLUX_ENGINE` unless set explicitly.

## Security posture

- `fluxd` runs unprivileged. The only root component is `flux-portd`, which
  exposes five operations over a unix socket and refuses any PCI address absent
  from `/etc/flux/portd.yaml`. The management NIC is never in that allowlist.
- Passwords are Argon2id. Session tokens are 256 bits of OS randomness, stored
  only as their SHA-256, and delivered in an `HttpOnly; SameSite=Strict` cookie.
- Roles are enforced by the type a handler's argument is declared as, so an
  unguarded endpoint is visibly unguarded at the definition.

## License

MIT. See [LICENSE.md](LICENSE.md).
