#!/usr/bin/env bash
#
# Installs, upgrades, and removes Flux on a Debian- or EL-based appliance.
#
#     curl -fsSL https://raw.githubusercontent.com/quartzsystems/flux/main/deploy/install.sh | sudo bash
#
# Running it again upgrades in place: configuration, the database, and run
# history are preserved, the previous binaries are kept, and a start that fails
# its health check rolls back to them automatically.
#
# The whole thing is wrapped in main() and called on the last line, so a download
# truncated mid-flight cannot execute half an installer.

set -euo pipefail

# --- Defaults ---------------------------------------------------------------

readonly REPO="quartzsystems/flux"
readonly SERVICE_USER="flux"
readonly DB_NAME="flux"
readonly DB_USER="flux"
readonly BACKUPS_KEPT=3
readonly HEALTH_TIMEOUT=60

# Every path is an overridable default rather than a constant, which is what
# lets the test harness point them at a sandbox instead of at the live system.
PREFIX="${PREFIX:-/usr}"
SYSCONF="${SYSCONF:-/etc/flux}"
WEBROOT="${WEBROOT:-/usr/share/flux/web}"
STATE_DIR="${STATE_DIR:-/var/lib/flux}"
VERSION_STAMP="${VERSION_STAMP:-$STATE_DIR/installed-version}"
BACKUP_ROOT="${BACKUP_ROOT:-$STATE_DIR/backups}"

WANT_VERSION="latest"
FROM_SOURCE=false
DATABASE_URL_OVERRIDE=""
ENGINE="trex"
DO_DEPS=true
DO_DB=true
DO_METRICS=true
DO_FIREWALL=true
DO_START=true
ALLOW_DOWNGRADE=false
UNINSTALL=false
PURGE=false

# Filled in as the run proceeds.
OS_FAMILY=""
OS_PRETTY=""
ARCH=""
STAGE=""
TARGET_VERSION=""
CURRENT_VERSION=""
BACKUP_DIR=""
IS_UPGRADE=false
FRESH_CONFIG=false

# --- Output -----------------------------------------------------------------

if [[ -t 1 ]]; then
    C_STEP=$'\033[0;32m'; C_WARN=$'\033[0;33m'; C_ERR=$'\033[0;31m'; C_OFF=$'\033[0m'
else
    C_STEP=""; C_WARN=""; C_ERR=""; C_OFF=""
fi

step() { printf '%s==>%s %s\n'    "$C_STEP" "$C_OFF" "$*"; }
info() { printf '    %s\n' "$*"; }
warn() { printf '%s==>%s %s\n'    "$C_WARN" "$C_OFF" "$*" >&2; }
die()  { printf '%serror:%s %s\n' "$C_ERR"  "$C_OFF" "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

# systemd is absent when installing into a container image being built. Every
# unit operation checks this rather than failing the install, because placing the
# files is still the right outcome there.
have_systemd() { [[ -d /run/systemd/system ]] && have systemctl; }

usage() {
    cat <<'EOF'
Installs or upgrades Flux.

Usage: install.sh [options]

  --version <x.y.z>     Release to install. Default: latest
  --from-source         Install from this checkout's target/release and web/out
                        instead of downloading a release
  --database-url <url>  Use an existing PostgreSQL rather than provisioning one
  --engine <trex|mock>  Packet engine for a fresh install. Default: trex
                        (mock simulates a four-port chassis, for evaluation)

  --prefix <dir>        Default: /usr
  --sysconfdir <dir>    Default: /etc/flux
  --webroot <dir>       Default: /usr/share/flux/web

  --no-deps             Do not install distribution packages
  --no-db               Do not create the role and database
  --no-metrics          Do not install VictoriaMetrics
  --no-firewall         Do not open the HTTP port
  --no-start            Place everything but do not start any service
  --allow-downgrade     Permit installing an older version than is present

  --uninstall           Stop and remove Flux, keeping configuration and data
  --purge               With --uninstall, also remove configuration, state,
                        and the database. Destroys every run and account.

  -h, --help            This text

Upgrading is the same command as installing. Configuration files are never
overwritten, the database is dumped first, and a start that fails its health
check rolls the binaries back.
EOF
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version)         WANT_VERSION="${2:?--version needs a value}"; shift 2 ;;
            --from-source)     FROM_SOURCE=true; shift ;;
            --database-url)    DATABASE_URL_OVERRIDE="${2:?--database-url needs a value}"; shift 2 ;;
            --engine)          ENGINE="${2:?--engine needs a value}"; shift 2 ;;
            --prefix)          PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
            --sysconfdir)      SYSCONF="${2:?--sysconfdir needs a value}"; shift 2 ;;
            --webroot)         WEBROOT="${2:?--webroot needs a value}"; shift 2 ;;
            --no-deps)         DO_DEPS=false; shift ;;
            --no-db)           DO_DB=false; shift ;;
            --no-metrics)      DO_METRICS=false; shift ;;
            --no-firewall)     DO_FIREWALL=false; shift ;;
            --no-start)        DO_START=false; shift ;;
            --allow-downgrade) ALLOW_DOWNGRADE=true; shift ;;
            --uninstall)       UNINSTALL=true; shift ;;
            --purge)           PURGE=true; shift ;;
            -h|--help)         usage; exit 0 ;;
            *)                 die "unknown option $1 (try --help)" ;;
        esac
    done

    case "$ENGINE" in
        trex|mock) ;;
        *) die "--engine must be trex or mock, got '$ENGINE'" ;;
    esac

    if $PURGE && ! $UNINSTALL; then
        die "--purge only means something together with --uninstall"
    fi
}

