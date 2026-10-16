# Live summary timeline

Date: 2026-10-16. Status: approved, implemented on `enhance/live-summary-timeline`.
Builds on `enhance/live-offline-transcription` (Nemotron live text and the background final pass).

## Goal

While a meeting is recording, keep a short, time-stamped list of what was said: points,
decisions and action items, one or two entries a minute. After the meeting the same entries
become the meeting's **Chapters**. Clicking a chapter jumps the transcript to that time.

## Non-goals

- Replacing the full summary. The post-meeting summary still runs as before.
- Editing entries. They are read-only in this first version.
- Changing the summarization internals. `fix/summary-consistency` owns `summary/processor.rs`,
  llama-helper sampling, `summary_engine/models.rs`, `llm_client.rs` and `autoSummary.ts`.
  This feature only calls their entry points.

## Architecture

```
live worker (Nemotron or chunked) ──emit──▶ "transcript-update" (final segments only)
                                              │ app.listen (per recording session)
                                              ▼
                               live_summary::worker (tokio task)
         ┌────────────────────────────────────┼─────────────────────────────────────┐
         │ WindowScheduler (pure)             │ prompt::build (pure)                 │
         │  pending segments, cadence 60 s,   │  last 3 entries, "You"/"Others"     │
         │  ≥ 25 words, one request in flight │                                      │
         └────────────────┬───────────────────┴──────────────┬──────────────────────┘
                          ▼                                  ▼
               summary model (same as the summary setting) ─▶ parse::entries (pure)
                          │
                          ├─▶ meeting_timeline_entries (SQLite) once the meeting id is bound
                          └─▶ "live-timeline-update" event + in-memory snapshot
```

The module is `frontend/src-tauri/src/live_summary/`:

| File | Role |
| --- | --- |
| `scheduler.rs` | Pure. Pending segments, when a window is due, merging and coalescing. |
| `prompt.rs` | Pure. System and user prompts. |
| `parse.rs` | Pure. Defensive JSON parsing of the model output. |
| `llm.rs` | Resolves the summary provider and model and calls the existing LLM entry points. |
| `worker.rs` | The per-session task: listens, schedules, calls, persists, emits. |
| `mod.rs` | Session lifecycle (`start_session`, `finish_session`), settings, Tauri commands. |

Persistence is `database/repositories/timeline.rs`, with the migration
`20261016000000_add_meeting_timeline_entries.sql`.

## Lifecycle

- **Start.** `start_recording*` calls `live_summary::start_session(app, session_id)` once
  capture is live. It never returns an error to the recording path. Inside, it registers a
  `transcript-update` listener, which forwards final segments into an unbounded channel, and
  spawns the worker. The worker belongs to the recording session, not to the meeting page, so
  navigation does not affect it.
- **Meeting id.** The frontend creates the meeting row right after start and binds it to the
  session through `meeting_activity`. The worker reads `recording_identity().meeting_id` on
  every tick. Entries made before the binding stay in memory and are written once the id
  appears.
- **Pause.** While `recording_identity().status == Paused`, nothing is scheduled. Segments
  that arrived before the pause stay pending.
- **Stop.** After the live transcription worker drains, `stop_recording_session` calls
  `live_summary::finish_session()`. That call only sends a signal, so it never delays saving.
  The worker then:
  1. makes one last request if the pending text is big enough;
  2. gives the request in flight up to `STOP_GRACE` (15 s), then cancels it;
  3. writes what it has and exits.
- **Watchdog.** If the recording session disappears without a finish signal (an error path),
  the worker notices on its next tick and finishes the same way.

## Windows and backpressure

Constants are in `scheduler.rs`.

| Constant | Value |
| --- | --- |
| `CADENCE` | 60 s since the last request started |
| `MIN_WORDS` | 25 words of new text |
| `MAX_WINDOW_WORDS` | 1200 |
| `MAX_IDLE_CARRY` | 5 min |

- **At most one request in flight.** While it runs, new segments keep collecting, so a window
  that comes due merges into the next request.
- **Far behind.** All pending text becomes one window. If it is longer than
  `MAX_WINDOW_WORDS`, the oldest segments are dropped, the drop is logged, and the window
  starts at the first kept segment. The final summary still covers everything.
- **Silence.** Below `MIN_WORDS` there is no request and no entry. Short text carries over to
  the next window, unless it is older than `MAX_IDLE_CARRY`, in which case it is dropped. This
  stops a stray "okay" from widening the next window by ten minutes.
- **Entries per window.** A normal window asks for 1–2 entries. A merged window, one spanning
  more than 1.5× the cadence, may return up to 3.

## Prompt

- **System prompt.** You write a live meeting timeline. The labels are explained:
  - `You` is the person recording, on the microphone.
  - `Others` is the remote participants from system audio, possibly several people.

  Output is JSON only:
  `{"entries":[{"start":"mm:ss","end":"mm:ss","kind":"point|decision|action","text":"…","speaker":"You|Others|null"}]}`.
  - `text` is at most 15 words.
  - `decision` means something was agreed.
  - `action` means someone will do something.
  - An empty list is allowed when nothing new happened.
- **User prompt.**
  - The last 3 entries, under "Already noted (do not repeat)".
  - The window's transcript lines, as `[mm:ss] You: …`.
  - How many entries to return.

## Output parsing

