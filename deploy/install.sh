#!/usr/bin/env bash
#
# Installs Flux onto an AlmaLinux 10 appliance.
#
# Assumes the binaries and the exported UI have already been built — this script
# places files, creates accounts, and enables units. It does not compile
# anything, so it can run on an appliance image with no toolchain.
#
# Build first, on a machine that has one:
#
#     make build web-build
#
# then run this from the repository root as root.

set -euo pipefail

readonly PREFIX="${PREFIX:-/usr}"
readonly SYSCONF="${SYSCONF:-/etc/flux}"
readonly WEBROOT="${WEBROOT:-/usr/share/flux/web}"
readonly REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

log()  { printf '\033[0;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[0;33m==>\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[0;31m==>\033[0m %s\n' "$*" >&2; exit 1; }

[[ $EUID -eq 0 ]] || die "run as root"

# --- Preflight --------------------------------------------------------------

for artifact in \
    "$REPO_ROOT/target/release/fluxd" \
    "$REPO_ROOT/target/release/flux-portd" \
    "$REPO_ROOT/web/out/index.html"
do
    [[ -e "$artifact" ]] || die "missing $artifact — run 'make build web-build' first"
done

command -v dpdk-devbind.py >/dev/null \
    || warn "dpdk-devbind.py not found; port binding will fail until DPDK tools are installed"

# --- Accounts ---------------------------------------------------------------
#
# A system account with no login shell and no home directory. flux-portd runs as
# root but creates its socket in a directory group-owned by flux, which is how
# the unprivileged daemon reaches it and nothing else does.

if ! getent group flux >/dev/null; then
    log "creating group flux"
    groupadd --system flux
fi

if ! getent passwd flux >/dev/null; then
    log "creating user flux"
    useradd --system --gid flux --no-create-home \
            --home-dir /var/lib/flux --shell /sbin/nologin flux
fi

# --- Binaries and assets ----------------------------------------------------

log "installing binaries into $PREFIX/bin"
install -m 0755 "$REPO_ROOT/target/release/fluxd"      "$PREFIX/bin/fluxd"
install -m 0755 "$REPO_ROOT/target/release/flux-portd" "$PREFIX/bin/flux-portd"

log "installing the web UI into $WEBROOT"
rm -rf "$WEBROOT"
install -d -m 0755 "$WEBROOT"
cp -r "$REPO_ROOT/web/out/." "$WEBROOT/"

# --- Configuration ----------------------------------------------------------
#
# Never overwrite an existing config: this script is also the upgrade path, and
# clobbering /etc/flux/fluxd.env would drop the database password.

install -d -m 0750 -o root -g flux "$SYSCONF"

if [[ ! -f "$SYSCONF/fluxd.env" ]]; then
    log "installing $SYSCONF/fluxd.env"
    install -m 0640 -o root -g flux \
        "$REPO_ROOT/deploy/flux/fluxd.env.example" "$SYSCONF/fluxd.env"
    warn "edit $SYSCONF/fluxd.env and set DATABASE_URL before starting fluxd"
else
    log "keeping existing $SYSCONF/fluxd.env"
fi

if [[ ! -f "$SYSCONF/portd.yaml" ]]; then
    log "installing $SYSCONF/portd.yaml"
    install -m 0640 -o root -g flux \
        "$REPO_ROOT/deploy/flux/portd.yaml.example" "$SYSCONF/portd.yaml"
    warn "edit $SYSCONF/portd.yaml — the example addresses are placeholders"
    warn "the management NIC must NOT be listed there"
else
    log "keeping existing $SYSCONF/portd.yaml"
fi

install -d -m 0750 -o flux -g flux /var/lib/flux

# Certificates are uploaded through the UI, so the daemon writes here. Mode 0700
# because the private key lives in it and nothing but fluxd has any business
# reading it — the daemon writes the key 0600 as well, but the directory is what
# stops another local account from listing what is there.
install -d -m 0700 -o flux -g flux "$SYSCONF/tls"

# --- Units ------------------------------------------------------------------

log "installing systemd units"
install -m 0644 "$REPO_ROOT/deploy/systemd/fluxd.service"      /etc/systemd/system/
install -m 0644 "$REPO_ROOT/deploy/systemd/flux-portd.service" /etc/systemd/system/
systemctl daemon-reload

# --- Next steps -------------------------------------------------------------

cat <<'NEXT'

Installed. Remaining steps, in order:

  1. Create the database, once, as a Postgres superuser:

       psql "postgres://postgres@localhost/postgres" \
            -v flux_password="'a-real-password'" \
            -f deploy/sql/bootstrap.sql

     Then put the same password into DATABASE_URL in /etc/flux/fluxd.env.

  2. List the data-plane NICs in /etc/flux/portd.yaml.
     Confirm the management NIC is NOT among them:

       ip route show default          # this interface must not be listed

  3. Reserve hugepages for DPDK. Add to the kernel command line and reboot:

       default_hugepagesz=1G hugepagesz=1G hugepages=16 iommu=pt intel_iommu=on

  4. Install and start VictoriaMetrics, listening on 127.0.0.1:8428. It stores
     the per-second samples behind the analytics page. Flux runs without it —
     tests execute and report from Postgres either way — but the historical
     charts stay empty until it is up.

  5. Start the services:

       systemctl enable --now flux-portd fluxd

  6. Read the generated administrator password out of the journal — it is
     printed exactly once, on the first start with an empty users table:

       journalctl -u fluxd | grep -A4 'first administrator'

  7. Open http://<appliance>:8080/

  8. Install a certificate under Settings -> TLS, set FLUX_COOKIE_SECURE=1 in
     /etc/flux/fluxd.env, and restart fluxd. Until then the session cookie —
     which is a bearer credential — crosses the network in the clear.

NEXT