# --- Platform ---------------------------------------------------------------

detect_platform() {
    [[ $EUID -eq 0 ]] || die "run as root (try: sudo bash install.sh)"
    [[ -r /etc/os-release ]] \
        || die "no /etc/os-release; this does not look like a Linux distribution I know"

    # shellcheck disable=SC1091
    . /etc/os-release
    OS_PRETTY="${PRETTY_NAME:-${NAME:-unknown}}"

    # ID_LIKE is what makes this work on derivatives — Rocky says "rhel centos
    # fedora", Ubuntu says "debian" — without naming every downstream distro.
    local haystack=" ${ID:-} ${ID_LIKE:-} "
    if [[ $haystack == *" rhel "* || $haystack == *" fedora "* || $haystack == *" centos "* ]]; then
        OS_FAMILY="el"
    elif [[ $haystack == *" debian "* || $haystack == *" ubuntu "* ]]; then
        OS_FAMILY="debian"
    else
        die "unsupported distribution '$OS_PRETTY': expected a Debian- or RHEL-based system"
    fi

    case "$(uname -m)" in
        x86_64|amd64)  ARCH="x86_64" ;;
        aarch64|arm64) ARCH="aarch64" ;;
        *) die "unsupported architecture $(uname -m); releases are built for x86_64 and aarch64" ;;
    esac

    step "$OS_PRETTY ($OS_FAMILY family, $ARCH)"
    have_systemd || warn "systemd is not running; units will be placed but nothing started"
}

# --- Packages ---------------------------------------------------------------

pkg_install() {
    (($# > 0)) || return 0
    step "Installing packages: $*"

    case "$OS_FAMILY" in
        el)
            local mgr=dnf
            have dnf || mgr=yum
            "$mgr" install -y "$@"
            ;;
        debian)
            DEBIAN_FRONTEND=noninteractive apt-get update -qq
            DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$@"
            ;;
    esac
}

install_dependencies() {
    $DO_DEPS || { info "skipping package installation (--no-deps)"; return 0; }

    local -a packages=(curl tar ca-certificates)

    if $DO_DB && [[ -z $DATABASE_URL_OVERRIDE ]]; then
        case "$OS_FAMILY" in
            el)     packages+=(postgresql-server postgresql-contrib) ;;
            debian) packages+=(postgresql postgresql-contrib) ;;
        esac
    fi

    pkg_install "${packages[@]}"

    # DPDK's binding helper is what flux-portd shells out to. It lives in a
    # different package per family and is genuinely optional — an appliance on
    # the mock engine never calls it — so a failure here is a warning.
    if [[ $ENGINE == trex ]] && ! have dpdk-devbind.py; then
        local dpdk_pkg
        case "$OS_FAMILY" in
            el)     dpdk_pkg="dpdk-tools" ;;
            debian) dpdk_pkg="dpdk" ;;
        esac
        if ! pkg_install "$dpdk_pkg" 2>/dev/null; then
            warn "could not install $dpdk_pkg; port binding will fail until DPDK tools are present"
        fi
    fi
}

# --- Payload ----------------------------------------------------------------

script_dir() { cd "$(dirname "${BASH_SOURCE[0]}")" && pwd; }

# Resolves "latest" through the release redirect rather than the API, which is
# rate-limited when unauthenticated and more to parse.
resolve_latest() {
    local url
    url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPO/releases/latest")" \
        || die "could not reach GitHub to find the latest release"

    local tag="${url##*/}"
    [[ -n $tag && $tag != "releases" && $tag != "latest" ]] \
        || die "no published release found for $REPO; use --from-source, or --version to name one"
    printf '%s' "${tag#v}"
}

stage_payload() {
    STAGE="$(mktemp -d)"

    local here
    here="$(script_dir)"

    # A release tarball carries the installer beside the payload, so finding
    # bin/ next to itself is how the script knows it is running from one.
    if [[ -x $here/bin/fluxd ]]; then
        step "Installing from the release tree at $here"
        cp -a "$here/." "$STAGE/"
        TARGET_VERSION="$(tr -d '[:space:]' < "$STAGE/VERSION")"
        return
    fi

    if $FROM_SOURCE; then
        stage_from_source "$here/.."
        return
    fi

    local version="$WANT_VERSION"
    if [[ $version == latest ]]; then
        step "Resolving the latest release"
        version="$(resolve_latest)"
    fi
    download_release "$version"
}

