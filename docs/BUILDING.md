# Building Minutes from Source

Minutes is Mac-only: it builds and runs on Apple Silicon Macs with macOS 13 or later. Windows and Linux are not supported, and the Rust crate refuses to compile for any other target.

## 1. Install Dependencies

```bash
# Install Homebrew (if not already installed)
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Install required tools
brew install cmake node
npm install -g pnpm@9.15.9

# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Install Xcode and select it with `xcode-select`. The PR Check script also needs [Bun](https://bun.sh) on `PATH`.

## 2. Install Frontend Dependencies

```bash
cd frontend
pnpm install --frozen-lockfile
```

Frozen installation keeps the lockfile and installed dependency set aligned. When intentionally changing dependencies, update and commit `pnpm-lock.yaml`.

## 3. Build and Run

```bash
# Development mode (with hot reload)
pnpm tauri:dev

# Production build
pnpm tauri:build

# Local Apple Silicon app bundle with Metal, unsigned, no updater artifacts
pnpm tauri:build:local:mac    # runs scripts/build-local-macos.sh
```

Whisper is always built with Metal and CoreML on macOS, and the llama-helper sidecar is built with `--features metal`. See [GPU_ACCELERATION.md](GPU_ACCELERATION.md).

## Build Output Location

This is a Cargo workspace rooted at the repository, so build output lands in `target/` at the repository root, not under `frontend/src-tauri/`. The local build helper prints the bundle path when it finishes:

```
target/release/bundle/macos/minutes.app
```

## Verification

Run the same sequence PR Check runs, from the repository root:

```bash
bash scripts/pr-check.sh
```
