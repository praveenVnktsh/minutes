#!/usr/bin/env bash
# Verify this repository: the one sequence every pull request must pass.
#
#   scripts/pr-check.sh
#
# PR Check (.github/workflows/pr-check.yml) runs exactly this, and so does
# foreman's board.toml as its [test] command. One copy, because two would drift:
# a build agent that passes a local sequence CI does not run would open pull
# requests that go red, and every red one costs the card an attempt.
#
# Needs pnpm, bun and a Rust toolchain on PATH, plus the Tauri system libraries
# (see CONTRIBUTING.md). Runs CPU-only.
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

step() { printf '\n==> %s\n' "$*"; }

# --- frontend ------------------------------------------------------------------
cd frontend
step "install frontend dependencies"
pnpm install --frozen-lockfile
step "typecheck"
pnpm typecheck
step "lint"
pnpm lint
# Runs each test file in its own process; see scripts/test-each.mjs for why.
step "frontend tests"
pnpm test
# Also produces frontend/out, which the Rust app embeds at compile time
# (tauri.conf.json's frontendDist), so it must come before the Rust build.
step "build frontend"
pnpm build
cd "$root"

# --- rust ----------------------------------------------------------------------
# tauri.conf.json declares llama-helper as an externalBin, and the Tauri build
# script refuses to compile the app until a binary sits at
# binaries/llama-helper-<host triple>. Tests only need it to exist, so a debug
# build is enough.
step "stage the llama-helper sidecar"
cargo build -p llama-helper
target="$(rustc -vV | awk '/^host:/ {print $2}')"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
mkdir -p frontend/src-tauri/binaries
cp "$target_dir/debug/llama-helper" "frontend/src-tauri/binaries/llama-helper-${target}"

step "rust tests"
cargo test -p meetily

printf '\nPR Check passed.\n'
