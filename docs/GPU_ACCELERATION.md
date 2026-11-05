# GPU Acceleration Guide

Minutes is Mac-only (Apple Silicon, macOS 13+; transcription needs macOS 15+), and transcription always runs on Apple's frameworks.

## Backends

*   **Core ML (speech-helper):** Speech recognition, voice activity detection and speaker diarization run in the `speech-helper` Swift sidecar through [FluidAudio](https://github.com/FluidInference/FluidAudio), which schedules its models on the Neural Engine, GPU and CPU. There is no build flag; the sidecar is built by `speech-helper/build.sh`.
*   **Metal (llama-helper):** The built-in summary models run in the llama-helper sidecar, built with `--features metal`. `frontend/scripts/build-local-macos.sh` and the CI workflows do this for you.

The Rust crate has no GPU Cargo features.