stage_from_source() {
    local root="$1"
    step "Installing from the checkout at $root"

    local -a required=(
        "$root/target/release/fluxd"
        "$root/target/release/flux-portd"
        "$root/web/out/index.html"
        "$root/VERSION"
    )
    local artifact
    for artifact in "${required[@]}"; do
        [[ -e $artifact ]] || die "missing $artifact — run 'make build web-build' first"
    done

    mkdir -p "$STAGE/bin" "$STAGE/web" "$STAGE/systemd" "$STAGE/config" "$STAGE/sql"
    install -m0755 "$root/target/release/fluxd"      "$STAGE/bin/fluxd"
    install -m0755 "$root/target/release/flux-portd" "$STAGE/bin/flux-portd"
    cp -a "$root/web/out/."        "$STAGE/web/"
    cp -a "$root/deploy/systemd/." "$STAGE/systemd/"
    cp -a "$root/deploy/flux/."    "$STAGE/config/"
    cp -a "$root/deploy/sql/."     "$STAGE/sql/"
    cp    "$root/VERSION"          "$STAGE/VERSION"

    TARGET_VERSION="$(tr -d '[:space:]' < "$STAGE/VERSION")"
}

# Checks one file in a directory against the SHA256SUMS beside it.
#
# The line is selected by an exact filename comparison rather than by grepping
# for it. A tarball name is full of dots, which a regex reads as "any character",
# and the separator is two spaces or a space and an asterisk depending on which
# platform wrote the file — neither of which a naive pattern survives. Selecting
# no line at all is a failure, not a pass: that is the case where an attacker
# supplies a file the checksums say nothing about.
verify_checksum() {
    local dir="$1" name="$2" expected

    expected="$(awk -v want="$name" '
        { candidate = $2; sub(/^\*/, "", candidate); if (candidate == want) print }
    ' "$dir/SHA256SUMS")"

    [[ -n $expected ]] || { warn "$name is not listed in SHA256SUMS"; return 1; }

    ( cd "$dir" && printf '%s\n' "$expected" | sha256sum -c --status - )
}

download_release() {
    local version="$1"
    local tarball="flux-${version}-${ARCH}-linux.tar.gz"
    local base="https://github.com/$REPO/releases/download/v${version}"

    have curl || die "curl is required to download a release"
    step "Downloading Flux $version for $ARCH"

    curl -fsSL --retry 3 -o "$STAGE/$tarball" "$base/$tarball" \
        || die "could not download $base/$tarball"

    # A missing checksum file is a hard failure rather than a skipped check:
    # installing an unverified binary as root is exactly what it exists to stop.
    curl -fsSL --retry 3 -o "$STAGE/SHA256SUMS" "$base/SHA256SUMS" \
        || die "could not download $base/SHA256SUMS"

    step "Verifying the download"
    verify_checksum "$STAGE" "$tarball" \
        || die "checksum mismatch on $tarball — refusing to install it"

    tar -xzf "$STAGE/$tarball" -C "$STAGE" --strip-components=1
    rm -f "$STAGE/$tarball" "$STAGE/SHA256SUMS"

    [[ -x $STAGE/bin/fluxd ]] || die "the release tarball did not contain bin/fluxd"
    TARGET_VERSION="$(tr -d '[:space:]' < "$STAGE/VERSION")"
}

# --- Version comparison -----------------------------------------------------

# True when $1 is strictly older than $2.
#
# `sort -V` alone is not enough. It gets the numeric ordering right — the one a
# string comparison gets wrong between 0.10.0 and 0.9.0 — but it sorts 1.0.0
# *before* 1.0.0-rc.1, which is backwards. Under that ordering, upgrading from a
# release candidate to the release it led to would be refused as a downgrade, so
# the pre-release is compared separately.
version_lt() {
    [[ $1 == "$2" ]] && return 1

    local a_core="${1%%-*}" b_core="${2%%-*}"
    local a_pre="" b_pre=""
    [[ $1 == *-* ]] && a_pre="${1#*-}"
    [[ $2 == *-* ]] && b_pre="${2#*-}"

    if [[ $a_core != "$b_core" ]]; then
        [[ "$(printf '%s\n%s\n' "$a_core" "$b_core" | sort -V | head -n1)" == "$a_core" ]]
        return $?
    fi

    # Same release number: a pre-release of it comes first.
    if [[ -n $a_pre && -z $b_pre ]]; then return 0; fi
    if [[ -z $a_pre && -n $b_pre ]]; then return 1; fi

    [[ "$(printf '%s\n%s\n' "$a_pre" "$b_pre" | sort -V | head -n1)" == "$a_pre" ]]
}