`parse.rs` returns `Vec<ParsedEntry>` or a `ParseError`.

- Strips `<think>…</think>` (and an unclosed `<think>` up to the first `{`) and code fences.
- Accepts `{"entries":[…]}`, a bare array, or a single object.
- If the JSON is truncated (because of the 150-token cap), it salvages every complete
  `{…}` object that has a `text`.
- **Times.** Times may be `"mm:ss"`, `"h:mm:ss"` or seconds. They are clamped into the
  window. A missing or invalid time falls back to the window's bounds, and `end` is never
  before `start`.
- **Kind.** Synonyms are normalised: "action item" and "todo" become `action`, "agreed"
  becomes `decision`. Anything else becomes `point`.
- **Text.** It is trimmed and capped at 200 characters. Empty text is dropped, as is a
  near-duplicate of a recent entry (normalised, case-insensitive). At most the requested
  number of entries is kept.
- **Errors.** Output with no usable JSON is a `ParseError`. The worker logs it, skips the
  window, and sets the status to failed. The next window retries.

## Model call

`llm.rs` resolves the provider and model from the summary model setting, the same way
`SummaryService::regenerate_meeting_title` does. That resolution is shared:
`summary::service::load_llm_config` is now `pub(crate)`.

- **Built-in AI.** It calls `summary_engine::generate_with_builtin_options` with
  `max_tokens = 150` and `temperature = 0.2`. This is a narrow addition next to
  `generate_with_builtin`, which now delegates to it with the old defaults.
- **Other providers.** They call `llm_client::generate_summary` with `max_tokens = 150` and
  `temperature = 0.2`. Today only Custom OpenAI forwards these values. The others rely on the
  short-output instruction, and the parser tolerates truncation.
- **Seed.** No seed is sent. llama-helper on this base has no seed, and
  `fix/summary-consistency` adds a fixed default seed.
- **Timeout.** Each request has its own 60 s timeout and a cancellation token.

## Persistence

```sql
CREATE TABLE meeting_timeline_entries (
  id TEXT PRIMARY KEY NOT NULL,
  meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('point','decision','action')),
  text TEXT NOT NULL,
  speaker TEXT,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX idx_meeting_timeline_entries_meeting ON meeting_timeline_entries(meeting_id, start_ms);
```

- **Times.** They are audio time relative to the recording start. A resumed recording already
  carries the resume offset in its segment times. The entries therefore still line up after
  the final pass replaces the live transcript.
- **Inserts.** They use `INSERT OR IGNORE` on the entry's UUID, so retrying a write is safe.
  `foreign_keys` is on (the sqlx default), so a write for a meeting that was deleted or
  discarded during the recording fails. The entry is not resurrected, which is the lesson of
  PRA-643. The worker logs this and drops the entry.
- **Deletes.** `delete_meeting_with_transaction` explicitly deletes the meeting's entries, as
  it does for transcripts. The cascade also covers `delete_debug_meetings`.

## Events and commands

- **`live-timeline-update`.** Payload `{ session_id, meeting_id, status, error, entries }`,
  where `entries` is everything this session has made so far.
  - `status` is one of `listening`, `generating`, `failed`, `unavailable`, `disabled` or
    `stopped`.
  - `failed` comes with a short message. The next window retries automatically.
- **Commands.**
  - `get_live_timeline` returns the current session's snapshot, so the panel rehydrates
    after navigation.
  - `list_meeting_timeline_entries(meetingId)` reads the stored entries.
  - `get_live_summary_settings` and `set_live_summary_settings(enabled)` read and write the
    setting.

## Settings

- `store.json` key `liveSummaryEnabled`. An absent key means on.
- The toggle is in Settings → Summary, under "Live summary".
- The worker checks the key each time a window comes due, so turning it off mid-meeting stops
  new requests.

## UI

- **During recording.** A "Live summary" section at the top of the live transcript dock
  (`LiveSummaryPanel`).
  - It is collapsible and capped at about 40% of the height.
  - It shows a time-stamped list. Decisions use the `badge.success` recipe and actions use
    `badge.info`.
  - A subtle "Listening…" tail shows while the session is active.
  - When the status is `failed`, a non-blocking `panel.warning` notice reads "Live summary
    paused". It clears on the next success.
- **After the meeting.** A "Chapters" list above the transcript in `TranscriptPanel`.
  Clicking an entry dispatches `meetily:jump-to-time` `{ seconds }`.
  `VirtualizedTranscriptView` handles that event with `nearestSegmentIndex`, the helper the
  base branch added for the final-pass swap. If the target is past the loaded page, it lands
  on the last loaded row.

## Failure isolation

- Every call from the recording path into this module is infallible and non-blocking.
- The worker runs on its own task. Panics inside it are caught by the task boundary and are
  only logged.
- LLM errors, parse errors and database errors change only the feature's own status.
- The listener is removed when the worker exits.

## Risks

- **GPU contention.** With the built-in model, llama-helper decodes on Metal while Nemotron
  decodes on CPU (sherpa-onnx). They compete for memory bandwidth and CPU threads, not for
  the GPU queue. The final pass (Whisper on Metal) can overlap with the last timeline request
  for at most `STOP_GRACE`.
- **Cloud providers.** They see transcript text every minute. This is the provider the user
  already chose for summaries, so the privacy stance does not change.
