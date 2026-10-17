# GPU Acceleration Guide

Minutes is Mac-only (Apple Silicon, macOS 13+), and transcription always runs on the GPU through Apple's frameworks.

## Backends

Minutes uses the `whisper-rs` library with two Apple backends:

*   **Metal:** GPU acceleration on Apple Silicon.
*   **Core ML:** An additional acceleration layer for Apple Silicon.

Both are enabled unconditionally: the macOS dependency section of `frontend/src-tauri/Cargo.toml` declares `whisper-rs` with the `metal` and `coreml` features, so a plain `pnpm tauri:dev` or `pnpm tauri:build` already uses them.

The llama-helper sidecar (built-in summary models) is built with `--features metal`. `scripts/build-local-macos.sh` and the CI workflows do this for you.

## Cargo Features

The `metal` and `coreml` Cargo features remain as explicit opt-ins for scripts such as `pnpm tauri:dev:metal` and `pnpm tauri:dev:coreml`. `openmp` is available for whisper.cpp's OpenMP CPU threading but is not used by default.
