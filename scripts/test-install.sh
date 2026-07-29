#!/usr/bin/env bash
#
# Exercises the pure logic inside deploy/install.sh.
#
# The installer's side effects need root and a distribution, so they are covered
# by the container smoke test in CI. What is checked here is the reasoning it
# does *before* touching anything: version ordering, which decides whether an
# upgrade is allowed, and the config rewriter, which handles a generated password
# full of characters that would be pattern syntax to sed.
#
# Loaded by stripping the final `main "$@"` line, so sourcing the script defines
# its functions without running an install.

set -euo pipefail

# Assigned before being made readonly: `readonly X="$(cmd)"` returns the status
# of `readonly`, not of the command substitution, so a failing cd would go
# unnoticed under `set -e`.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly ROOT
readonly INSTALLER="$ROOT/deploy/install.sh"

failures=0

ok()   { printf '  ok    %s\n' "$*"; }
fail() { printf '  FAIL  %s\n' "$*" >&2; failures=$((failures + 1)); }

check() {
    local what="$1" expected="$2" actual="$3"
    [[ $expected == "$actual" ]] && ok "$what" || fail "$what: expected '$expected', got '$actual'"
}

# The installer runs main on its last line; drop it so sourcing is inert.
# `head -n -1` is a GNU extension, which is fine — this only runs on Linux CI
# and developer machines with coreutils.
eval "$(head -n -1 "$INSTALLER")"

printf 'version ordering\n'

version_says() { version_lt "$1" "$2" && printf 'older' || printf 'not-older'; }

check "0.1.0 is older than 0.2.0"        older     "$(version_says 0.1.0 0.2.0)"
check "0.2.0 is not older than 0.1.0"    not-older "$(version_says 0.2.0 0.1.0)"
check "a version is not older than itself" not-older "$(version_says 1.4.2 1.4.2)"

# The one a string comparison gets wrong, and the reason this uses sort -V.
check "0.9.0 is older than 0.10.0"       older     "$(version_says 0.9.0 0.10.0)"
check "0.10.0 is not older than 0.9.0"   not-older "$(version_says 0.10.0 0.9.0)"
check "1.9.9 is older than 2.0.0"        older     "$(version_says 1.9.9 2.0.0)"

# A pre-release sorts before the release it leads to, so upgrading from an rc to
# the final version is an upgrade rather than a refused downgrade.
check "1.0.0-rc.1 is older than 1.0.0"   older     "$(version_says 1.0.0-rc.1 1.0.0)"

printf '\nconfiguration rewriting\n'

SYSCONF="$(mktemp -d)"
trap 'rm -rf "$SYSCONF"' EXIT

cat > "$SYSCONF/fluxd.env" <<'EOF'
# A comment
DATABASE_URL=postgres://flux:CHANGE-ME@127.0.0.1:5432/flux
FLUX_ENGINE=trex
FLUX_BIND=0.0.0.0:8080
EOF

check "reads a value"        "trex"          "$(config_value FLUX_ENGINE)"
check "reads the last line"  "0.0.0.0:8080"  "$(config_value FLUX_BIND)"

set_config_value FLUX_ENGINE mock
check "replaces in place" "mock" "$(config_value FLUX_ENGINE)"

# A generated password is 32 characters of mixed case and digits, and the URL
# around it is full of slashes. sed would treat some of that as syntax.
readonly TRICKY='postgres://flux:aB3&x/y\z%s@127.0.0.1:5432/flux'
set_config_value DATABASE_URL "$TRICKY"
check "survives slashes, ampersands, and backslashes" "$TRICKY" "$(config_value DATABASE_URL)"

set_config_value FLUX_TLS_DIR /etc/flux/tls
check "appends a key that was absent" "/etc/flux/tls" "$(config_value FLUX_TLS_DIR)"

check "rewriting does not duplicate keys" "1" \
    "$(grep -c '^FLUX_ENGINE=' "$SYSCONF/fluxd.env")"
check "unrelated lines survive" "1" \
    "$(grep -c '^# A comment$' "$SYSCONF/fluxd.env")"

printf '\nport selection\n'

check "reads the configured port" "8080" "$(http_port)"
set_config_value FLUX_BIND "127.0.0.1:9443"
check "follows a changed port"    "9443" "$(http_port)"
set_config_value FLUX_BIND "nonsense"
check "falls back when unparseable" "8080" "$(http_port)"

printf '\npg_hba rewriting\n'

