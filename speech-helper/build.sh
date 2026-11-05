#!/usr/bin/env bash
# Build speech-helper (Swift, FluidAudio on Core ML) and stage it where
# Tauri's externalBin expects it:
# frontend/src-tauri/binaries/speech-helper-<host triple>.
#
#   speech-helper/build.sh            # release build (what ships)
#   speech-helper/build.sh debug
set -euo pipefail

configuration="${1:-release}"
here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd -- "$here/.." && pwd)"
target="$(rustc -vV | awk '/^host:/ {print $2}')"

swift build --package-path "$here" -c "$configuration" --product speech-helper
binary="$(swift build --package-path "$here" -c "$configuration" --show-bin-path)/speech-helper"

mkdir -p "$root/frontend/src-tauri/binaries"
cp "$binary" "$root/frontend/src-tauri/binaries/speech-helper-${target}"
echo "Staged speech-helper at frontend/src-tauri/binaries/speech-helper-${target}"
