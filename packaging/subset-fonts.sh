#!/usr/bin/env bash
# Regenerate the UI font files under ui/fonts/ from upstream Iosevka.
#
# The control panel embeds Iosevka Aile so every install renders the
# same face, but a stock Iosevka file is about 6 MB because it covers
# thousands of glyphs the interface never draws. This script cuts each face
# down to the blocks the UI actually uses (Latin plus the punctuation,
# arrow, math, box-drawing, and geometric-shape blocks the .slint text
# reaches for), which lands under 100 KB per face. Text outside those
# blocks, such as a device name in another script, is drawn by whatever
# system font can supply the glyph.
#
# The unhinted upstream package is the source: Slint's renderer does not
# execute TrueType hinting, so the instructions would be dead weight. Every
# layout feature is stripped as well: the interface uses none of Iosevka's
# ligatures or character variants.
#
# Rerun after bumping VERSION below, then commit ui/fonts/ and update the
# Iosevka entry in THIRD-PARTY.md.
#
#     nix shell nixpkgs#python3Packages.fonttools -c ./packaging/subset-fonts.sh
#
# Run from anywhere inside the repository. Needs pyftsubset (fonttools),
# curl, unzip, and sha256sum on PATH.

set -euo pipefail

die() { echo "subset-fonts: $*" >&2; exit 1; }

VERSION=34.3.0
ARCHIVE="PkgTTF-Unhinted-IosevkaAile-$VERSION.zip"
URL="https://github.com/be5invis/Iosevka/releases/download/v$VERSION/$ARCHIVE"
SHA256=258f16de26cfeb539daebf0ecc374dd90cf5eea9afe3163dafc40cd551315dbc

# Regular, Bold, and Italic are the only faces the interface asks for. The
# two intermediate weights it names once each fall to the nearest of these.
FACES=(Regular Bold Italic)

# Basic Latin and Latin-1, Latin Extended-A, General Punctuation, Arrows,
# Mathematical Operators, the diameter sign, Box Drawing, Geometric Shapes.
UNICODES="U+0000-017F,U+2000-206F,U+2190-21FF,U+2200-22FF,U+2300,U+2500-257F,U+25A0-25FF"

for tool in pyftsubset curl unzip sha256sum; do
    command -v "$tool" >/dev/null || die "$tool is not on PATH"
done

cd "$(git rev-parse --show-toplevel)"
out=ui/fonts
mkdir -p "$out"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "fetching $URL"
curl -sSfL -o "$work/$ARCHIVE" "$URL"
echo "$SHA256  $work/$ARCHIVE" | sha256sum -c --quiet \
    || die "checksum mismatch for $ARCHIVE; update SHA256 only after verifying the release"

for face in "${FACES[@]}"; do
    src="IosevkaAile-$face.ttf"
    unzip -q -o "$work/$ARCHIVE" "$src" -d "$work"
    pyftsubset "$work/$src" \
        --unicodes="$UNICODES" \
        --layout-features='' \
        --no-hinting \
        --output-file="$out/$src"
    printf '%s: %d KB\n' "$out/$src" "$(( $(stat -c %s "$out/$src") / 1024 ))"
done
