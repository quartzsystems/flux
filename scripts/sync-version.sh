#!/usr/bin/env bash
#
# Propagates the root VERSION file into the manifests that cannot read it.
#
# VERSION is the single source of truth. The Rust binaries read it directly at
# build time (see crates/flux-core/build.rs), but Cargo and npm both want a
# literal in their manifest, so those two are written here and checked in CI:
#
#     scripts/sync-version.sh --set 0.2.0  # raise the version and propagate it
#     scripts/sync-version.sh              # propagate the current VERSION
#     scripts/sync-version.sh --check      # fail if they are out of step
#
# The check is what makes VERSION authoritative rather than merely first. Without
# it the manifests drift, and the first anyone notices is a release tarball whose
# name disagrees with the binary inside it.

set -euo pipefail

# Assigned before being made readonly: `readonly X="$(cmd)"` returns the status
# of `readonly`, not of the command substitution, so a failing cd would go
# unnoticed under `set -e`.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly ROOT
readonly VERSION_FILE="$ROOT/VERSION"
readonly CARGO_TOML="$ROOT/Cargo.toml"
readonly PACKAGE_JSON="$ROOT/web/package.json"

die() { printf 'sync-version: %s\n' "$*" >&2; exit 1; }

# Semantic version with an optional pre-release. Defined once so that setting a
# version and validating the file cannot disagree about what a version is.
readonly SEMVER='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'

check_only=false
new_version=""
case "${1:-}" in
    --check) check_only=true ;;
    --set)   new_version="${2:?--set needs a version, e.g. --set 0.2.0}" ;;
    "")      ;;
    *)       die "unknown argument ${1}; expected --check, --set <version>, or nothing" ;;
esac

# `--set` writes VERSION and then falls through to the sync below, so raising
# the version and propagating it are one operation. Keeping them separate is
# what let a release go out with the manifests a version behind.
if [[ -n $new_version ]]; then
    [[ $new_version =~ $SEMVER ]] \
        || die "'$new_version' is not a version like 1.2.3 or 1.2.3-rc.1"
    printf '%s\n' "$new_version" > "$VERSION_FILE"
    printf 'sync-version: VERSION set to %s\n' "$new_version"
fi

[[ -f $VERSION_FILE ]] || die "no VERSION file at $VERSION_FILE"
version="$(tr -d '[:space:]' < "$VERSION_FILE")"

# Validated on the way out as well as in, because VERSION is edited by hand more
# often than through --set, and a malformed version becomes a release tag, a
# tarball name, and a systemd unit description before anyone reads it.
[[ $version =~ $SEMVER ]] \
    || die "VERSION holds ${version@Q}, which is not a version like 1.2.3 or 1.2.3-rc.1"

# --- Cargo -----------------------------------------------------------------
#
# Scoped to [workspace.package]. A blanket substitution would rewrite every
# dependency's version in [workspace.dependencies] as well.

cargo_version() {
    awk '
        /^\[/            { in_section = ($0 == "[workspace.package]") }
        in_section && /^version *= *"/ {
            match($0, /"[^"]*"/)
            print substr($0, RSTART + 1, RLENGTH - 2)
            exit
        }
    ' "$CARGO_TOML"
}

write_cargo_version() {
    local tmp
    tmp="$(mktemp)"
    awk -v version="$1" '
        /^\[/ { in_section = ($0 == "[workspace.package]") }
        in_section && !done && /^version *= *"/ {
            print "version = \"" version "\""
            done = 1
            next
        }
        { print }
    ' "$CARGO_TOML" > "$tmp"
    mv "$tmp" "$CARGO_TOML"
}

# --- npm -------------------------------------------------------------------
#
# The top-level "version" key only. Anchored on the two-space indent that npm
# itself writes, so a nested "version" inside a dependency block cannot match.

json_version() {
    sed -n 's/^  "version": "\([^"]*\)",$/\1/p' "$1" | head -n 1
}

write_json_version() {
    local file="$1" version="$2" tmp
    tmp="$(mktemp)"
    sed -E 's|^  "version": "[^"]*",$|  "version": "'"$version"'",|' "$file" > "$tmp"
    mv "$tmp" "$file"
}

# --- Apply or check --------------------------------------------------------

declare -a drifted=()

[[ "$(cargo_version)" == "$version" ]] || drifted+=("Cargo.toml")
[[ "$(json_version "$PACKAGE_JSON")" == "$version" ]] || drifted+=("web/package.json")

if $check_only; then
    if ((${#drifted[@]} > 0)); then
        printf 'sync-version: %s disagree with VERSION (%s)\n' "${drifted[*]}" "$version" >&2
        printf 'sync-version: run scripts/sync-version.sh to fix\n' >&2
        exit 1
    fi
    printf 'sync-version: everything agrees on %s\n' "$version"
    exit 0
fi

write_cargo_version "$version"
write_json_version "$PACKAGE_JSON" "$version"

# The lockfile records the workspace crates' own versions, so it moves too.
# Regenerated rather than edited: `cargo metadata` is the cheapest command that
# rewrites it, and hand-patching a lockfile is how they end up corrupt.
if command -v cargo >/dev/null 2>&1; then
    (cd "$ROOT" && cargo metadata --format-version 1 --offline >/dev/null 2>&1) || true
fi

# Verify the substitutions actually landed rather than silently matching nothing.
[[ "$(cargo_version)" == "$version" ]] || die "failed to write the version into Cargo.toml"
[[ "$(json_version "$PACKAGE_JSON")" == "$version" ]] \
    || die "failed to write the version into web/package.json"

printf 'sync-version: everything now reads %s\n' "$version"
