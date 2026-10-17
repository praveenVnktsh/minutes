# Replay viewer

A static page that plays back a pipeline replay: the recording screen (live
transcript dock, live summary, Copilot panel, record pill) at any moment, and a
multi-lane timeline of the whole run.

```bash
open site/replay/index.html                 # or: python3 -m http.server -d site  →  /replay/
open "site/replay/index.html#sample"        # the synthetic demo
```

Open an `events.jsonl` (and its `report.json` for the pass criteria) with the
file picker or by dropping them on the page. Files are parsed in the browser.
Nothing is uploaded and the page makes no network requests with your data.

Replays come from the recorder (`frontend/src-tauri/examples/replay.rs`):

```bash
cargo build --release -p llama-helper
cargo run --release -p meetily --features dev-replay --example replay -- --audio "/path/to/audio.mp4"
```

llama-helper is found next to the binary or in the workspace's `target/`, so the
recorder runs from any folder; `MEETILY_LLAMA_HELPER` still wins when set.

## Served, with a live Copilot

```bash
cargo run --release -p meetily --features dev-replay --example replay -- serve --open
# → http://127.0.0.1:7357/replay/   (--port N; --site site/replay serves the viewer from disk)
```

`replay serve` serves this viewer and the runs under the replays folder on
127.0.0.1 only. The viewer then shows a **Runs** list (newest first; a run still
being recorded is followed live) and the Copilot input works: type a question
(Enter sends, Shift+Enter is a new line) or click a quick action, and the real
Copilot answers as if asked at the cursor's moment of the meeting. Playback
pauses while it answers (a toggle under the input). Answers stream with Stop and
Copy, first-word latency, tokens/s and prompt-prefix reuse, and their `[MM:SS]`
citations move the cursor.

What runs is the app's Copilot (`copilot::core::Core`): its context builder,
prompts, model resolution (the run's sandbox settings; the built-in model by
default), streaming client, prefix reuse and budget trimming. Its inputs are
rebuilt from `events.jsonl` up to the cursor (`replay::live_copilot`): the final
transcript lines Copilot had heard, the newest live summary snapshot and the
user's notes, never re-transcribed. One llama-helper stays up across asks (so
prefixes are reused) and asks run one at a time. The only difference is a 60 s
first-word timeout instead of 15 s, so a cold prefix still answers.

The meeting recording plays along: on the meeting-time axis at 1×, 2× and 4×
(pitch kept), the recording is the clock the cursor follows; above 4× and on
the wall-time axis it is muted and the cursor runs on its own. Seeks, jumps and
citation clicks move it too (re-seeked past 250 ms of drift). Volume and mute
are remembered in this browser. The recording is the one the run linked in its
sandbox (`sandbox/meeting/audio.*`), served with HTTP ranges.

Each live ask and its answer is appended to the run's `interactive.jsonl` (never
to `events.jsonl`); the viewer lists them as "You asked live at [MM:SS]".

| Endpoint | |
| --- | --- |
| `GET /api/runs` | The runs: name, recording, length, status (`running`, `done`, `incomplete`), pass/fail counts, start time. |
| `GET /api/runs/<run>/events`, `/report`, `/interactive` | The run's files. |
| `GET /api/runs/<run>/audio` | The run's recording, with `Range` support (206/416); 404 with the reason when the file is gone. |
| `GET /api/runs/<run>/tail?from=<line>` | `events.jsonl` from a line on, as server-sent events, following the file until the run ends (`event: end`). |
| `POST /api/runs/<run>/copilot` | `{audio_ms, seq?, kind: "ask"\|"action", text?, action?}` → server-sent events `queued`, `context`, `warming`, `start`, `first-token`, `delta` `{offset, text}`, then `done` (the saved entry, with `stats`) or `error`. |
| `POST /api/runs/<run>/copilot/stop` | Stops the running answer inside the helper. |

It answers only `Host: 127.0.0.1:<port>` / `localhost:<port>`, sends no CORS
headers, takes POSTs only as JSON from its own page, and serves only run folders
directly under the replays root. Run contents stay on this machine.

It writes `events.jsonl`, `report.json`, `report.md` and `app.log` to
`~/Library/Application Support/com.meetily.ai/replays/<run>/`. Real meetings
are private: never commit or upload those files. The committed sample
(`sample-events.jsonl`, `sample-report.json`, `sample-data.js`) is generated
from a made-up meeting by `node site/replay/make-sample.js`.

## events.jsonl

One JSON object per line, in write order:

```json
{"seq":12,"t_ms":4810,"audio_ms":5400,"type":"event","name":"transcript-update","payload":{}}
```

