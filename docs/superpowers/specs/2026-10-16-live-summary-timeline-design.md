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
         │  pending segments, cadence 60 s,   │  topic, glossary, last 8 notes       │
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
- **Entries per window.** A normal window allows up to 2 entries (usually one). A merged window, one spanning
  more than 1.5× the cadence, may return up to 3.

## Prompt

Revised on `enhance/live-notes-style`: the model is a **notetaker**. Notes say what was said,
never who said it, and sit under topics.

- **System prompt.** The labels are explained (`You` is the person recording, `Others` is
  everyone else) and the model is told never to mention them in a note.
  - Headline style, 6–12 words and never more than 14; facts, names and numbers first; tidy
    numbers. Usually one note per window; `{"entries":[]}` is a normal answer.
  - Numbers only from the new transcript's own lines about that subject, never moved from one
    company or person to another.
  - `action`: a commitment or follow-up someone takes on: "I'll …", an agreed "let's / we
    should …", an offer ("I can introduce you…"), or a request You accept. `speaker` is the
    owner (`You`, a name, or `Others` when the other side commits) and `text` is the task
    alone. `decision`: an explicit agreement. Everything else (facts, plans described,
    suggestions nobody took on, questions) is a `point`; points and decisions have
    `speaker: null`.
  - `topic`: what is being discussed now, in at most 5 words (a company, product, project or
    person). A worked example: background → current work → another company is three topics.
  - Short good/bad examples (synthetic, no meeting content), including two positive actions,
    one owned by each side.

  Output: `{"entries":[{"start":"mm:ss","end":"mm:ss","topic":"…","kind":"point","text":"…","speaker":null}]}`.
- **Topic step** (local models only: built-in, Ollama). Before the notes call, one small call
  asks whether the window is still mainly about the current topic
  (`prompt::topic_user_prompt`: the topic, since when, its last 3 notes and the window's
  transcript). The answer is `{"same":true}` or `{"same":false,"topic":"…"}`;
  `notes::decide_topic` keeps the current topic for "same", a near-duplicate title or one
  sharing two specific words ("Sequoia Investment Thesis" / "Sequoia's Investment Strategy"),
  and reuses an earlier title when the conversation returns to it. Asked for notes and a topic
  together, the 4B model kept copying the current topic for 7+ minutes after the subject
  changed.
  - Not asked while the current topic is younger than 3 minutes, nor for the tail window at
    stop, nor when there is no topic yet: the notes call names the topic then. Cloud
    providers never get it, so a window stays one request there.
  - Its own timeout is 15 s; with the notes call it shares one 60 s budget per window.
  - A failed step (pre-empted, refused, timed out) fails the window, which goes through the
    usual retry and back-off; the notes call is not made. An unreadable answer leaves the
    topic to the notes call.
  - Traced as `timeline-request` with id `<n>-topic`, next to the window's own request.
- **User prompt.**
  - `Topic of the new transcript: <title> (already decided …)` when the topic step answered;
    otherwise `Current topic: <title> (since mm:ss)`, offered as a choice once it has run over
    5 minutes.
  - `Glossary (use these spellings): …`: up to 20 names from earlier notes (`glossary.rs`).
  - The last 8 notes, grouped under their topics, under "Notes so far (do not repeat)".
  - The previous window's last 30 s (at most 90 words), under "Earlier transcript (context
    only, already covered)".
  - The window's transcript lines, as `[mm:ss] You: …`, and "Write up to N notes (usually one)".

## Output parsing

`parse.rs` returns `Vec<ParsedEntry>` or a `ParseError`. `parse_notes` adds the notetaker rules
of `notes.rs`; `parse_entries` is the reply as written (kept for the A/B tool).

- Strips `<think>…</think>` (and an unclosed `<think>` up to the first `{`) and code fences.
- Accepts `{"entries":[…]}`, a bare array, or a single object.
- Repairs mismatched brackets (a dropped `}` before `]`); the reading with more entries wins
  over the reply as written, so a repair never stops at the first object. If the JSON is
  truncated, it salvages every complete `{…}` object that has a `text`.
- **Times.** Times may be `"mm:ss"`, `"h:mm:ss"` or seconds. They are clamped into the
  window. A missing or invalid time falls back to the window's bounds, and `end` is never
  before `start`.
- **Kind.** Synonyms are normalised: "action item" and "todo" become `action`, "agreed"
  becomes `decision`. Anything else becomes `point`.
