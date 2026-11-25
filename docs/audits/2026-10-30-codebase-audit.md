# Codebase audit: clutter, weird pathways, dead code (2026-10-30)

Four read-only passes: core UI/UX, settings and onboarding, frontend dead code, Rust dead code. Paths under `frontend/src/` unless they start with `src-tauri/`. Effort S/M/L.

Follows up [2026-09-18-app-ux-audit.md](2026-09-18-app-ux-audit.md). Since then, UX-01, 02, 04–07, 12–15, 17–19 and the original UX-21 list were fixed. UX-03 and UX-08 are partly fixed. UX-10 (transcript search covers only loaded rows) is still open. Most new problems come from live notes, Copilot, pause/resume and meeting detection being added on top of the existing screens rather than replacing anything.

## 1. Core UI

| # | Finding | Fix | Effort |
|---|---|---|---|
| U1 | The live recording screen shows recording state five times: the `RecordingStatusBar` inside `VirtualizedTranscriptView.tsx:634` (with a "Return to recording" button that points at the current page), the LIVE pill, the floating controls, the sidebar's pulsing button, and the sidebar row badge | Remove `RecordingStatusBar` from the transcript view. On the live workspace, make the sidebar button a non-clickable "Recording" label | S |
| U2 | Toasts sit on the stop/pause controls. Floating controls, `RecordingControllerFeedback` and the sonner toaster all anchor bottom-center. The consent toast forces bottom-center for 10s | Move the toaster to top-right. Make the consent notice inline. Re-anchor the controller feedback | S |
| U3 | Stopping shows progress in 3–5 places (sidebar, pill, `ShellActivitySurface`, header banner, transcript panel). `showFinalizing` isn't gated by `isOnLiveWorkspace` | One progress surface per page (the header banner on the meeting page). Gate `showFinalizing` | M |
| U4 | On home, the sidebar duplicates `MeetingsLibrary`: its own New, Import, search (sharing one query) and full list, under a heading that says "Recent" | Make the sidebar a thin rail: New, Meetings, ~5 recent, Settings | M |
| U5 | When import is turned off, "Enable audio import" is advertised in the sidebar, the library and the palette | Hide it, or take import out of Beta (S4) | S |
| U6 | Naming soup: AI notes / summary / enhanced notes / Meeting summary / chat / AI chat / meeting assistant / Copilot. "Enhance transcript" actually re-transcribes | Use "Notes" and "AI notes". Rename the button to "Re-transcribe". Pick one name for the assistant | S |
| U7 | You can't regenerate AI notes on any meeting that has live notes (`page-content.tsx:301`). "Show AI notes" is a global localStorage switch that also silently drops AI lines from Copy and Export | Show "Regenerate AI notes" whenever a transcript exists. Make Copy/Export say what they include | M |
| U8 | "Resume meeting" lives only in the transcript dock toolbar, which narrow windows hide. "Resume" also means un-pause | Rename to "Continue recording" and put it in the meeting header | S |
| U9 | The transcript panel header stacks 6 buttons, a ⋯ menu that repeats Speakers, search, a scrubber and 4 status lines | Keep Search and Speakers as buttons; put the rest in ⋯. Merge the status lines | M |
| U10 | The chat's "Choose model" edits the global AI-notes model | Give chat its own setting, or label the picker as also changing the notes model | S |
| U11 | Confidence dots are hard-coded on. Their toggle lived only in the unmounted `SettingsModal` | Wire the dots to the setting, or remove them | S |
| U12 | The palette searches only the 8 most recent meetings. "⌘ / Ctrl + K" is stale. `focus-sidebar-search` has no sender. The theme toggle appears in 3 places. "Regenerate title" is offered mid-recording | Small fixes | S |
| U13 | Import and recovery call `router.push` directly, skipping the shell's save-before-navigate | Use `navigate` | S |
| U14 | The device-picker level meter is fake: `src-tauri/src/audio/simple_level_monitor.rs:43` emits a sine wave. The real `level_monitor.rs` is unused | Wire the real monitor back in, or remove the meter | M |

## 2. Settings and onboarding

Proposed target (7 tabs → 6): **General** (notifications, shortcuts, storage, updates) · **Recording** (save audio, detection and ignore list, devices, setup check; capture backend under Advanced) · **Transcription** (language, final pass, vocabulary; engines under Advanced) · **AI notes** (live toggle, end-of-meeting toggle, language, one model area, Copilot toggle; prompts under Advanced) · **Integrations** · **Labs & Developer**.

