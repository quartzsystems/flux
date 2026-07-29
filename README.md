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

**Feature complete — all four milestones delivered.**

| | |
|---|---|
| ✅ **Milestone 1** | Workspace, API, login and sessions, user administration, migrations, port inventory with driver binding and reservations, dashboard and ports pages, `flux-portd` privileged helper |
| ✅ **Milestone 2** | Flow documents with a full editor, frame builder and hex preview, rate maths, `Engine` with mock and TRex implementations, statistics collector, WebSocket stream, manual test type, run history and the live run view |
| ✅ **Milestone 3** | All four RFC 2544 benchmarks with the search as a pure, exhaustively tested function; per-benchmark wizards; printable self-contained reports; pcap import |
| ✅ **Milestone 4** | Stateful L4-7 load profiles over TRex ASTF, the topology view, historical analytics, TLS, retention and appliance identity, configuration export/import, deployment |

The API, the orchestrator, and the UI are verified end to end against a real
Postgres. What is *not* verified against reality is called out under
[known gaps](docs/ARCHITECTURE.md#known-gaps); the important one is that
`TrexEngine` has never talked to a live TRex, which needs DPDK-capable
hardware.

## Install

On a fresh AlmaLinux, Rocky, RHEL, Debian, or Ubuntu box with systemd:

```bash
curl -fsSL https://raw.githubusercontent.com/quartzsystems/flux/main/deploy/install.sh | sudo bash
```

That installs PostgreSQL and VictoriaMetrics, creates the `flux` role and
database with a generated password, places the daemon and the privileged helper,
enables both units, and prints where to find the first administrator password.
Open `http://<appliance>:8080/` when it finishes.

Binaries are statically linked against musl, so one build runs on both
distribution families — AlmaLinux 9 and 10, Rocky, RHEL, Debian 12, and Ubuntu
22.04 upward, on x86_64 and aarch64. Downloads are verified against the
release's `SHA256SUMS` before anything is placed.

To look around without hardware, install the mock engine — a simulated four-port
100G chassis that drives the entire UI:

```bash
curl -fsSL https://raw.githubusercontent.com/quartzsystems/flux/main/deploy/install.sh   | sudo bash -s -- --engine mock
```

<details>
<summary>Other options</summary>

```
--version <x.y.z>     Install a specific release rather than the latest
--from-source         Install from a checkout you have already built
--database-url <url>  Point at an existing PostgreSQL instead of provisioning one
--no-deps             Do not install distribution packages
--no-metrics          Skip VictoriaMetrics (analytics stays empty)
--no-firewall         Do not open the HTTP port
--no-start            Place everything but start nothing
--uninstall           Remove Flux, keeping configuration and data
--uninstall --purge   Remove everything, including the database
```

</details>

### Upgrading

The same command. Run it again and it upgrades in place:

```bash
curl -fsSL https://raw.githubusercontent.com/quartzsystems/flux/main/deploy/install.sh | sudo bash
```

- `fluxd.env` and `portd.yaml` are never overwritten. The new release's example
  lands beside them as `fluxd.env.example` so new settings are visible.
- The database is dumped to `/var/lib/flux/backups/<version>-<timestamp>/`
  before anything changes, along with the binaries and the UI it is replacing.
- If the new version does not answer its health check within a minute, the
  previous binaries go back and the services restart on them.
- The last three backups are kept; older ones are pruned.

Migrations are forward-only, so a rollback restores the binaries but not the
schema. That is what the dump is for, and the installer tells you the exact
`pg_restore` command if it ever has to roll back. A downgrade is refused unless
you pass `--allow-downgrade`.

## Versioning

The `VERSION` file at the repository root is the single source of truth. The
binaries read it at build time — `fluxd --version` reports what the tree said,
not what `Cargo.toml` happened to say — and `scripts/sync-version.sh` writes it
into `Cargo.toml` and `web/package.json`. CI fails if they disagree.

Cutting a release:

```bash
make release V=0.2.0          # writes VERSION and propagates it in one step
git commit -am "Release 0.2.0"
git push                      # let CI go green before tagging
git tag v0.2.0 && git push --tags
```

Raising the version and propagating it are one command deliberately. Doing them
separately is forgettable, and forgetting means a tag that CI rejects after you
have already pushed it — recoverable only by moving the tag.

Push the commit before tagging, too. The `version` job on `main` catches drift
in seconds; the same check inside the release workflow catches it only once the
tag exists.

The tag triggers the release workflow, which refuses to publish if the tag and
`VERSION` disagree, builds both architectures, and attaches the tarballs and
`SHA256SUMS` to a GitHub release.

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
4. **Tests** — or create an RFC 2544 benchmark. The wizard opens with the seven
   standard frame sizes and a sixty-second trial, and warns before the run if
   anything you change would make the result non-conformant.
5. **Runs** — watch the live charts, then open the report and print it. A
   60-second trial really does take 60 seconds; set `FLUX_MOCK_TIMESCALE=60` to
   speed the clock up while developing.
6. **Topology** — the diagram lays itself out from the flows, and the edges
   carry live rate and loss while a run is in flight. Name the device under test
   here and every run started afterwards records it on its report.
7. **Load profiles** — for the stateful side, define client and server pools, an
   application, and a connection-rate ramp. A port group is stateless *or*
   stateful, so give the profile its own group.
8. **Analytics** — once a run has finished, chart what it recorded. Needs
   VictoriaMetrics on `FLUX_VM_URL`; without it, runs and reports still work and
   only the historical charts stay empty.

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
| `make version` | Print the version everything builds from |
| `make version-sync` | Write `VERSION` into the manifests |
| `make dist` | Build the release tarball into `dist/` |
| `make ci` | Everything the pipeline runs |

## Layout

```
flux/
├── crates/
│   ├── flux-core/     Shared domain types and service traits
│   ├── fluxd/         The daemon: API, orchestrator, engine supervisor
│   └── flux-portd/    Privileged helper: NIC binding and hugepages
├── web/               Next.js UI, exported statically and served by fluxd
├── deploy/            install.sh, systemd units, SQL bootstrap, config examples
├── scripts/           Version sync, release packaging, installer tests
├── .github/workflows/ CI, the reusable build, and the release
└── docs/              Architecture notes
```

`VERSION` at the root is what every one of those derives its version from.

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
- TLS is uploaded through the UI, checked before it is written, and the key is
  stored mode 0600. Until a certificate is installed the appliance serves plain
  HTTP and the session cookie crosses the network in the clear — install one
  before putting the appliance on a shared network, and set
  `FLUX_COOKIE_SECURE=1` at the same time.
- Configuration export deliberately excludes accounts, sessions, run history,
  and TLS material.

## License

MIT. See [LICENSE.md](LICENSE.md).
