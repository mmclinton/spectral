#!/usr/bin/env bash
# Regenerate cargo-sources.json from Cargo.lock.
#
# Flathub builds with no network, so every crate has to be listed as an
# explicit source with a checksum. Run this whenever Cargo.lock changes and
# commit the result alongside it.
#
#     ./packaging/flatpak/generate-sources.sh
#
# Everything it needs is fetched on demand and nothing is left behind, so the
# dev shell carries no packaging tooling for a target it never builds.

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

# The generator declares its own dependencies inline (PEP 723), so uv builds
# the environment from the script itself — no Python packages to pin here or
# in the flake. --python is not optional: left to itself uv fetches a
# generic-linux CPython, which NixOS cannot execute.
nix shell nixpkgs#uv nixpkgs#python3 --command sh -c '
    exec uv run --script --python "$(command -v python3)" "$@"
' -- "${scratch}/flatpak-cargo-generator.py" \
    "${repo_root}/Cargo.lock" \
    -o "${here}/cargo-sources.json"

echo "wrote ${here}/cargo-sources.json"