| # | Finding | Fix | Effort |
|---|---|---|---|
| S1 | Transcript language can't be set anywhere. Its only UI is in the unmounted `SettingsModal`, but `RecordingControllerContext.tsx:845` still reads it | Delete `SettingsModal`. Add a language select to Transcription | S |
| S2 | Onboarding downloads Parakeet (~670 MB). Then `migrate_final_model` switches to Whisper turbo and quietly pulls ~1.2 GB more | Download Nemotron and Whisper turbo directly, with honest sizes | M |
| S3 | A custom prompt or a non-English language silently sends notes down the old whole-transcript path (`summary/live_enhanced.rs:258`), and the UI doesn't say so | Put prompts under Advanced with a warning; same note on the language pin | S |
| S4 | Onboarding treats Qwen 4B as the required "Summarization Engine" and the fine-tuned E2B as optional, which is backwards on Macs with 16 GB or more | Make E2B required and 4B optional | M |
| S5 | E2B has two download controls on the Summary tab (`BuiltInModelManager` and `LiveNotesModelSettings`) | One "Notes model" area | S |
| S6 | The provider picker exists in 4 places (Summary tab, the chat's `ModelSettingsModal`, the dead modal, Copilot's own free-text field) | One picker. Chat deep-links to it. Copilot's override goes to Developer | M |
| S7 | The Summary tab has 8 cards and 4 names for one feature. "Automatic summaries" actually controls whether final notes are written | Rename to "AI notes" and restructure as above | S |
| S8 | The live-notes copy promises action items, which #117/#118 removed | Reword | S |
| S9 | Summary-language "recents" feed a quick-switch that doesn't exist | Replace with one select | S |
| S10 | Beta holds shipped default-on features (Import, the live transcription toggle) and Debug mode. The Copilot debug line is in the Summary tab | Promote the shipped features. Move debug controls to Developer | S |
| S11 | Unreachable Deepgram/ElevenLabs/Groq/OpenAI transcription UI and settings in `TranscriptSettings.tsx` and `setting.rs`, with no Rust implementation | Delete | S |
| S12 | Two "Open folder" buttons and two notification toggles across General and Recording. The filename-format note is wrong (it's `<meeting>/audio.mp4`) and `file_format` is display-only | Dedupe; fix or remove the note | S |
| S13 | Jargon: ScreenCaptureKit vs Core Audio, Neural Engine/GPU/CPU, Nemotron/Qwen/Gemma quant names | Put it behind Advanced | S |
| S14 | Onboarding's progress bar says `totalSteps={4}` on one step and 5 on the others. Permissions come after a multi-GB download starts. The dead `screenRecording` field and the Meetily Homebrew DB probe (`OnboardingContext.tsx:249`) remain | Fix the count. Ask for permissions first. Remove the dead field and the probe | S |
| S15 | `About.tsx` copy says "Use any model / Works everywhere". `ModelConfigForm` still offers gpt-4/gpt-4-turbo | Moot once About is deleted (F1). Refresh the model list | S |

## 3. Frontend dead code (~3,500 LOC)

- **F1 unused files (~2,260):** `app/_components/SettingsModal.tsx`, `LanguageSelection.tsx`, `Info.tsx`/`Logo.tsx`/`About.tsx`, `ConsoleToggle.tsx`, `EditableTitle.tsx`, `BlockNoteEditor/`, `molecules/form-components/`, `ui/{form,button-group,input-group,sheet,progress,separator}.tsx`, `app/metadata.ts` and `metadata.tsx`, the empty `config/api.ts`, `RecordingPostProcessingProvider.tsx`, `hooks/useRecording{Start,Stop,StateSync}.ts`, `usePermissionCheck.ts`, `meeting-details/useModelConfiguration.ts`, `useModalState.ts` and its test, `lib/summaryCitations.ts` and its test, `TranscriptRecovery/index.ts`, `frontend/API.md`. Also the `StatusOverlays` export, `LiveNotesPad`'s non-bare branch, and dead props on `RecordingControls`.
- **F2 analytics is a no-op (~900 TS):** Rust hard-codes `enabled: false` with an empty key, and the frontend opts out at boot. Delete `lib/analytics.ts`, `AnalyticsProvider.tsx` and the 44 call sites.
- **F3 broken wiring:** `builtin_ai_get_models_directory` is invoked but never registered. `chunk-drop-warning`, `transcription-complete` and `transcript-error` are listened for but never emitted. `transcriptServerAddress` in SidebarProvider defaults to the old whisper-server URL and nothing reads it.
- **F4 ~300 lines of dead methods/exports** across `lib/whisper.ts`, `lib/parakeet.ts`, `lib/builtin-ai.ts`, `recordingService`, `transcriptService`, `configService` and others.
- **F5 duplicates to merge:** Parakeet download events are handled in 4 places, even though `useModelDownload` exists. There are three near-identical model managers. `debugMode` and `developerSettings` use the same backend-flag pattern twice. There are 5 byte formatters and about 9 mm:ss formatters.
- **F6 unused deps:** `@heroicons/react`, `@hookform/resolvers`, `@radix-ui/react-accordion`, all 8 `@remirror/*`, `@tiptap/react`, `@tiptap/starter-kit`, `lodash`, `@types/lodash`, `concurrently`, `wait-on`, then `react-hook-form` after F1. `@tauri-apps/plugin-fs` and `@tauri-apps/plugin-notification` are medium confidence.
- **F7 hygiene:** 147 `console.log` calls in production code. A hex colour in `MeetingAssistantPanel.tsx:199` breaks dark mode.

## 4. Rust dead code (~7,500 high-confidence LOC + ~1,900 to feature-gate)

- **R1 not compiled at all (~2,830):** `audio/recording_commands.rs.backup`, `audio/recording_saver_old.rs`, `audio/system_audio_stream.rs`, `whisper_engine/_stderr_suppressor.rs` (the last Windows/Linux cfgs), and `audio/system_audio_types.ts`.
- **R2 analytics (~1,120):** `src-tauri/src/analytics/`, 25 commands, and the `posthog-rs` crate.
- **R3 Whisper parallel processing (~1,120):** `parallel_commands.rs`, `parallel_processor.rs` and `system_monitor.rs`. None of the 11 commands is called.
- **R4 dead audio modules (~2,040):** `level_monitor` and `simple_level_monitor` (see U14), `ffmpeg_mixer.rs` (RNNoise is hard-off, so `nnnoiseless` is dead too), `diagnostics.rs`, `post_processor.rs`, `async_logger.rs`, dead functions in `audio_processing.rs`, `AdaptiveWhisperConfig`, `BatchProcessor`, and `TranscriptionEngine::Provider` with its two providers.
- **R5 56 registered commands never invoked:** the notification test and DND family, the system-audio family, `start_recording_with_devices`, the status queries, `api_delete_meeting`, `api_validate_template`, and the legacy-database pair.
- **R6 feature-gate:** `replay/report.rs`, `script.rs` and `events.rs` (~1,880) ship in release, but only the `dev-replay` runner uses them.
- **R7 medium confidence:** the live-notes "compact" prompt mode now only runs while the E2B adapter is still downloading (~400). The Meetily Homebrew DB import (~150). Two ~245-line near-duplicate `start_recording_*` functions, plus another pair in `lib.rs` (~290 saved by merging).
- **R8 deps:** `clap`, `chrono-tz`, `core-graphics`, `dashmap`, `dasp`, `rand`, and `esaxx-rs` with its MSVC `[patch]` (a C++ build) are unused. `nnnoiseless` and `posthog-rs` are used only by dead code. Unused dev-deps: `infer`, `criterion`, `memory-stats`, `strsim`, `futures`, `tracing-subscriber`.
- **Keep:** the `processor.rs` whole-transcript fallback (custom prompt, non-English, imports, cloud providers), `live_outline`, `tidy`, the JSON prompt mode (8 GB Macs), Parakeet (opt-in), and chunked-Whisper live fallback.

## 5. Repo cruft

- ~33 MB of unreferenced images in `docs/`, including `meetily-export.gif` (15 MB), `meetily_demo.gif` (8.6 MB), `1–7.png`, `pv2.*`, `editor*.png`, `logo1–3.png` and `HighLevel.jpg`. Only `docs/8.png` is referenced.
- A tracked log file from 2024: `src-tauri/logs/meeting-minutes.log.2024-12-30`.
- Stale notes: `src-tauri/LOGGING_OPTIMIZATIONS.md`, `src-tauri/NOTIFICATION_TESTING.md` (documents commands that R5 deletes), and `scripts/inject_transcript.py` ("Meetily SQLite database").
- `package.json` scripts: `export` (`next export` was removed in Next 14), and `tauri:{dev,build}:{metal,coreml}`, which are on by default.
- Typo in a bundled template: `templates/psychatric_session.json`.
- CLAUDE.md is out of date: it still calls the app "Meetily", gives `fix/audio-mixing` as the current branch, mentions BlackHole and points to `parallel_processor.rs`.
