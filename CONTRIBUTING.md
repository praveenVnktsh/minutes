# Contributing to Minutes

Issues and pull requests are welcome at [github.com/praveenvnktsh/minutes](https://github.com/praveenvnktsh/minutes).

## Workflow

1. Create a focused branch from `main`.
2. Make the change and add tests where practical.
3. Run the relevant frontend and Rust checks.
4. Open a pull request describing behavior, verification, and user-visible impact.

```bash
git clone https://github.com/YOUR_USERNAME/minutes.git
cd minutes
git switch -c feature/short-description
```

Keep unrelated changes separate and never commit API keys, signing keys, recordings, transcripts, model binaries, or other private data.

## Verification

Every pull request to `main` runs **PR Check** (`.github/workflows/pr-check.yml`), and it must pass before the pull request can merge. Merges to `main` publish a canary build that auto-updates immediately, so this check is the gate in front of a release.

To run the same checks locally, from the repository root:

```bash
# Frontend. `pnpm build` also produces frontend/out, which the Rust app embeds
# at compile time, so it must run before the Rust tests.
cd frontend
pnpm install --frozen-lockfile
pnpm typecheck
pnpm lint
pnpm test
pnpm build
cd ..

# Rust. The Tauri build refuses to compile the app until the llama-helper
# sidecar exists at binaries/llama-helper-<host triple>.
cargo build -p llama-helper
TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
mkdir -p frontend/src-tauri/binaries
cp target/debug/llama-helper "frontend/src-tauri/binaries/llama-helper-${TARGET}"
cargo test -p meetily
```

CI runs these CPU-only on Ubuntu 22.04. On your own machine you can add a GPU feature, such as `cargo test -p meetily --features metal` on macOS. See [docs/BUILDING.md](docs/BUILDING.md) for platform setup.

## License

By contributing, you agree that your contribution will be licensed under the repository's MIT License.
