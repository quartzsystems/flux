#!/usr/bin/env bash
#
# The appliance's version.
#
#     scripts/version.sh              # print it
#     scripts/version.sh --set 0.2.0  # raise it
#     scripts/version.sh --check      # validate it, and that nothing shadows it
#
# VERSION at the repository root is the only place a version is written. The
# binaries read it at compile time through crates/flux-core/build.rs, the
# release tarball is named from it, and the release workflow refuses to publish
# a tag that disagrees with it.
#
# It used to be copied into Cargo.toml and web/package.json and kept in step by
# this script. That copy drifted twice and blocked two releases, so it is gone:
# both manifests now carry a placeholder, and `--check` enforces that they still
# do. A duplicate that has to be maintained is a duplicate that will not be.

set -euo pipefail

# Assigned before being made readonly: `readonly X="$(cmd)"` returns the status
# of `readonly`, not of the command substitution, so a failing cd would go
# unnoticed under `set -e`.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly ROOT
readonly VERSION_FILE="$ROOT/VERSION"
readonly CARGO_TOML="$ROOT/Cargo.toml"
readonly PACKAGE_JSON="$ROOT/web/package.json"

# The version the manifests are expected to hold, forever. Anything else means
# somebody has started a second source of truth.
readonly PLACEHOLDER="0.0.0"

# Semantic version with an optional pre-release. Defined once so that setting a
# version and validating one cannot disagree about what a version is.
readonly SEMVER='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'

die() { printf 'version: %s\n' "$*" >&2; exit 1; }

# --- Reading the manifests --------------------------------------------------
#
# Scoped to [workspace.package]; a blanket match would find every dependency's
# version in [workspace.dependencies] too.

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

# The top-level "version" key only, anchored on the two-space indent npm writes,
# so a nested one inside a dependency block cannot match.
json_version() {
    sed -n 's/^  "version": "\([^"]*\)",$/\1/p' "$PACKAGE_JSON" | head -n 1
}

# --- Commands ---------------------------------------------------------------

read_version() {
    [[ -f $VERSION_FILE ]] || die "no VERSION file at $VERSION_FILE"
    tr -d '[:space:]' < "$VERSION_FILE"
}

do_set() {
    local wanted="$1"
    [[ $wanted =~ $SEMVER ]] \
        || die "'$wanted' is not a version like 1.2.3 or 1.2.3-rc.1"

    # Validated before anything is written, so a typo leaves VERSION as it was.
    printf '%s\n' "$wanted" > "$VERSION_FILE"
    printf 'version: now %s\n' "$wanted"
    printf 'version: nothing else to update — the binaries read this file directly\n'
}

do_check() {
    local version cargo json failed=0
    version="$(read_version)"

    if [[ ! $version =~ $SEMVER ]]; then
        printf "version: VERSION holds '%s', which is not a version like 1.2.3\n" "$version" >&2
        failed=1
    fi

    # Not "does it match VERSION" but "is it still inert". Checking for a match
    # is what required the two to be synchronised in the first place.
    cargo="$(cargo_version)"
    if [[ $cargo != "$PLACEHOLDER" ]]; then
        printf "version: Cargo.toml [workspace.package] says '%s'\n" "$cargo" >&2
        printf "version: it must stay at %s — the version belongs in VERSION alone\n" \
            "$PLACEHOLDER" >&2
        failed=1
    fi

    json="$(json_version)"
    if [[ $json != "$PLACEHOLDER" ]]; then
        printf "version: web/package.json says '%s'\n" "$json" >&2
        printf "version: it must stay at %s — the version belongs in VERSION alone\n" \
            "$PLACEHOLDER" >&2
        failed=1
    fi

    ((failed == 0)) || exit 1
    printf 'version: %s, and nothing shadows it\n' "$version"
}

case "${1:-}" in
    --check) do_check ;;
    --set)   do_set "${2:?--set needs a version, e.g. --set 0.2.0}" ;;
    "")      printf '%s\n' "$(read_version)" ;;
    *)       die "unknown argument '$1'; expected --check, --set <version>, or nothing" ;;
esac
