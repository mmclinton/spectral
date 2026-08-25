#!/usr/bin/env bash
# Bump every in-repo version spot for a release, or pin the post-tag
# artifacts that need the published tarball.
#
# The release version lives in five files that must move together; this
# script is the authority on that list. It edits the mechanical spots and
# verifies the authored one (the metainfo <release> entry), so a release
# cut cannot silently miss a spot. Any file that no longer matches the
# shape this script expects fails the run loudly — that is drift, and the
# fix is to update this script alongside the file that changed.
#
#     ./packaging/bump-version.sh X.Y.Z
#         prepare: verify the metainfo entry, bump Cargo.toml,
#         Cargo.lock, nix/package.nix, and the PKGBUILD pkgver/pkgrel
#     ./packaging/bump-version.sh --post-tag X.Y.Z
#         after the tag is public: pin the PKGBUILD tarball hash and
#         regenerate .SRCINFO
#
# Run from anywhere inside the repository. The dev shell provides
# everything it needs (cargo, curl, makepkg with MAKEPKG_CONF preset).

set -euo pipefail

die() { echo "bump-version: $*" >&2; exit 1; }

mode=prepare
if [[ "${1:-}" == "--post-tag" ]]; then
    mode=post-tag
    shift
fi
version="${1:-}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "usage: bump-version.sh [--post-tag] X.Y.Z"

cd "$(git rev-parse --show-toplevel)"

metainfo=assets/page.codeberg.mmc.Spectral.metainfo.xml
pkgbuild=packaging/aur/PKGBUILD

# Replace the single line matching an ERE, by line number so neither the
# pattern nor the replacement goes through awk's -v escape processing.
# Zero matches means the file's shape drifted; more than one means the
# pattern is no longer unique. Both are errors, not situations to guess in.
replace_line() {
    local file=$1 pattern=$2 replacement=$3 hits
    hits=$(grep -n -E "$pattern" "$file" | cut -d: -f1) || true
    [[ -n "$hits" && $(echo "$hits" | wc -l) -eq 1 ]] \
        || die "$file: expected exactly one line matching '$pattern'"
    awk -v n="$hits" -v rep="$replacement" \
        'NR == n { print rep; next } { print }' "$file" > "$file.tmp" \
        && mv "$file.tmp" "$file"
}

case $mode in
prepare)
    # Verified, not edited: the release entry is authored prose and it
    # ships inside the tarball, so it must exist before anything moves.
    grep -q "<release version=\"$version\" date=\"" "$metainfo" \
        || die "$metainfo has no <release> entry for $version — author it first (it ships in the tarball)"

    replace_line Cargo.toml \
        '^version = "[0-9]+\.[0-9]+\.[0-9]+"$' \
        "version = \"$version\""
    replace_line nix/package.nix \
        '^  version = "[0-9]+\.[0-9]+\.[0-9]+";$' \
        "  version = \"$version\";"
    replace_line "$pkgbuild" \
        '^pkgver=[0-9]+\.[0-9]+\.[0-9]+$' \
        "pkgver=$version"
    replace_line "$pkgbuild" \
        '^pkgrel=[0-9]+$' \
        "pkgrel=1"

    # Any workspace resolution rewrites the lockfile's own stanza; offline
    # so nothing else can move as a side effect.
    cargo metadata --format-version 1 --offline > /dev/null
    grep -A 1 '^name = "spectral"$' Cargo.lock | grep -q "\"$version\"" \
        || die "Cargo.lock did not pick up $version"

    git --no-pager diff --stat -- \
        Cargo.toml Cargo.lock nix/package.nix "$pkgbuild"
    ;;
post-tag)
    grep -q "^pkgver=$version$" "$pkgbuild" \
        || die "PKGBUILD pkgver is not $version — run the prepare mode first"

    url="https://codeberg.org/mmc/spectral/archive/v$version.tar.gz"
    sha=$(curl -sfL "$url" | sha256sum | cut -d' ' -f1) \
        || die "could not fetch $url — is the tag pushed to Codeberg?"
    replace_line "$pkgbuild" \
        "^sha256sums=\('[0-9a-f]{64}'\)$" \
        "sha256sums=('$sha')"

    command -v makepkg > /dev/null \
        || die "makepkg not found — run from the dev shell"
    ( cd packaging/aur && makepkg --printsrcinfo > .SRCINFO )

    git --no-pager diff --stat -- packaging/aur/
    ;;
esac
