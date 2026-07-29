#!/usr/bin/env bash
#
# Builds the release tarball from artifacts that are already compiled.
#
#     scripts/package.sh [--target <rust-target>] [--out <dir>]
#
# CI and `make dist` both call this, so the layout inside the tarball is defined
# in exactly one place — the same place install.sh reads it from.
#
# The tarball is self-installing: unpack it anywhere and run ./install.sh, which
# finds bin/ beside itself and skips the download.

set -euo pipefail

# Assigned before being made readonly: `readonly X="$(cmd)"` returns the status
# of `readonly`, not of the command substitution, so a failing cd would go
# unnoticed under `set -e`.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly ROOT

TARGET=""
OUT="$ROOT/dist"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target) TARGET="${2:?--target needs a value}"; shift 2 ;;
        --out)    OUT="${2:?--out needs a value}"; shift 2 ;;
        *)        printf 'package: unknown option %s\n' "$1" >&2; exit 1 ;;
    esac
done

die() { printf 'package: %s\n' "$*" >&2; exit 1; }

version="$(tr -d '[:space:]' < "$ROOT/VERSION")"
[[ -n $version ]] || die "VERSION is empty"

# Cross-compiled output lands under target/<triple>/release.
if [[ -n $TARGET ]]; then
    bin_dir="$ROOT/target/$TARGET/release"
    arch="${TARGET%%-*}"
else
    bin_dir="$ROOT/target/release"
    arch="$(uname -m)"
fi

for artifact in "$bin_dir/fluxd" "$bin_dir/flux-portd" "$ROOT/web/out/index.html"; do
    [[ -e $artifact ]] || die "missing $artifact — build first (make build web-build)"
done

# The binaries must agree with VERSION. They read it at build time, so a mismatch
# means the tree changed after they were compiled and the tarball would be named
# for something other than what is inside it.
for binary in fluxd flux-portd; do
    reported="$("$bin_dir/$binary" --version 2>/dev/null | awk '{print $2}')" || reported=""
    if [[ -n $reported && $reported != "$version" ]]; then
        die "$binary reports $reported but VERSION says $version — rebuild"
    fi
done

name="flux-${version}-${arch}-linux"
staging="$(mktemp -d)"
root="$staging/$name"
trap 'rm -rf "$staging"' EXIT

mkdir -p "$root"/{bin,web,systemd,config,sql}

install -m0755 "$bin_dir/fluxd"      "$root/bin/fluxd"
install -m0755 "$bin_dir/flux-portd" "$root/bin/flux-portd"
cp -a "$ROOT/web/out/."        "$root/web/"
cp -a "$ROOT/deploy/systemd/." "$root/systemd/"
cp -a "$ROOT/deploy/flux/."    "$root/config/"
cp -a "$ROOT/deploy/sql/."     "$root/sql/"

install -m0755 "$ROOT/deploy/install.sh" "$root/install.sh"
install -m0644 "$ROOT/VERSION"           "$root/VERSION"
install -m0644 "$ROOT/LICENSE.md"        "$root/LICENSE.md"

# README.md is deliberately not shipped. Its links are relative to the
# repository — docs/, .env.example, the brand SVG — and none of those exist in
# a tarball, so it would arrive as a page of dangling links. `install.sh --help`
# and the release notes cover what someone unpacking this needs.

mkdir -p "$OUT"
tarball="$OUT/$name.tar.gz"

# Reproducible-ish: sorted entries, fixed ownership, and mtimes pinned to the
# VERSION file so rebuilding the same commit produces the same bytes.
tar --create --gzip \
    --file "$tarball" \
    --directory "$staging" \
    --owner=0 --group=0 --numeric-owner \
    --sort=name \
    --mtime="@$(stat -c %Y "$ROOT/VERSION")" \
    "$name"

# Regenerated across everything in the output directory rather than appended to,
# so packaging twice does not leave a duplicate line the installer would then
# check the same file against twice. Bare names, no ./ prefix: install.sh looks
# the tarball up by the exact name it downloaded.
( cd "$OUT" && sha256sum -- *.tar.gz > SHA256SUMS )

printf 'package: wrote %s (%s)\n' "$tarball" "$(du -h "$tarball" | cut -f1)"
