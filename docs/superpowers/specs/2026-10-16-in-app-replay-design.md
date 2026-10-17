# In-app replay

Date: 2026-10-16. Status: approved, implemented on `enhance/in-app-replay` (stacked on #99,
`dev/replay-harness`).

## Goal

Play a pipeline replay (`replays/<run>/events.jsonl`, see `site/replay/README.md`) inside the
app, through the app's real recording workspace, so what a replay shows cannot drift from what
the app renders. Meeting data stays on this machine: runs are read from the app data folder
and nothing is uploaded.

## Non-goals

- Starting a new replay from inside the app (the `replay` example still records runs). Follow-up.
- Removing `site/replay`. It keeps working; its README points at the in-app viewer as primary.

## Turning it on

Settings → Developer → **Replays** (off by default, a per-device preference in
`localStorage`, `lib/developerSettings.ts`). When on, the sidebar shows a **Replays** item
under Meetings. It opens `/replays`.

## Runs list (`/replays`)

`replay_list_runs` (Rust, reusing `replay::serve`'s run index) lists the runs under
`<app data>/replays`, newest first: recording name, meeting length, status (done / running /
incomplete), report pass/fail counts and date. The empty state explains how to create a run
(`cargo run … --example replay -- --audio …`). Clicking a run opens `/replays?run=<name>`.

## The viewer (`/replays?run=<name>`)

The real recording workspace, as `meeting-details` renders it while recording:
`MeetingWorkspace` with `LiveTranscriptPanel` (with `LiveSummaryPanel` inside it, partials,
You/Others chips and the "Question for you" tag), `CopilotPanel` and
`FloatingRecordingControls` (the record pill). Only the notes column differs: a read-only view
of the notes Copilot was given, because `LiveNotesPad` writes notes to disk.

Below it, a transport bar; on the right, a collapsible debug drawer.

### How the real components are driven

`lib/liveSource.ts` adds a `LiveSource`: `{ kind, listen(event, handler), invoke(command, args) }`
with a React context. The default value is `tauriLiveSource`, which calls
`@tauri-apps/api/event.listen` and `@tauri-apps/api/core.invoke` with exactly the arguments the
components passed before, so production behaviour is unchanged (tests check the default path
still calls Tauri). The live components read it with `useLiveSource()`:

| Component / hook | Reads through the source |
| --- | --- |
| `useCopilot` | `copilot_get_state`, `copilot-state`, `copilot-delta` |
| `CopilotPanel` | `copilotApiFor(source)`: ask, stop, retry, shorter, dismiss, notes, settings |
| `useLiveTimeline` (`LiveSummaryPanel`) | `get_live_timeline`, `live-timeline-update` |
| `LiveTranscriptPanel` | `transcript-partial`, `set_live_transcription_enabled` |
| `RecordingWaveform` | `get_recording_level` |

The other inputs are contexts the replay page overrides for its subtree only:
`TranscriptContext` (final lines at the cursor), `RecordingStateContext` (recording / stopping /
processing from the run's phase, the timer from meeting time), `RecordingControllerContext`
(start / stop / pause never touch the recorder: pause/resume toggles playback, the others
explain that replays never record) and `ConfigContext` (live transcription treated as always on,
so the panel's toggle cannot write the user's preference).

`ReplaySource` (`lib/replay/replaySource.ts`) implements `LiveSource` over the fold:

- `listen` registers a handler and immediately hands it the state at the cursor for that event
  (the copilot snapshot, the live summary snapshot, each source's partial), so a component
  that subscribes mid-replay is hydrated exactly as if it had been listening all along.
- `invoke` answers reads from the fold (`copilot_get_state`, `get_live_timeline`, a level for
  the waveform), routes Copilot asks to the live Copilot below, and refuses or ignores writes.
- Playing forward delivers the recorded events between the old and new cursor, in order, as
  the app received them (so `applyDelta`, `applyPartial` and `acceptSnapshot` run for real).
  A jump of more than 300 records is delivered as the coalesced state instead.
- Seeking (either direction) computes the state at the new cursor from the reducer's snapshot
  index and delivers it as one consistent snapshot. When a slice goes from something back to
  nothing (seeking before the first Copilot or live summary event) the workspace remounts
  (`epoch` key) and rehydrates through `invoke`, since the events cannot express "none".

### The reducer

`lib/replay/reducer.ts` is a TypeScript port of `site/replay/parser.js` (`reduce`, `fold`,
`buildIndex`, `stateAt`, `countAtTime` / `countAtAudio`, `buildLanes`, `summarize`,
`nextOfName`, `liveCursor`, `parseInteractive`, `audioPlan`, `needsResync`). A parity test
folds `site/replay/sample-events.jsonl` with both and compares every snapshot; others check
that seeking in any order equals the fold from the start.

### Transport

Play/pause (Space), speeds 1×, 2×, 4×, 16×, 64×, 256×, 1024× (16× default), a meeting-time /
wall-time axis toggle, the multi-lane timeline (a React port of the site's canvas: cached lane
layer, dimmed future, cursor; click to seek, drag to pan, wheel to zoom, double-click to fit),
previous/next marker by record name (←/→), and the meeting audio.

Audio is the run's source recording (`sandbox/meeting/audio.*`, a link to the real file).
`replay_audio_path` canonicalizes it, allows exactly that file on Tauri's asset protocol and
returns its path; the page plays it with an `<audio>` element (`convertFileSrc`), which streams
ranges instead of decoding the whole file. The CSP gains `media-src 'self' asset:
http://asset.localhost`. Behaviour mirrors the site viewer (`audioPlan` / `needsResync`): on the
meeting axis, at ≤ 4× it plays at the playback rate with `preservesPitch`; above 4× and on the
wall axis it is silent; at 1× `audio.currentTime` drives the clock; drift over 250 ms resyncs.
Volume and mute are remembered per device.

### Live Copilot

The Copilot input and quick actions work at any cursor: `copilot_ask` from the real
`CopilotPanel` becomes `replay_copilot_ask { run, audioMs, seq, kind, text, action, requestId }`.
Rust reuses `replay::serve`'s live Copilot, now transport-agnostic (`replay::live_ask`): inputs
rebuilt from `events.jsonl` up to the cursor (`live_copilot::inputs_at`), the real `Core`,
prompts, provider resolution from the run's sandbox settings, streaming, prefix reuse, one
helper kept alive, asks serialized, cancellable. Progress arrives as `replay-copilot` events
(`queued`, `context`, `warming`, `start`, `first-token`, `delta`, `done`, `error`) and the
source turns them into `copilot-state` / `copilot-delta` for the panel, appended after the
recorded messages. Playback pauses while an answer streams (a toggle in the drawer). Each ask is
saved to the run's `interactive.jsonl`; the drawer lists them as "You asked live at [MM:SS]",
and `[MM:SS]` links (there and in answers) seek the replay.

### Debug drawer

Collapsible, on the right: run header and limitations, pass/fail scorecard from `report.json`,
live asks, Copilot requests (latency, tok/s, reused tokens), detector decisions (filterable by
step), llama-helper queue and preemptions, live backlog / RTF sparkline, warnings. Rows before
the cursor are marked past; clicking a row's time seeks.

## Safety

- The replay subtree never calls a recording, transcription, summary or meetings-database
  command: the overridden controller and `ReplaySource` refuse them. Replay records are never
  emitted on the Tauri event bus, so the app's global listeners never see them, and no meeting
  is created.
- Rust commands only read run folders directly under `<app data>/replays` (`serve::run_dir`),
  open the sandbox database read-only, and write only the run's `interactive.jsonl`.
- Leaving the page pauses the audio and cancels any live Copilot answer
  (`replay_copilot_cancel`).

## Tests

- `tests/lib/live-source.test.tsx`: the default source calls Tauri `listen`/`invoke` with the
  same names and arguments; a provided source replaces it.
- `src/lib/replay/reducer.test.ts`: parity with `site/replay/parser.js`; seeking forward and
  backward equals the fold.
- `src/lib/replay/replaySource.test.ts`: hydration on subscribe, incremental delivery, seek
  snapshots, live-ask composition.
- `src/lib/replay/transport.test.ts`: speeds, clock steps, audio plan and resync.
- Rust: `replay::commands` run index and file access stay inside the root; `live_ask` context
  reconstruction at a cursor; the asker's sink stops on cancel.