# This shipped broken: the sed used `|` as its delimiter while the pattern needs
# `|` to alternate between the two loopback forms, so sed read the first one as
# the end of the expression and died with "unknown option to `s'" on a real
# AlmaLinux box, taking the rest of the install with it.
HBA="$(mktemp -d)"

stock_el_hba() {
    cat > "$1" <<'EOF'
# TYPE  DATABASE        USER            ADDRESS                 METHOD

# "local" is for Unix domain socket connections only
local   all             all                                     peer
# IPv4 local connections:
host    all             all             127.0.0.1/32            ident
# IPv6 local connections:
host    all             all             ::1/128                 ident
# Allow replication connections from localhost
local   replication     all                                     peer
host    replication     all             127.0.0.1/32            ident
host    replication     all             ::1/128                 ident
EOF
}

stock_el_hba "$HBA/pg_hba.conf"
rewrite_hba_loopback "$HBA/pg_hba.conf" && rewritten=changed || rewritten=untouched
check "a stock EL file is rewritten" changed "$rewritten"

check "IPv4 loopback now uses scram" "1" \
    "$(grep -c '^host    all             all             127\.0\.0\.1/32            scram-sha-256$' "$HBA/pg_hba.conf")"
check "IPv6 loopback now uses scram" "1" \
    "$(grep -c '^host    all             all             ::1/128                 scram-sha-256$' "$HBA/pg_hba.conf")"

# Replication is a different service with different exposure; changing its auth
# was never asked for and would be a surprise.
check "replication lines are left alone" "2" \
    "$(grep -c '^host    replication .* ident$' "$HBA/pg_hba.conf")"
check "the local peer line survives" "1" \
    "$(grep -c '^local   all             all                                     peer$' "$HBA/pg_hba.conf")"
check "no ident remains for host all all" "0" \
    "$(grep -Ec '^host[[:space:]]+all[[:space:]]+all.*ident$' "$HBA/pg_hba.conf")"

# A backup is taken before the file is touched, so there is a way back.
check "the original is backed up" "1" \
    "$(find "$HBA" -name 'pg_hba.conf.flux-backup.*' | wc -l | tr -d ' ')"

# Running it again must be a no-op rather than a second backup and rewrite.
rewrite_hba_loopback "$HBA/pg_hba.conf" && rewritten=changed || rewritten=untouched
check "a second pass changes nothing" untouched "$rewritten"

# Debian ships scram already, and a hand-tuned file must not be second-guessed.
printf 'host    all             all             127.0.0.1/32            scram-sha-256\n' \
    > "$HBA/debian.conf"
rewrite_hba_loopback "$HBA/debian.conf" && rewritten=changed || rewritten=untouched
check "an already-scram file is untouched" untouched "$rewritten"

printf 'host    all             all             127.0.0.1/32            md5\n' > "$HBA/custom.conf"
rewrite_hba_loopback "$HBA/custom.conf" && rewritten=changed || rewritten=untouched
check "a customised method is untouched" untouched "$rewritten"

rm -rf "$HBA"

printf '\nappliance hostname\n'

# `hostname` is absent from several minimal EL images, so the summary must not
# depend on it. Each fallback is exercised in a subshell so shadowing `have`
# does not leak into the checks below.
check "names the loopback when nothing else is known" "127.0.0.1" \
    "$(HOSTNAME="" bash -c '
        eval "$(head -n -1 "'"$INSTALLER"'")"
        have() { [[ $1 != hostname ]] && command -v "$1" >/dev/null 2>&1; }
        appliance_host
    ')"

check "falls back to the shell's own HOSTNAME" "from-the-shell" \
    "$(HOSTNAME="from-the-shell" bash -c '
        eval "$(head -n -1 "'"$INSTALLER"'")"
        have() { [[ $1 != hostname ]] && command -v "$1" >/dev/null 2>&1; }
        appliance_host
    ')"

printf '\nchecksum verification\n'

verdict() { verify_checksum "$1" "$2" 2>/dev/null && printf 'accepted' || printf 'rejected'; }

SUMS="$(mktemp -d)"
printf 'the payload\n' > "$SUMS/flux-1.0.0-x86_64-linux.tar.gz"
printf 'something else\n' > "$SUMS/other.tar.gz"
good="$(sha256sum "$SUMS/flux-1.0.0-x86_64-linux.tar.gz" | cut -d' ' -f1)"