detect_existing() {
    [[ -r $VERSION_STAMP ]] || return 0
    CURRENT_VERSION="$(tr -d '[:space:]' < "$VERSION_STAMP")"
    [[ -n $CURRENT_VERSION ]] || return 0

    IS_UPGRADE=true
    if [[ $CURRENT_VERSION == "$TARGET_VERSION" ]]; then
        step "Reinstalling $TARGET_VERSION over itself"
    elif version_lt "$TARGET_VERSION" "$CURRENT_VERSION"; then
        $ALLOW_DOWNGRADE || die "$CURRENT_VERSION is installed and $TARGET_VERSION is older.
       A downgrade cannot undo a schema migration the newer version applied.
       Restore a dump from $BACKUP_ROOT, or pass --allow-downgrade if you know
       this release still reads the current schema."
        warn "downgrading $CURRENT_VERSION to $TARGET_VERSION at your request"
    else
        step "Upgrading $CURRENT_VERSION to $TARGET_VERSION"
    fi
}

# --- Accounts and directories -----------------------------------------------

ensure_accounts() {
    if ! getent group "$SERVICE_USER" >/dev/null; then
        step "Creating group $SERVICE_USER"
        groupadd --system "$SERVICE_USER"
    fi

    if ! getent passwd "$SERVICE_USER" >/dev/null; then
        step "Creating user $SERVICE_USER"
        useradd --system --gid "$SERVICE_USER" --no-create-home \
                --home-dir "$STATE_DIR" --shell /sbin/nologin "$SERVICE_USER"
    fi

    install -d -m0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$STATE_DIR"
    install -d -m0700 -o "$SERVICE_USER" -g "$SERVICE_USER" "$BACKUP_ROOT"
    install -d -m0750 -o root -g "$SERVICE_USER" "$SYSCONF"

    # An uploaded certificate's private key lives here, so nothing but the
    # daemon has any business listing it.
    install -d -m0700 -o "$SERVICE_USER" -g "$SERVICE_USER" "$SYSCONF/tls"
}

# --- Backup and rollback ----------------------------------------------------

backup_current() {
    $IS_UPGRADE || return 0

    BACKUP_DIR="$BACKUP_ROOT/${CURRENT_VERSION}-$(date -u +%Y%m%dT%H%M%SZ)"
    step "Backing up $CURRENT_VERSION to $BACKUP_DIR"
    install -d -m0700 "$BACKUP_DIR"

    local binary
    for binary in fluxd flux-portd; do
        [[ -e $PREFIX/bin/$binary ]] && cp -a "$PREFIX/bin/$binary" "$BACKUP_DIR/$binary"
    done
    [[ -d $WEBROOT ]] && cp -a "$WEBROOT" "$BACKUP_DIR/web"
    [[ -d $SYSCONF ]] && cp -a "$SYSCONF" "$BACKUP_DIR/config"

    backup_database
    return 0
}

# A dump before every upgrade, because migrations are forward-only. Putting the
# old binaries back does not undo one; this is the only thing that does.
backup_database() {
    local url
    url="$(config_value DATABASE_URL || true)"
    [[ -n $url ]] || return 0
    have pg_dump || { warn "pg_dump is not installed; skipping the pre-upgrade dump"; return 0; }

    step "Dumping the database"
    if pg_dump --no-owner --format=custom --file="$BACKUP_DIR/flux.dump" "$url" 2>/dev/null; then
        chmod 0600 "$BACKUP_DIR/flux.dump"
        info "wrote $BACKUP_DIR/flux.dump"
    else
        warn "the database dump failed; continuing without one"
        warn "if this upgrade goes wrong there will be no schema to restore from"
    fi
}

rollback() {
    if [[ -z $BACKUP_DIR || ! -d $BACKUP_DIR ]]; then
        warn "no backup to roll back to"
        return 1
    fi

    warn "rolling back to $CURRENT_VERSION"
    stop_services

    local binary
    for binary in fluxd flux-portd; do
        [[ -e $BACKUP_DIR/$binary ]] && install -m0755 "$BACKUP_DIR/$binary" "$PREFIX/bin/$binary"
    done
    if [[ -d $BACKUP_DIR/web ]]; then
        rm -rf "$WEBROOT"
        cp -a "$BACKUP_DIR/web" "$WEBROOT"
    fi

    printf '%s\n' "$CURRENT_VERSION" > "$VERSION_STAMP"
    start_services || true

    warn "the binaries are back at $CURRENT_VERSION, but any migration $TARGET_VERSION applied"
    warn "is still in place. If $CURRENT_VERSION will not start, restore the dump:"
    warn "  pg_restore --clean --if-exists --no-owner -d '<DATABASE_URL>' $BACKUP_DIR/flux.dump"
}

