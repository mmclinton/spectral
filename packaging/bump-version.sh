#!/usr/bin/env bash
# Bump every in-repo version spot for a release.
#
# The release version lives in five files that must move together; this
# script is the authority on that list. It edits the mechanical spots and
# verifies the authored one (the metainfo <release> entry), so a release
# cut cannot silently miss a spot. Any file that no longer matches the
# shape this script expects fails the run loudly — that is drift, and the
# fix is to update this script alongside the file that changed.
#
#     ./packaging/bump-version.sh X.Y.Z
#
# Run from anywhere inside the repository. The dev shell provides cargo.

set -euo pipefail

die() { echo "bump-version: $*" >&2; exit 1; }

version="${1:-}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "usage: bump-version.sh X.Y.Z"

cd "$(git rev-parse --show-toplevel)"

metainfo=assets/page.codeberg.mmc.Spectral.metainfo.xml
man_page=assets/spectral.1

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

# The .TH line's other field is the page's own revision date, which is not
# the release date, so it is carried across rather than rewritten.
th=$(grep -E '^\.TH SPECTRAL 1 ' "$man_page") \
    || die "$man_page: expected a .TH line"
[[ "$th" =~ ^(\.TH\ SPECTRAL\ 1\ \"[0-9]{4}-[0-9]{2}-[0-9]{2}\"\ \")spectral\ [0-9]+\.[0-9]+\.[0-9]+(\".*)$ ]] \
    || die "$man_page: the .TH line no longer carries a version where this script expects one"
replace_line "$man_page" '^\.TH SPECTRAL 1 ' \
    "${BASH_REMATCH[1]}spectral $version${BASH_REMATCH[2]}"

# Any workspace resolution rewrites the lockfile's own stanza; offline
# so nothing else can move as a side effect.
cargo metadata --format-version 1 --offline > /dev/null
grep -A 1 '^name = "spectral"$' Cargo.lock | grep -q "\"$version\"" \
    || die "Cargo.lock did not pick up $version"

git --no-pager diff --stat -- Cargo.toml Cargo.lock nix/package.nix "$man_page"
