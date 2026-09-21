# Contributing to Minutes

Issues and pull requests are welcome at [github.com/praveenvnktsh/minutes](https://github.com/praveenvnktsh/minutes).

## Feedback from the App

The desktop app's "Send feedback" dialog opens a prefilled GitHub issue on [github.com/praveenvnktsh/minutes/issues/new](https://github.com/praveenvnktsh/minutes/issues/new). Issues must stay enabled on the repository (Settings → General → Features → Issues, or `gh repo edit --enable-issues`). If Issues are ever disabled, the dialog copies the report to the clipboard and tells the user instead of opening a broken page.

## Workflow

1. Create a focused branch from `main`.
2. Make the change and add tests where practical.
3. Run `cargo fmt --all`. The repo uses rustfmt's defaults, so there is no `rustfmt.toml` and format-on-save in your editor produces the same result. PR Check does not enforce this yet, so it is on you.
4. Run the relevant frontend and Rust checks.
5. Open a pull request describing behavior, verification, and user-visible impact.

```bash
git clone https://github.com/YOUR_USERNAME/minutes.git
cd minutes
git switch -c feature/short-description
```

Run this once per clone so `git blame` skips the wholesale rustfmt reformatting and keeps naming the person who actually wrote a line:

```bash
git config blame.ignoreRevsFile .git-blame-ignore-revs
```

GitHub applies the same file automatically when it renders blame on the web.

Keep unrelated changes separate and never commit API keys, signing keys, recordings, transcripts, model binaries, or other private data.

## Verification

Every pull request to `main` runs **PR Check** (`.github/workflows/pr-check.yml`), and it must pass before the pull request can merge. Merges to `main` publish a canary build that auto-updates immediately, so this check is the gate in front of a release.

To run exactly what PR Check runs, from the repository root:

```bash
bash scripts/pr-check.sh
```

It typechecks, lints, tests and builds the frontend, then stages the `llama-helper` sidecar and runs the Rust tests. The order matters: the Rust app embeds the built frontend at compile time, and the Tauri build refuses to compile until the sidecar exists. foreman's `board.toml` uses the same script as its test command, so a change that passes it locally is a change CI will pass.

It needs `pnpm`, `bun` and a Rust toolchain on `PATH`. On Linux it also needs the Tauri system libraries:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libasound2-dev libopenblas-dev libx11-dev libxtst-dev libxrandr-dev
```

CI runs it CPU-only on Ubuntu 22.04. On your own machine you can add a GPU feature, such as `cargo test -p meetily --features metal` on macOS. See [docs/BUILDING.md](docs/BUILDING.md) for platform setup.

## License

By contributing, you agree that your contribution will be licensed under the repository's MIT License.