prune_backups() {
    local -a old=()
    mapfile -t old < <(ls -1dt "$BACKUP_ROOT"/*/ 2>/dev/null | tail -n +$((BACKUPS_KEPT + 1)))
    ((${#old[@]} == 0)) && return 0

    info "pruning ${#old[@]} backup(s) older than the last $BACKUPS_KEPT"
    rm -rf -- "${old[@]}"
}

# --- Files ------------------------------------------------------------------

install_files() {
    step "Installing binaries into $PREFIX/bin"
    # Written beside the target and moved into place: `mv` within a filesystem is
    # atomic, so nothing ever observes a half-written binary. Replacing the path
    # under a running process is safe — it keeps the inode it started with.
    local binary
    for binary in fluxd flux-portd; do
        install -m0755 "$STAGE/bin/$binary" "$PREFIX/bin/.$binary.new"
        mv -f "$PREFIX/bin/.$binary.new" "$PREFIX/bin/$binary"
    done

    step "Installing the web UI into $WEBROOT"
    install -d -m0755 "$(dirname "$WEBROOT")"
    rm -rf "$WEBROOT.new"
    install -d -m0755 "$WEBROOT.new"
    cp -a "$STAGE/web/." "$WEBROOT.new/"
    rm -rf "$WEBROOT"
    mv "$WEBROOT.new" "$WEBROOT"

    step "Installing systemd units"
    local unit
    for unit in "$STAGE"/systemd/*.service; do
        [[ -e $unit ]] || continue
        # VictoriaMetrics' unit is only worth placing if its binary will exist.
        if [[ $(basename "$unit") == victoria-metrics.service ]] && ! $DO_METRICS; then
            continue
        fi
        install -m0644 "$unit" /etc/systemd/system/
    done
    have_systemd && systemctl daemon-reload

    install -m0644 "$STAGE/sql/bootstrap.sql" "$SYSCONF/bootstrap.sql"
}

# --- Configuration ----------------------------------------------------------

# Reads one KEY=value out of the daemon's EnvironmentFile.
config_value() {
    local key="$1"
    [[ -r $SYSCONF/fluxd.env ]] || return 1
    sed -n "s/^${key}=//p" "$SYSCONF/fluxd.env" | tail -n 1
}

set_config_value() {
    local key="$1" value="$2" file="$SYSCONF/fluxd.env" tmp

    if grep -q "^${key}=" "$file"; then
        # A DATABASE_URL is full of characters sed would read as pattern syntax,
        # so the rewrite goes through awk. The value arrives via the environment
        # rather than -v, because -v processes backslash escapes in what it is
        # given and would silently eat one out of a supplied connection string.
        tmp="$(mktemp)"
        FLUX_CFG_KEY="$key" FLUX_CFG_VALUE="$value" awk '
            BEGIN { key = ENVIRON["FLUX_CFG_KEY"]; value = ENVIRON["FLUX_CFG_VALUE"] }
            index($0, key "=") == 1 && !done { print key "=" value; done = 1; next }
            { print }
        ' "$file" > "$tmp"
        cat "$tmp" > "$file"
        rm -f "$tmp"
    else
        printf '%s=%s\n' "$key" "$value" >> "$file"
    fi
}

ensure_config() {
    # Never overwritten. This script is also the upgrade path, and clobbering
    # fluxd.env would take the database credentials with it.
    if [[ ! -f $SYSCONF/fluxd.env ]]; then
        FRESH_CONFIG=true
        step "Writing $SYSCONF/fluxd.env"
        install -m0640 -o root -g "$SERVICE_USER" \
            "$STAGE/config/fluxd.env.example" "$SYSCONF/fluxd.env"
        set_config_value FLUX_ENGINE "$ENGINE"
        [[ $ENGINE == mock ]] && set_config_value FLUX_PORTD mock
    else
        info "keeping the existing $SYSCONF/fluxd.env"
        install -m0644 "$STAGE/config/fluxd.env.example" "$SYSCONF/fluxd.env.example"
        info "this release's example is at $SYSCONF/fluxd.env.example — new settings live there"
    fi

    if [[ ! -f $SYSCONF/portd.yaml ]]; then
        step "Writing $SYSCONF/portd.yaml"
        install -m0640 -o root -g "$SERVICE_USER" \
            "$STAGE/config/portd.yaml.example" "$SYSCONF/portd.yaml"
        if [[ $ENGINE == trex ]]; then
            warn "edit $SYSCONF/portd.yaml — the example PCI addresses are placeholders"
            warn "the management NIC must NOT be listed there"
        fi
    else
        info "keeping the existing $SYSCONF/portd.yaml"
    fi
    return 0
}

# --- Database ---------------------------------------------------------------

random_password() {
    # tr from /dev/urandom rather than openssl, which is not guaranteed present.
    LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 32
}

as_postgres() { runuser -u postgres -- "$@"; }

start_postgres() {
    have_systemd || { warn "no systemd; cannot start PostgreSQL"; return 1; }

    # EL ships an uninitialised data directory; Debian's package creates a
    # cluster during installation.
    if [[ $OS_FAMILY == el && ! -f /var/lib/pgsql/data/PG_VERSION ]]; then
        step "Initialising the PostgreSQL data directory"
        if have postgresql-setup; then
            postgresql-setup --initdb
        else
            as_postgres initdb -D /var/lib/pgsql/data
        fi
    fi

    systemctl enable --now postgresql
}

# EL's stock pg_hba.conf answers loopback TCP with `ident`, which needs an ident
# daemon nobody runs; Debian's already uses scram. Only the two stock loopback
# lines are touched, so a hand-tuned file is left exactly as it is.
allow_local_password_auth() {
    local hba
    hba="$(as_postgres psql -tAc 'SHOW hba_file' 2>/dev/null | tr -d '[:space:]')" || return 0
    [[ -n $hba && -f $hba ]] || return 0

    grep -Eq '^host[[:space:]]+all[[:space:]]+all[[:space:]]+(127\.0\.0\.1/32|::1/128)[[:space:]]+ident[[:space:]]*$' \
        "$hba" || return 0

    step "Allowing password authentication on loopback in $hba"
    cp -a "$hba" "$hba.flux-backup.$(date -u +%Y%m%dT%H%M%SZ)"

    local tmp
    tmp="$(mktemp)"
    sed -E 's|^(host[[:space:]]+all[[:space:]]+all[[:space:]]+(127\.0\.0\.1/32|::1/128)[[:space:]]+)ident[[:space:]]*$|\1scram-sha-256|' \
        "$hba" > "$tmp"
    cat "$tmp" > "$hba"
    rm -f "$tmp"

    # Spelled out rather than left to `set -e`: if neither works the connection
    # test below fails anyway, and this says which step actually went wrong.
    if ! systemctl reload postgresql && ! systemctl restart postgresql; then
        warn "could not reload PostgreSQL after editing $hba"
    fi
    return 0
}

provision_database() {
    if [[ -n $DATABASE_URL_OVERRIDE ]]; then
        step "Using the database you supplied"
        set_config_value DATABASE_URL "$DATABASE_URL_OVERRIDE"
        return 0
    fi

    $DO_DB || { info "skipping database provisioning (--no-db)"; return 0; }

    # An existing DATABASE_URL means this is an upgrade, or an operator set one
    # up by hand. Either way it is not this script's to change.
    local existing
    existing="$(config_value DATABASE_URL || true)"
    if [[ -n $existing && $existing != *CHANGE-ME* ]]; then
        info "keeping the configured DATABASE_URL"
        return 0
    fi

    have psql || { warn "psql is not installed; skipping database provisioning"; return 0; }
    start_postgres || { warn "PostgreSQL is not running; skipping database provisioning"; return 0; }
    allow_local_password_auth

    local password
    password="$(random_password)"

    step "Creating the $DB_USER role and $DB_NAME database"
    if [[ "$(as_postgres psql -tAc "SELECT 1 FROM pg_roles WHERE rolname = '$DB_USER'" \
             | tr -d '[:space:]')" == 1 ]]; then
        warn "the role $DB_USER already exists; resetting its password"
    fi

    as_postgres psql -v ON_ERROR_STOP=1 -q -c "
        DO \$\$ BEGIN
            IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '$DB_USER') THEN
                EXECUTE format('ALTER ROLE $DB_USER LOGIN PASSWORD %L', '$password');
            ELSE
                EXECUTE format('CREATE ROLE $DB_USER LOGIN PASSWORD %L', '$password');
            END IF;
        END \$\$;"

    # CREATE DATABASE cannot run inside a DO block, so it is generated by a
    # query that yields nothing when the database is already there.
    as_postgres psql -v ON_ERROR_STOP=1 -q -tAc \
        "SELECT 'CREATE DATABASE $DB_NAME OWNER $DB_USER'
         WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = '$DB_NAME')" \
        | as_postgres psql -v ON_ERROR_STOP=1 -q

    as_postgres psql -v ON_ERROR_STOP=1 -q -d "$DB_NAME" \
        -c "GRANT ALL ON SCHEMA public TO $DB_USER;"

    step "Testing the connection"
    if ! PGPASSWORD="$password" psql -h 127.0.0.1 -U "$DB_USER" -d "$DB_NAME" \
         -tAc 'SELECT 1' >/dev/null 2>&1; then
        die "created the database but could not connect to it as $DB_USER.
       Check that pg_hba.conf allows password authentication from 127.0.0.1,
       then set DATABASE_URL in $SYSCONF/fluxd.env by hand and start fluxd."
    fi

    set_config_value DATABASE_URL "postgres://${DB_USER}:${password}@127.0.0.1:5432/${DB_NAME}"
    info "DATABASE_URL written to $SYSCONF/fluxd.env"
}

# --- VictoriaMetrics --------------------------------------------------------

# Optional by design: without it a run still executes and still reports, and only
# the historical charts stay empty. So every failure here warns rather than dies.
install_victoria_metrics() {
    $DO_METRICS || { info "skipping VictoriaMetrics (--no-metrics)"; return 0; }
    if have victoria-metrics && $IS_UPGRADE; then
        info "VictoriaMetrics is already installed"
        return 0
    fi
    have curl || { warn "curl is not installed; skipping VictoriaMetrics"; return 0; }

    local vm_arch
    case "$ARCH" in
        x86_64)  vm_arch="amd64" ;;
        aarch64) vm_arch="arm64" ;;
    esac

    step "Installing VictoriaMetrics"
    local url tag
    url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
        https://github.com/VictoriaMetrics/VictoriaMetrics/releases/latest 2>/dev/null)" || true
    tag="${url##*/}"

    if [[ -z $tag || $tag == "latest" || $tag == "releases" ]]; then
        warn "could not determine the latest VictoriaMetrics release; skipping it"
        warn "analytics stays empty until one is serving on FLUX_VM_URL"
        return 0
    fi

    local asset="victoria-metrics-linux-${vm_arch}-${tag}.tar.gz"
    local download="https://github.com/VictoriaMetrics/VictoriaMetrics/releases/download/${tag}/${asset}"

    if ! curl -fsSL --retry 3 -o "$STAGE/$asset" "$download"; then
        warn "could not download $download; skipping VictoriaMetrics"
        return 0
    fi

    tar -xzf "$STAGE/$asset" -C "$STAGE"
    if [[ ! -f $STAGE/victoria-metrics-prod ]]; then
        warn "unexpected VictoriaMetrics archive layout; skipping it"
        return 0
    fi

    install -m0755 "$STAGE/victoria-metrics-prod" "$PREFIX/bin/victoria-metrics"

    if ! getent passwd victoria-metrics >/dev/null; then
        useradd --system --no-create-home --home-dir /var/lib/victoria-metrics \
                --shell /sbin/nologin victoria-metrics
    fi
    install -d -m0750 -o victoria-metrics -g victoria-metrics /var/lib/victoria-metrics

    info "installed VictoriaMetrics $tag"
}

