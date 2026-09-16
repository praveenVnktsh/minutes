# Changelog

All notable changes to Minutes are documented here. This project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each GitHub release publishes the section below that matches its version.

## [Unreleased]

## [1.5.3] - 2026-09-16

### Added

- **Choose the speaker count when identifying speakers** — the transcript "Identify speakers" action now offers Auto-detect or a fixed 2–6 speakers, so meetings with a known number of participants get accurate labels instead of relying on the automatic clustering estimate.

## [1.5.2] - 2026-09-16

### Changed

- **Sharper local speaker diarization** (macOS) — switches to fp32 segmentation and a stronger VoxCeleb ResNet34 speaker embedding in place of the previous int8 segmentation and NeMo titanet-small embedding, for better speaker separation.
- Clustering sensitivity can now be tuned with the `MEETILY_DIARIZATION_THRESHOLD` environment variable.

## [1.5.1] - 2026-09-16

### Added

- **Check for updates** in Settings → General: shows the current version and lets you check for and install updates.

## [1.5.0] - 2026-09-16

### Added

- **Microphone-based meeting detection** — prompts only when an app is actually capturing your mic, so music/video/system audio no longer triggers it.
- **Compact, app-themed meeting-detected toast** (light/dark aware).
- **Configurable global shortcuts** (Settings → General) — record a new combination or disable either shortcut.
- **Discard short recordings** (Settings → Recordings) — meetings shorter than a configurable threshold (default 10s) with no transcript or notes are deleted on stop, including their audio folder.

## [1.4.0] - 2026-09-15

### Added

- **Debug mode** (Settings → Beta): record test meetings into an isolated bucket so they never pollute real recordings, with a diagnostics panel, one-click **Delete debug meetings**, verbose logging, and a quick **Debug on** button in the sidebar.

## [1.3.0] - 2026-09-15

### Added

- **Live transcription toggle during a meeting** (enabling starts the worker mid-session).
- **Bounded live segments** — long uninterrupted speech is force-flushed at 15s instead of waiting for a pause.
- **Transcript playback** — click a timestamp to play from there, with the active line highlighted.
- **Transcript search navigation** — highlighted matches with prev/next and an `n/m` counter.
- **Rust-driven meeting detection** — tray badge and prompt work while Minutes is in the background; broader app coverage (WhatsApp/Telegram/Signal) plus a generic fallback.
- **Deep links** (`minutes://meeting/<id>`) and **Copy meeting link**.
- **Speaker tools** — sample quotes per speaker, click-to-edit labels that rename every line, and automatic detection of you from the microphone channel.
- **Small-window layout** — everything but your notes collapses; the transcript/chat dock is a side rail.
- **Full transcript loading** — no lazy paging.

### Fixed

- Enhanced notes are anchored on your notes; the workspace no longer hangs on "Generating summary…".
- macOS-only dock badge gated so Windows builds compile.

## [1.2.0] - 2026-09-14

### Added

- **Speaker naming** — sample quotes per speaker and click-to-edit transcript labels that rename every segment without jumping the scroll.
- **Compact layout** — small frames collapse everything except notes; the notes/transcript divider is draggable and clamped so notes stay readable.
- **Full transcript scroll** — the whole transcript loads up front.

## [1.1.1] - 2026-09-13

### Fixed

- Summary generation could hang on "Generating summary…" until a reload; the workspace now polls and adopts the completed summary.
- Enhanced notes ignored the user's own notes; the summarizer now treats them as the outline.

## [1.1.0] - 2026-09-13

### Added

- **Tray widgets** — live recording timer, recent meetings submenu, and window/dock recording indicators.
- **Global shortcuts** (toggle recording / show or hide) and a **command palette** (`Cmd/Ctrl+K`).
- **Pin/favorite** and **archive** meetings.
- **Export a meeting** to Markdown.
- **Custom transcription vocabulary**.
- Quality-of-life pass: working ESLint + test/typecheck scripts, production console stripping, dead-code removal, Meetily→Minutes rebrand, warning-free Rust build.

## [1.0.0] - 2026-09-12

### Added

- First public Minutes release: local-first meeting recording, live/after-meeting transcription, a notes-first meeting workspace with raw and enhanced notes, meeting chat, local and provider summaries, speaker diarization, meeting detection, folders, search, audio import, and signed updates.