| Field | Meaning |
| --- | --- |
| `seq` | 0-based write order. |
| `t_ms` | Wall time since the replay started (the **wall** axis). |
| `audio_ms` | Meeting time (the **meeting** axis): the simulated clock every recording-time rule runs on. It follows the audio while the meeting records, never goes backwards, and runs at the wall clock's rate after the audio ends and while model work is in flight. |
| `type` | `meta`, `event` or `debug`. |
| `name` | See below. |
| `payload` | The event's data. |

### `meta`

- `run-start`: `{schema_version, audio_name, duration_s, fed_s, realtime, live_engine, summary_model, script, notes, limitations[], started_at}`.
- `run-end`, and `run-error` `{error}` if the replay failed.

### `event`: what the app's UI hears

Payloads are exactly what the Rust side emits, so the viewer folds them the way
the app's listeners do: `transcript-update`, `transcript-partial`,
`live-timeline-update`, `copilot-state`, `copilot-delta`, `transcription-error`,
`transcription-warning`, the transcription queue events
(`transcription-queue-*`, `retranscription-*`), `final-transcript-pass-complete`,
`diarization-progress`, `diarization-status-changed`, `diarization-complete`,
`meeting-activity-updated` and `transcripts-updated`.

### `debug`: decisions and measurements

Related records share ids.

| Name | Payload |
| --- | --- |
| `phase` | `{phase}`: `live-start`, `audio-end`, `stop`, `stopped`, `final-pass-start`, `final-pass-done`, `summary-start`, `summary-done`, `run-end`. |
| `detector` | `{decision_id, step, line_ms, decision}`. `step` is `rule_reject`, `skipped` (the reason is in `decision`), `confirming`, `confirm_yes`, `confirm_no`, `confirm_unknown`, `confirm_failed` or `confirm_timeout`. `decision_id` is `line-<line_ms>`. |
| `card` | `{decision_id, message_id, line_ms}`: the suggestion card a decision opened. `message_id` is the Copilot message (`copilot-state`, `copilot-delta`, `copilot-request`). |
| `copilot-request` | Keyed by `id` (the message id): `{phase: "start", task}`, `{phase: "first-token", first_token_ms}`, `{phase: "done", status, error, kind, stats}`. Latencies are wall time. |
| `timeline-request` | Keyed by `id`: `{phase: "start", start_ms, end_ms, words, dropped_segments}`, `{phase: "done", outcome}` (`done`, `truncated`, `preempted`, `transient`, `unavailable`). |
| `sidecar` | llama-helper's queue, keyed by request `id`: `{phase: "queued", priority}`, `{phase: "acquired", waited_ms}`, `{phase: "done", ok, busy_ms}`, `{phase: "preempt", id, by}`; and from the app log `{what, message}` (`spawn`, `cancel`, `timeline-preempted`, `timeout`, …). |
| `script` | A scripted Copilot step: `{at_ms, kind, tag, text, message_id, error}`, or `{kind: "notes", count, text}` when the user's notes were updated (`text` is what Copilot was given). |
| `live` | Every 250 ms: `{backlog_s, rtf, rtf_total, busy_s, decoded_audio_s, fed_audio_s, llm_busy}`. `busy_s` and `decoded_audio_s` are cumulative decode time and decoded audio. |
| `memory` | Every 5 s: `{app_rss_mb, helper_rss_mb}`. |
| `final-transcript` | `{lines: [{start, end, speaker, text}]}` after the final pass and diarization. |
| `summary` | One summary run: `{run, status, markdown, error, ms}`. |
| `log` | A warning or error the app logged: `{level, target, message}`. |

## The reducer

`parser.js` derives everything from the records:

- `reduce(state, record)` folds one record into the recording screen's state:
  final lines (`transcript-update`), the in-progress partial per source
  (`transcript-partial`, an empty text clears it), the live summary snapshot
  (`live-timeline-update`), the Copilot snapshot (`copilot-state`) with
  streamed chunks applied at their byte offset (`copilot-delta`, the rule in
  `frontend/src/lib/copilot.ts`), notices, the final pass, the final transcript
  and the summary runs. It also keeps the run's phase and the latest meeting
  and wall times.
- The state at a cursor is `fold(records, n)`: the reduce of the first `n`
  records. `buildIndex` snapshots the fold every 400 records and `stateAt`
  starts from the nearest one, so seeking anywhere, backwards too, is exact.
  Tests check that seeking in any order equals the fold from the start.
- `countAtTime` / `countAtAudio` turn a cursor time on either axis into `n`.
- `buildLanes` turns the records into timeline items, each with a span on both
  axes and the record it starts at.

Run the tests with `node --test site/replay/parser.test.js`.

## Performance

The timeline's lanes are drawn once into an offscreen layer per view (zoom,
size, axis, theme, selection); each frame only copies it, dims what has not
happened yet and draws the cursor. Items narrower than a pixel that share one
are drawn once. The live summary, Copilot and status panels re-render only when
what they show changes, at most every 100 ms while playing, and the transcript
appends new rows. A 57k-event run plays at 60+ fps.