# --- Firewall ---------------------------------------------------------------

# The name to tell the operator to point a browser at.
#
# `hostname` is not guaranteed to be installed — it is absent from several
# minimal EL images — so bash's own $HOSTNAME is the fallback before giving up
# and naming the loopback, which at least works from the console.
appliance_host() {
    local name=""
    have hostname && name="$(hostname -f 2>/dev/null || hostname 2>/dev/null || true)"
    [[ -z $name ]] && name="${HOSTNAME:-}"
    printf '%s' "${name:-127.0.0.1}"
}

http_port() {
    local bind port
    bind="$(config_value FLUX_BIND || true)"
    port="${bind##*:}"
    [[ $port =~ ^[0-9]+$ ]] && printf '%s' "$port" || printf '8080'
}

configure_firewall() {
    $DO_FIREWALL || { info "skipping firewall configuration (--no-firewall)"; return 0; }

    local port
    port="$(http_port)"

    if have firewall-cmd && systemctl is-active --quiet firewalld 2>/dev/null; then
        step "Opening $port/tcp in firewalld"
        firewall-cmd --permanent --add-port="$port/tcp" >/dev/null
        firewall-cmd --reload >/dev/null
    elif have ufw && ufw status 2>/dev/null | grep -q '^Status: active'; then
        step "Opening $port/tcp in ufw"
        ufw allow "$port/tcp" >/dev/null
    else
        info "no active firewall detected; leaving the network alone"
    fi
}

