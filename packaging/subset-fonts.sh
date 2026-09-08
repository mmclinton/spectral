#!/usr/bin/env bash
# Regenerate the UI font files under ui/fonts/ from upstream Iosevka.
#
# The control panel embeds Iosevka Aile so every install renders the
# same face, but a stock Iosevka file is about 6 MB because it covers
# thousands of glyphs the interface never draws. This script cuts each face
# down to the blocks the UI actually uses (Latin plus the punctuation,
# arrow, math, and geometric-shape blocks the .slint text reaches for),
# which lands under 100 KB per face. Text outside those
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
#     ./packaging/subset-fonts.sh
#
# Run from anywhere inside the repository.

set -euo pipefail

die() { echo "subset-fonts: $*" >&2; exit 1; }

# The subsetter and the unpacker exist for this script alone, so they are
# fetched on demand rather than carried in the dev shell for a file
# regenerated about once a year. Re-exec instead of wrapping each call:
# every step below has to run against the same environment, and one
# invocation is one evaluation.
if [[ -z "${SUBSET_FONTS_TOOLING:-}" ]]; then
    self=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/$(basename -- "${BASH_SOURCE[0]}")
    command -v nix >/dev/null || die "nix is not on PATH; it supplies pyftsubset and unzip"
    export SUBSET_FONTS_TOOLING=1
    exec nix shell \
        nixpkgs#python3Packages.fonttools \
        nixpkgs#unzip \
        nixpkgs#curl \
        nixpkgs#git \
        --command "$self" "$@"
fi

VERSION=34.3.0
ARCHIVE="PkgTTF-Unhinted-IosevkaAile-$VERSION.zip"
URL="https://github.com/be5invis/Iosevka/releases/download/v$VERSION/$ARCHIVE"
SHA256=258f16de26cfeb539daebf0ecc374dd90cf5eea9afe3163dafc40cd551315dbc

# Every weight and style the .slint files ask for. A request Slint cannot
# satisfy exactly is answered with the nearest face in the family and never
# with a synthesized one, so a face missing here is not a fallback but a
# silently wrong render: 500 arrives as Regular, 600 as Bold, and an italic
# link loses its weight entirely because style is matched before weight.
FACES=(Regular Bold Italic BoldItalic Medium SemiBold)

# Basic Latin and Latin-1, Latin Extended-A, General Punctuation, Arrows,
# Mathematical Operators, the diameter sign, Geometric Shapes.
UNICODES="U+0000-017F,U+2000-206F,U+2190-21FF,U+2200-22FF,U+2300,U+25A0-25FF"

for tool in pyftsubset curl unzip sha256sum git; do
    command -v "$tool" >/dev/null || die "$tool is not on PATH"
done

cd "$(git rev-parse --show-toplevel)"

# A font bump moves no lockfile, so the release checklist's THIRD-PARTY.md
# reconcile never fires for one. Checked here instead, before anything is
# written: a notice that names the wrong version is worse than none.
grep -q "^## Iosevka Aile $VERSION " THIRD-PARTY.md \
    || die "THIRD-PARTY.md does not name Iosevka Aile $VERSION; update its entry first"

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
    # The subsetter keeps name IDs 0-6 and drops the rest. 13 and 14 are the
    # OFL notice and the license URL: THIRD-PARTY.md carries both for the
    # binary, but a font file that leaves the tree on its own should still
    # say what it is licensed under. 16 is the typographic family, and it is
    # load-bearing here — Medium and Semibold spell name ID 1 "Iosevka Aile
    # Medium" and "Iosevka Aile Semibold", so only 16 puts them in the
    # family the interface names. 17 keeps the matching subfamily.
    pyftsubset "$work/$src" \
        --unicodes="$UNICODES" \
        --layout-features='' \
        --no-hinting \
        --name-IDs+=13,14,16,17 \
        --output-file="$out/$src"
    printf '%s: %d KB\n' "$out/$src" "$(( $(stat -c %s "$out/$src") / 1024 ))"
done