- **Notetaker rules** (`notes.rs`):
  - Narration ("Others discuss…", "The speaker notes that…", "Speaker holds…") is rewritten to
    its content, or dropped when too little is left or it describes the person ("Others are
    excited…"). A narrated note is never an action or a decision.
  - "You'll send the deck" becomes owner `You`, task "send the deck"; "Others will introduce
    you…", "They'll share…", "Priya agreed to send…" become owner `Others` or `Priya`. An
    action without an owner, or whose text is a statement ("The team…", "Goal: …"), becomes a
    point, as does narration ("Others suggest…"). A named owner missing from the text is put
    in front ("Priya — build the model").
  - A decision phrased as a question becomes a point. Points and decisions drop `speaker`.
  - Notes are at most 14 words: cut to their leading clauses (`;`, ` — `, `,`), else before
    the last clause word ("with", "while", "using"…), else at 14 words without a dangling last
    word. A "from X to Y" range is never cut inside. "Ai"/"Ml"/"Av"/"Gpu" become acronyms.
  - **Numbers** (`facts.rs`): a figure not in the note's window or the context lines before
    it is taken out: its clause when the note has several, else the figure and the words that
    only qualify it ("about", "$", "percent", a month and its "on"). A note left with fewer
    than 4 words is dropped. Spoken compounds ("a hundred and fifty thousand", "two thousand
    five hundred"), ordinals ("twenty fourth"), years ("twenty twenty seven"), decimals ("two
    point uh seven") and scale suffixes ("10K", "$5M") match; clock times, versions ("v2.1")
    and names with digits ("Q3") are not checked; magnitude words ("thousands") need the same
    word or a figure that large.
  - With a topic step answer, every note of the window takes the decided topic.
  - Names that are close variants of an established glossary spelling (seen twice, more
    often than the variant) are rewritten to it.
  - Topics are tidied (≤ 5 words, title-case-ish). A proposal that is a near-duplicate of the
    current topic, or shares a specific word with it, reuses the current title; one that is a
    near-duplicate of an earlier topic reuses that title. No topic means the current one.
- **Text.** It is trimmed and capped at 200 characters. Empty text is dropped, as is a
  near-duplicate of one of the last 8 entries, judged on the refined text: word overlap of
  0.8, or (notes only) a rewording that shares at least 4 content words, 40% of the shorter
  note's and 30% of both notes' together. Notes whose figures differ ("May 10" → "May 24") or
  where only one is negated ("will ship" → "will not ship") are never repeats. At most the
  requested number of entries is kept.
- **Errors.** Output with no usable JSON is a `ParseError`. The worker logs it, skips the
  window, and sets the status to failed. The next window retries.

## A/B evaluation

`live_summary/eval.rs` is an ignored test that replays a replay run's `events.jsonl` through
the scheduler, the old and the new prompt and parser on a simulated clock with the real
built-in model, and writes `live-summary-ab.md` into the replay folder. That report quotes
meeting content: it stays on the machine and is never committed. The usage is in the file's
header. Its metrics include max words, notes over 14 words, repeats of the last 8 notes and
notes with a number not in their transcript, judged the same way for every variant;
`live_summary_rescore` recomputes them for an existing report's notes.

## Model call

`llm.rs` resolves the provider and model from the summary model setting, the same way
`SummaryService::regenerate_meeting_title` does. That resolution is shared:
`summary::service::load_llm_config` is now `pub(crate)`.

- **Built-in AI.** It calls `summary_engine::generate_with_builtin_options` with
  `max_tokens = 256` (150 before topics) and `temperature = 0.2`. This is a narrow addition next to
  `generate_with_builtin`, which now delegates to it with the old defaults.
- **Other providers.** They call `llm_client::generate_summary` with `max_tokens = 256` and
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
-- 20261016000400: the note's section title; NULL on entries made before topics.
ALTER TABLE meeting_timeline_entries ADD COLUMN topic TEXT;
```

- **Topic.** Optional everywhere: rows, the `live-timeline-update` entries and the frontend
  type. An entry without one serializes without the field, so older readers (the replay
  viewer) are unaffected, and the UI renders it without a header.

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
  - It shows a time-stamped list grouped under topic headers (a thin `hairline` rule and the
    title; `groupByTopic` starts a section when the topic changes). Decisions use the
    `badge.success` recipe and actions use `badge.info`. The speaker is never shown, except
    an action the user owns, which reads "You — task", and entries made before topics, which
    keep their "· speaker" label. The panel and the chapters read through `useLiveSource`, so
    the in-app replay shows the same headers; `site/replay` groups the same way.
  - A subtle "Listening…" tail shows while the session is active.
  - When the status is `failed`, a non-blocking `panel.warning` notice reads "Live summary
    paused". It clears on the next success.
- **After the meeting.** A "Chapters" list above the transcript in `TranscriptPanel`, grouped
  the same way. Clicking an entry dispatches `meetily:jump-to-time` `{ seconds }`.
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