# --- Services ---------------------------------------------------------------

flux_units() {
    local -a units=(flux-portd.service fluxd.service)
    if $DO_METRICS && [[ -f /etc/systemd/system/victoria-metrics.service ]] \
       && have victoria-metrics; then
        units=(victoria-metrics.service "${units[@]}")
    fi
    printf '%s\n' "${units[@]}"
}

stop_services() {
    have_systemd || return 0
    local unit
    while read -r unit; do
        systemctl is-active --quiet "$unit" 2>/dev/null && systemctl stop "$unit"
    done < <(flux_units)
    return 0
}

start_services() {
    have_systemd || return 0
    step "Starting services"
    local unit
    while read -r unit; do
        systemctl enable --now "$unit"
    done < <(flux_units)
}

# fluxd answers /system/health with 401 until you sign in, which is still proof
# that it bound its port and reached the database. Anything but a refused
# connection counts as up.
wait_for_health() {
    have_systemd || return 0
    have curl || { warn "curl is not installed; skipping the health check"; return 0; }

    local port deadline code scheme
    port="$(http_port)"

    step "Waiting for fluxd to answer on port $port"
    deadline=$((SECONDS + HEALTH_TIMEOUT))

    while ((SECONDS < deadline)); do
        if ! systemctl is-active --quiet fluxd; then
            warn "fluxd stopped; its last words were:"
            journalctl -u fluxd -n 25 --no-pager >&2 || true
            return 1
        fi

        # Both schemes, because a certificate may already be installed from a
        # previous run and the listener picks TLS on its own.
        for scheme in http https; do
            code="$(curl -sk -o /dev/null -w '%{http_code}' --max-time 3 \
                "$scheme://127.0.0.1:$port/api/v1/system/health" 2>/dev/null || echo 000)"
            if [[ $code != 000 ]]; then
                info "fluxd is answering over $scheme"
                return 0
            fi
        done
        sleep 2
    done

    warn "fluxd did not answer within ${HEALTH_TIMEOUT}s"
    journalctl -u fluxd -n 25 --no-pager >&2 || true
    return 1
}

