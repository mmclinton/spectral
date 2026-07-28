#!/usr/bin/env bash
# Regenerate cargo-sources.json from Cargo.lock.
#
# Flathub builds with no network, so every crate has to be listed as an
# explicit source with a checksum. Run this whenever Cargo.lock changes and
# commit the result alongside it.
#
# Needs the dev shell for its Python dependencies: nix develop -c ./generate-sources.sh

set -euo pipefail

# Pinned rather than tracking master: an unannounced change in the generator
# would silently reshape a file we commit and ship.
GENERATOR_COMMIT=f03a673abe6ce189cea1c2857e2b44af2dd79d1f
GENERATOR_URL="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/${GENERATOR_COMMIT}/cargo/flatpak-cargo-generator.py"

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "${here}/../.." && pwd)

scratch=$(mktemp -d)
trap 'rm -rf "${scratch}"' EXIT

curl -sSfL -o "${scratch}/flatpak-cargo-generator.py" "${GENERATOR_URL}"

python3 "${scratch}/flatpak-cargo-generator.py" \
    "${repo_root}/Cargo.lock" \
    -o "${here}/cargo-sources.json"

echo "wrote ${here}/cargo-sources.json"
