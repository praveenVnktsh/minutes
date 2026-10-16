# Live summary timeline: implementation plan

Spec: `docs/superpowers/specs/2026-10-16-live-summary-timeline-design.md`.
Branch: `enhance/live-summary-timeline`, cut from `enhance/live-offline-transcription`.

Each step is test-first where the logic is pure. Run `cargo test -p meetily live_summary`
after each Rust step.

## 1. Persistence

1. Migration `frontend/src-tauri/migrations/20261016000000_add_meeting_timeline_entries.sql`.
2. `database/repositories/timeline.rs` with `TimelineEntryRow` and `TimelineRepository`:
   - `insert_entries` (INSERT OR IGNORE)
   - `list_for_meeting` (ordered by `start_ms`)
   - `delete_for_meeting`
3. Tests run against an in-memory pool with the real `sqlx::migrate!` set:
   - round-trip and ordering
   - an idempotent re-insert
   - an insert for a missing meeting is rejected (no resurrection)
   - deleting the meeting through `MeetingsRepository::delete_meeting` removes its entries
4. Add an explicit `DELETE FROM meeting_timeline_entries` to `delete_meeting_with_transaction`.

## 2. Pure logic in `live_summary/`

1. `scheduler.rs`. Tests first, then `WindowScheduler`:
   - not due before `CADENCE`
   - due after it with at least `MIN_WORDS`
   - silence produces no window
   - short text carries over, and stale short text is dropped after `MAX_IDLE_CARRY`
   - in flight means no window, and segments keep merging
   - a merged window raises the entry budget
   - the coalesce cap drops the oldest segments
   - a flush at stop ignores the cadence but not `MIN_WORDS`
2. `prompt.rs`. Tests: the speakers are explained, previous entries are included and capped
   at 3, lines are formatted as `[mm:ss] You:`, and the entry budget appears in the prompt.
3. `parse.rs`. Tests:
   - plain JSON, a fenced block, `<think>` before the JSON, and a bare array
   - truncated JSON that is salvaged
   - garbage that returns an error
   - kind synonyms, time formats, clamping into the window, and `end` before `start`
   - duplicates of previous entries are dropped, and the count is capped

## 3. Model call

1. `summary/service.rs`: make `LlmConfig` and `load_llm_config` `pub(crate)` (merge point).
2. `summary/summary_engine/client.rs`: add `BuiltinGenerateOptions` and
   `generate_with_builtin_options`. `generate_with_builtin` delegates to it with the old
   defaults (merge point).
3. `live_summary/llm.rs`: `resolve_model(pool)` returns `TimelineModel` or `Unavailable`.
   `generate(app, model, system, user, token)` adds a timeout.

## 4. Worker and lifecycle

1. `live_summary/worker.rs`: the loop over a segment receiver, a 2 s tick, the finish signal
   and the in-flight result. Generation goes through a `Generator` trait object, so the
   worker can be tested with a fake one:
   - one request in flight at a time
   - finishing flushes the tail and cancels a request that hangs past the grace period
   - a failure sets the status to failed and the next window recovers
2. `live_summary/mod.rs`:
   - the `SESSION` static
   - `start_session` and `finish_session`
   - the snapshot
   - the settings (`liveSummaryEnabled`)
   - the commands `get_live_timeline`, `list_meeting_timeline_entries`,
     `get_live_summary_settings` and `set_live_summary_settings`
3. Hook `start_session` into both `start_recording*` paths after `finalize_recording_start`.
   Hook `finish_session` into `stop_recording_session` after the live worker drains.
4. Register the module in `lib.rs` and the commands in `generate_handler!`.

## 5. Frontend

1. `src/lib/liveTimeline.ts`: types, the API, `formatTimelineTime`, `kindLabel`,
   `upsertSnapshot`, and the event names. Unit tests.
2. `src/components/MeetingDetails/LiveSummaryPanel.tsx`: shown in `LiveTranscriptPanel`. It
   subscribes to `live-timeline-update` and hydrates from `get_live_timeline`.
3. `src/components/MeetingDetails/TimelineChapters.tsx`: shown in `TranscriptPanel` when
   entries exist. A click dispatches `meetily:jump-to-time`.
4. `VirtualizedTranscriptView`: handle `meetily:jump-to-time` with `nearestSegmentIndex`.
5. `src/components/LiveSummarySettings.tsx`: shown in Settings → Summary.
6. Component tests for the chapters click and the failure notice.

## 6. Verify

Run, from `frontend/`: `pnpm typecheck`, `pnpm lint` and `pnpm test`. Then run
`cargo check -p meetily` and `cargo test -p meetily`.

## 7. Smoke test (if the models are already on disk)

An `#[ignore]` test, `live_summary::smoke`, feeds a sample transcript through the worker with
the built-in model and prints the latency per window and the entries. Run it with
`cargo test -p meetily live_summary_smoke -- --ignored --nocapture`.
