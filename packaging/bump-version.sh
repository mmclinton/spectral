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

# Replace the single line matching an ERE, by line number so the pattern
# never reaches awk. The replacement travels in the environment rather than
# through `-v`, which does apply escape-sequence processing: the man page's
# replacement is roff, where a backslash is the escape character, so `-v`
# would silently eat the `\-` in the .TH manual title (and a `\n` would
# split the line outright).
#
# Zero matches means the file's shape drifted; more than one means the
# pattern is no longer unique. Both are errors, not situations to guess in.
replace_line() {
    local file=$1 pattern=$2 replacement=$3 hits
    hits=$(grep -n -E "$pattern" "$file" | cut -d: -f1) || true
    [[ -n "$hits" && $(echo "$hits" | wc -l) -eq 1 ]] \
        || die "$file: expected exactly one line matching '$pattern'"
    rep="$replacement" awk -v n="$hits" \
        'NR == n { print ENVIRON["rep"]; next } { print }' "$file" > "$file.tmp" \
        && mv "$file.tmp" "$file"
}

# Everything this script only reads is checked here, before the first edit,
# so a drifted file fails the run on an untouched tree rather than leaving
# a half-bumped one behind for the operator to unpick.

# The metainfo release entry is authored prose and it ships inside the
# tarball, so it is verified, never written.
grep -q "<release version=\"$version\" date=\"" "$metainfo" \
    || die "$metainfo has no <release> entry for $version — author it first (it ships in the tarball)"

# The .TH line's other field is the page's own revision date, which is not
# the release date, so it is carried across rather than rewritten. Captured
# now because BASH_REMATCH belongs to whichever match ran last.
th=$(grep -E '^\.TH SPECTRAL 1 ' "$man_page") \
    || die "$man_page: expected a .TH line"
[[ "$th" =~ ^(\.TH\ SPECTRAL\ 1\ \"[0-9]{4}-[0-9]{2}-[0-9]{2}\"\ \")spectral\ [0-9]+\.[0-9]+\.[0-9]+(\".*)$ ]] \
    || die "$man_page: the .TH line no longer carries a version where this script expects one"
th_prefix=${BASH_REMATCH[1]}
th_suffix=${BASH_REMATCH[2]}

replace_line Cargo.toml \
    '^version = "[0-9]+\.[0-9]+\.[0-9]+"$' \
    "version = \"$version\""
replace_line nix/package.nix \
    '^  version = "[0-9]+\.[0-9]+\.[0-9]+";$' \
    "  version = \"$version\";"
replace_line "$man_page" '^\.TH SPECTRAL 1 ' \
    "${th_prefix}spectral $version${th_suffix}"

# Any workspace resolution rewrites the lockfile's own stanza; offline
# so nothing else can move as a side effect.
cargo metadata --format-version 1 --offline > /dev/null
grep -A 1 '^name = "spectral"$' Cargo.lock | grep -q "\"$version\"" \
    || die "Cargo.lock did not pick up $version"

git --no-pager diff --stat -- Cargo.toml Cargo.lock nix/package.nix "$man_page"
