# CI/CD Hardware Acceleration Guide

Minutes is a Mac-only app (Apple Silicon, macOS 13+; transcription needs macOS 15+). Every CI build targets `aarch64-apple-darwin`.

## What CI Builds

| Component | Acceleration | How it is enabled |
| --- | --- | --- |
| speech-helper sidecar (FluidAudio) | Core ML (Neural Engine, GPU, CPU) | Always on: the Swift package runs its models through Core ML, no build flag |
| llama-helper sidecar | Metal GPU | `cargo build --release -p llama-helper --features metal` |

No extra Cargo feature is passed to the Tauri build.

## Workflows

- `build.yml` (shared release workflow): builds the speech-helper sidecar (`speech-helper/build.sh release`) and the llama-helper sidecar with `--features metal`, then the Tauri app for `aarch64-apple-darwin`.
- `build-devtest.yml`: the same build for unsigned or signed DevTest artifacts.
- `pr-check.yml`: runs `scripts/pr-check.sh` on an Apple Silicon runner (`macos-latest`).

## Related Documentation

- [Building from Source](../../docs/BUILDING.md)
- [Workflow Overview](WORKFLOWS_OVERVIEW.md)
- [DevTest Workflow Guide](README_DEVTEST.md)
- [FluidAudio](https://github.com/FluidInference/FluidAudio)
