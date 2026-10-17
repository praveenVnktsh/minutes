#!/usr/bin/env bash
# Build nemotron-helper (Swift, Core ML) and stage it where Tauri's externalBin
# expects it: frontend/src-tauri/binaries/nemotron-helper-<host triple>.
#
#   nemotron-helper/build.sh            # release build (what ships)
#   nemotron-helper/build.sh debug
set -euo pipefail

configuration="${1:-release}"
here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd -- "$here/.." && pwd)"
target="$(rustc -vV | awk '/^host:/ {print $2}')"

swift build --package-path "$here" -c "$configuration" --product nemotron-helper
binary="$(swift build --package-path "$here" -c "$configuration" --show-bin-path)/nemotron-helper"

mkdir -p "$root/frontend/src-tauri/binaries"
cp "$binary" "$root/frontend/src-tauri/binaries/nemotron-helper-${target}"
echo "Staged nemotron-helper at frontend/src-tauri/binaries/nemotron-helper-${target}"