# --- Uninstall --------------------------------------------------------------

do_uninstall() {
    step "Removing Flux"
    if have_systemd; then
        local unit
        for unit in fluxd.service flux-portd.service; do
            systemctl disable --now "$unit" 2>/dev/null || true
            rm -f "/etc/systemd/system/$unit"
        done
        systemctl daemon-reload
    fi

    rm -f "$PREFIX/bin/fluxd" "$PREFIX/bin/flux-portd"
    rm -rf "$WEBROOT"

    if $PURGE; then
        warn "purging configuration, state, and the database"

        if have psql && systemctl is-active --quiet postgresql 2>/dev/null; then
            as_postgres psql -q -c "DROP DATABASE IF EXISTS $DB_NAME;" || true
            as_postgres psql -q -c "DROP ROLE IF EXISTS $DB_USER;" || true
        fi

        rm -rf "$SYSCONF" "$STATE_DIR"
        getent passwd "$SERVICE_USER" >/dev/null && userdel "$SERVICE_USER" 2>/dev/null || true
        getent group  "$SERVICE_USER" >/dev/null && groupdel "$SERVICE_USER" 2>/dev/null || true

        step "Flux and all of its data are gone"
    else
        step "Flux is removed"
        info "kept $SYSCONF, $STATE_DIR, and the database"
        info "run again with --uninstall --purge to remove those too"
    fi

    info "VictoriaMetrics was left alone; it may be serving other things"
    return 0
}

# --- Summary ----------------------------------------------------------------

print_summary() {
    printf '\n'
    step "Flux $TARGET_VERSION is installed"

    if ! $DO_START; then
        info "nothing was started (--no-start). When you are ready:"
        info "  systemctl enable --now flux-portd fluxd"
        return 0
    fi

    if ! $FRESH_CONFIG; then
        info "upgraded from $CURRENT_VERSION; configuration and data untouched"
        [[ -n $BACKUP_DIR ]] && info "the previous version is backed up at $BACKUP_DIR"
        return 0
    fi

    cat <<EOF

  Open  http://$(appliance_host):$(http_port)/

  The first administrator password was generated and written to the journal
  exactly once, on this first start:

      journalctl -u fluxd | grep -A4 'first administrator'

EOF

    if [[ $ENGINE == trex ]]; then
        cat <<EOF
  Before it can move traffic:

    1. List the data-plane NICs in $SYSCONF/portd.yaml. Confirm the management
       NIC is not among them:

           ip route show default          # this interface must not be listed

    2. Reserve hugepages for DPDK. Add to the kernel command line and reboot:

           default_hugepagesz=1G hugepagesz=1G hugepages=16 iommu=pt intel_iommu=on

    3. Install Cisco TRex 3.x — Flux supervises it but does not ship it.

EOF
    else
        cat <<EOF
  Running the mock engine: a simulated four-port 100G chassis, so the whole UI
  works without hardware. Set FLUX_ENGINE=trex in $SYSCONF/fluxd.env and restart
  once there are real NICs to drive.

EOF
    fi

    cat <<EOF
  Install a TLS certificate under Settings -> TLS, then set FLUX_COOKIE_SECURE=1
  and restart. Until then the session cookie crosses the network in the clear.

EOF
}

# --- Entry point ------------------------------------------------------------

cleanup() { [[ -n ${STAGE:-} && -d ${STAGE:-} ]] && rm -rf "$STAGE"; return 0; }

main() {
    parse_args "$@"
    detect_platform

    trap cleanup EXIT

    if $UNINSTALL; then
        do_uninstall
        exit 0
    fi

    stage_payload
    [[ -n $TARGET_VERSION ]] || die "could not determine which version is being installed"
    detect_existing

    install_dependencies
    ensure_accounts
    backup_current
    stop_services
    install_files
    ensure_config
    provision_database
    install_victoria_metrics
    configure_firewall

    if $DO_START; then
        start_services
        if ! wait_for_health; then
            if $IS_UPGRADE; then
                rollback
                die "the new version did not come up; rolled back to $CURRENT_VERSION"
            fi
            die "fluxd did not start. The journal above says why; on a fresh install
       the usual cause is DATABASE_URL in $SYSCONF/fluxd.env."
        fi
    fi

    printf '%s\n' "$TARGET_VERSION" > "$VERSION_STAMP"
    prune_backups
    print_summary
}

main "$@"