# GNU coreutils writes two spaces; the same tool in binary mode writes a space
# and an asterisk. Both have to verify.
printf '%s  flux-1.0.0-x86_64-linux.tar.gz\n' "$good" > "$SUMS/SHA256SUMS"
check "two-space separator"  accepted "$(verdict "$SUMS" flux-1.0.0-x86_64-linux.tar.gz)"

printf '%s *flux-1.0.0-x86_64-linux.tar.gz\n' "$good" > "$SUMS/SHA256SUMS"
check "binary-mode separator" accepted "$(verdict "$SUMS" flux-1.0.0-x86_64-linux.tar.gz)"

printf '%s  flux-1.0.0-x86_64-linux.tar.gz\n' "${good//[0-9]/0}" > "$SUMS/SHA256SUMS"
check "a wrong hash is refused" rejected "$(verdict "$SUMS" flux-1.0.0-x86_64-linux.tar.gz)"

# The dangerous case: the file downloaded is not mentioned at all. Selecting no
# line must fail rather than verify nothing and report success.
printf '%s  other.tar.gz\n' "$good" > "$SUMS/SHA256SUMS"
check "an unlisted file is refused" rejected "$(verdict "$SUMS" flux-1.0.0-x86_64-linux.tar.gz)"

# The dots in a version are regex metacharacters, so a pattern-matched lookup
# would accept a neighbouring name. An exact comparison does not.
printf '%s  flux-1X0X0-x86_64-linux.tar.gz\n' "$good" > "$SUMS/SHA256SUMS"
check "a name that only matches as a regex is refused" rejected \
    "$(verdict "$SUMS" flux-1.0.0-x86_64-linux.tar.gz)"

rm -rf "$SUMS"

printf '\nset -e hazards\n'

# `cond && action` as the last statement of a function returns 1 when the
# condition is false, and under `set -e` that kills the caller with no message.
# The installer is full of optional steps written that way, so every function
# that ends in one has to end in an explicit `return 0` instead.
#
# Checked by calling each of them in a context where their optional work has
# nothing to do — no backup directory, no systemd, nothing installed.
guard_survives() {
    local fn="$1"
    ( set -euo pipefail; "$fn" >/dev/null 2>&1 ) && printf 'survived' || printf 'died'
}

# Arranged so each function actually reaches its tail rather than returning at
# an early guard — a test that passes because it short-circuited proves nothing.
#
# Exported rather than plainly assigned. These are read by the installer's
# functions, which arrive through the `eval` above; shellcheck cannot follow that
# and reports every one of them as written-but-never-read. `export` is its own
# documented answer for a variable used outside the file, and it is accurate
# here — these are handed to code defined elsewhere.
readonly SANDBOX="$SYSCONF"
export BACKUP_ROOT="$SANDBOX/backups"
export PREFIX="$SANDBOX/prefix"   # so $PREFIX/bin/fluxd is absent
export WEBROOT="$SANDBOX/webroot" # absent too
export STAGE="$SANDBOX/stage"
export BACKUP_DIR=""
export IS_UPGRADE=true            # past the early return, into the copy guards
export DO_METRICS=false
export PURGE=false
mkdir -p "$BACKUP_ROOT" "$STAGE/config"
: > "$STAGE/config/fluxd.env.example"
: > "$STAGE/config/portd.yaml.example"

# Every optional copy inside has nothing to copy, so each `[[ -e … ]] && cp`
# guard fails — which is exactly the shape that used to kill the caller.
check "backup_current with no binaries to save" survived "$(guard_survives backup_current)"

# fluxd.env and portd.yaml both exist here, so this takes the upgrade branch.
: > "$SANDBOX/portd.yaml"
check "ensure_config over existing files"       survived "$(guard_survives ensure_config)"

check "prune_backups below the keep count"      survived "$(guard_survives prune_backups)"
check "stop_services without systemd"           survived "$(guard_survives stop_services)"
check "cleanup with the staging dir gone"       survived "$(guard_survives cleanup)"

# And the guard itself is worth checking: if this pattern ever stopped being a
# hazard, the tests above would be protecting against nothing.
hazard() { [[ -e /definitely/not/here ]] && echo found; }
check "a bare trailing guard really does kill the caller" died "$(guard_survives hazard)"

printf '\n'
if ((failures > 0)); then
    printf '%d check(s) failed\n' "$failures" >&2
    exit 1
fi
printf 'all checks passed\n'
