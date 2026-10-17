# CI/CD Hardware Acceleration Guide

Minutes is a Mac-only app (Apple Silicon, macOS 13+). Every CI build targets `aarch64-apple-darwin`.

## What CI Builds

| Component | Acceleration | How it is enabled |
| --- | --- | --- |
| Whisper (whisper-rs) | Metal GPU, plus CoreML on Apple Silicon | Always on: `frontend/src-tauri/Cargo.toml` enables `whisper-rs` with `metal` and `coreml` for macOS |
| llama-helper sidecar | Metal GPU | `cargo build --release -p llama-helper --features metal` |

No extra Cargo feature is passed to the Tauri build: the macOS dependency section already enables Metal and CoreML.

## Workflows

- `build.yml` (shared release workflow): builds the llama-helper sidecar with `--features metal`, then the Tauri app for `aarch64-apple-darwin`.
- `build-devtest.yml`: the same build for unsigned or signed DevTest artifacts.
- `pr-check.yml`: runs `scripts/pr-check.sh` on an Apple Silicon runner (`macos-latest`).

## Cargo Features

```toml
metal = ["whisper-rs/metal"]
coreml = ["whisper-rs/coreml"]
openmp = ["whisper-rs/openmp"]
```

`metal` and `coreml` are explicit opt-ins kept for scripts such as `pnpm run tauri:dev:metal`; the macOS build already has both.

## Related Documentation

- [Building from Source](../../docs/BUILDING.md)
- [Workflow Overview](WORKFLOWS_OVERVIEW.md)
- [DevTest Workflow Guide](README_DEVTEST.md)
- [Whisper.cpp](https://github.com/ggerganov/whisper.cpp)
