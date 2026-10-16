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
| `script` | A scripted Copilot step: `{at_ms, kind, tag, text, message_id, error}`, or `{kind: "notes", count}` when the user's notes were updated. |
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
