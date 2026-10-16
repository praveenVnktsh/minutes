# Live Copilot

Date: 2026-10-16. Status: approved (user-vetted storyboard), implemented on `enhance/live-copilot`.
Builds on PR #95 (Nemotron live transcription, live summary timeline, serialized llama-helper).

## Goal

While a meeting records, the lower half of the right dock (where "AI chat" stacks under the
transcript) becomes **Copilot**. It answers questions about the last few minutes and suggests
a reply when someone else asks the user something. Answers stream token by token, cite moments
as `[MM:SS]` links, and are saved into the meeting's AI chat thread.

## Non-goals

- An overlay or a floating window. Copilot lives in the existing dock.
- Changing the summary pipeline, its budget/chunking, Ollama `num_ctx` logic, speaker
  identity, diarization, the transcription queue or the final pass. Copilot only reads the
  live transcript and live timeline, and calls the model entry points.

## UI

- The header pill reads **Copilot** while recording and **Chat** otherwise
  (`MeetingWorkspace.assistantLabel`). During recording the dock's lower panel renders
  `CopilotPanel` instead of `MeetingAssistantPanel`; after the meeting the AI chat panel
  shows the saved copilot moments.
- `CopilotPanel` copies `MeetingAssistantPanel`'s layout: header (title "Copilot",
  subtitle "Live · answers from the last few minutes and the live summary"), a
  "Suggestions" switch, quick-action pills (**What should I say?**, **Explain that**,
  **Catch me up**, **Draft follow-up**), a message list and the input "Ask about this
  meeting…".
- Answers render as Markdown while streaming, with **Copy**, **Stop** (while streaming) and
  **Shorter**. `[MM:SS]` and `[H:MM:SS]` become buttons that dispatch the existing
  `meetily:jump-to-time` event, so they work against the live transcript and, after the
  final pass replaced it, against the final one (times, not row ids).
- A suggestion card: header "OTHERS ASKED YOU · [MM:SS]", the quoted line, the streaming
  reply, and Copy / Not for me / Dismiss. The transcript line gets a "Question for you" tag
  in the warning tone (`panel.warning`/`badge.warning`).
- Pills and input are disabled until the first final transcript line exists.
- A failed request shows an inline `panel.error` card "Copilot couldn't get an answer" with
  the reason, Retry, and "Use built-in model instead" when a cloud provider failed.
- A debug line (hidden unless Settings → Copilot → Debug is on) under the input: first-word
  latency, tok/s, prompt tokens reused/total, detector decisions and the false-alarm count.

## Backend: `frontend/src-tauri/src/copilot/`

| File | Role |
| --- | --- |
| `detector.rs` | Pure. Rule check on a finalized Others line. |
| `context.rs` | Pure. Rolling transcript buffer, the 3-minute window, caps, prompt layout. |
| `prompt.rs` | Pure. System prompt, quick-action tasks, confirm prompt, Shorter. |
| `stream_parse.rs` | Pure. SSE and NDJSON decoders and per-provider event parsing. |
| `stream.rs` | Streaming calls: built-in (llama-helper), Ollama, Claude, OpenAI-compatible. |
| `session.rs` | Pure-ish state: messages, active card, cooldown, debug counters. |
| `persist.rs` | Writes copilot moments into `meeting_chat_messages`. |
| `mod.rs` | Session lifecycle, the transcript listener, request orchestration, commands, events. |

### Lifecycle

- `finalize_recording_start` calls `copilot::start_session(app, session_id)` next to the live
  summary. It registers a `transcript-update` listener (final lines only) and nothing else.
  **No model request is ever made while idle.**
- `stop_recording_session` calls `copilot::finish_session(session_id)`: new requests are
  refused ("The meeting has ended"), in-flight copilot requests are cancelled in the helper
  (never by killing it), unsaved moments are written, and the listener is removed.

### Context (per request)

Built by `context::build`:

1. **System prompt** (stable). Explains `You` (the person recording, microphone) and
   `Others` (remote participants, system audio), the `[MM:SS]` citation rule, brevity, and
   honesty: *"If the transcript, live summary and notes do not cover it, say it has not come
   up in the meeting so far. Never guess."*
2. **Live summary so far** (stable): every timeline entry as `[MM:SS] (decision) text`.
   Over the cap the oldest are dropped in blocks of 10 so the prefix stays stable.
3. **Transcript, older part** (stable): lines from the window start up to the stable cut.
4. — prefix boundary —
5. **Transcript, newest part**: lines after the stable cut.
6. **Notes**: the user's live notes, capped.
7. **Task**: the quick action or the question.

The window starts at `floor((latest − 180 s) / 120 s) · 120 s`, so it is 3–5 minutes and its
start only moves every two minutes. The stable cut is `floor(latest / 30 s) · 30 s`. The end of
the live summary block is the prefix's **anchor**: it survives the window moving.

Caps (estimated at 4 characters per token): timeline 1200, transcript 1600, notes 400,
question 200. The whole prompt therefore stays under ~3.6k tokens at minute 5 and minute 60.

### Proactive suggestions

On each finalized **Others** (`system`) line, when Suggestions is on:

1. `detector::rule_check` (pure). Passes on a question mark, an interrogative start
   (what/why/how/when/where/who/which/can/could/would/will/do/does/did/is/are/should …), or a
   request aimed at the user ("you", "your", "can you", "could you", "would you",
   "what do you think", "thoughts?"). Rejects fillers and tag questions ("right?",
   "you know?", "okay?", "yeah?", "make sense?" on their own or as the tail of a statement),
   lines under 3 words, and rhetorical "you know" phrases.
2. Skipped with a recorded reason if a card is already active (a card stops counting after
   2 minutes), the last card was under **20 s** ago, a confirm ran under 8 s ago, a confirm
   is still running, the provider refused us in the last 2 minutes, the recording is paused,
   or **live transcription is more than 2 s behind** (the streaming worker's queued audio).
3. **Confirm** with the model: the same context plus "Is the last line a question or request
   that You should answer? Reply yes or no." `max_tokens = 5`, temperature 0.
4. On yes: create the card, then stream a draft reply (`max_tokens = 300`).
5. "Not for me" increments a session false-alarm counter (debug line).

Never triggered by `mic` lines. One active card at a time.

### Models and streaming

- Model: the Copilot model setting if set (`store.json` `copilotModel = {provider, model}`),
  else the summary model. Keys and endpoints come from `load_llm_config`.
- Sampling: temperature 0.3; replies `max_tokens = 300`, confirm `5`, Shorter `150`.
- Streaming per provider:
  - **Built-in**: llama-helper `generate` with `stream: true` emits
    `{"type":"token","id":N,"text":"…"}` lines and a final `{"type":"response",…}` with
    `finish_reason`, `prompt_tokens`, `completion_tokens`, `reused_tokens`,
    `first_token_ms`, `tokens_per_second`.
  - **Ollama**: `/api/chat` with `stream: true` (NDJSON), `think: false`.
  - **Claude**: `/v1/messages` with `stream: true` (SSE `content_block_delta`).
  - **OpenAI-compatible** (OpenAI, Groq, OpenRouter, custom): `/chat/completions` with
    `stream: true` (SSE `choices[0].delta.content`, `[DONE]`).
- Streaming lives in new files (`copilot/stream*.rs`) and only reuses the body builders in
  `llm_client.rs`, so the summary paths are unchanged.

### Prompt-prefix reuse in llama-helper

Generate requests may carry `cache_prompt: true`, `cache_prefix_chars: N` (byte length of the
stable part of the formatted prompt), `cache_anchor_chars` (the earlier anchor) and
`cache_context_size`.

- The helper keeps one persistent context for cached requests and up to three
  **checkpoints**: token lists of stable prefixes and host-memory copies of sequence 0's
  state (`llama_state_seq_get_data_ext`, ~70 MB per 1k tokens on Qwen 3.5 4B). Checkpoints
  work for attention-only and hybrid/recurrent models (Qwen 3.5) alike; partial KV removal
  does not work on the latter.
- On a cached request: clear the KV cache; restore the longest checkpoint that still starts
  the prompt; evaluate up to the anchor and the stable boundary, saving a checkpoint at each
  one past the restored point; then evaluate the rest. `reused_tokens` reports the restored
  length.
- Non-cached requests (summary, title, timeline) keep the previous code path exactly: a fresh
  context per request. The checkpoint survives them because it lives in host memory.

### Priority and pre-emption

The sidecar's single exchange mutex becomes a small priority queue:

| Priority | Who | Behaviour |
| --- | --- | --- |
| `Background` | live timeline (`OnAbandon::Yield`) | Waits behind interactive waiters; an in-flight one is pre-empted. |
| `Normal` | summary, title, chat, tokenize | FIFO with background; never pre-empted. |
| `Interactive` | copilot | Goes first; pre-empts an in-flight background request. |

Pre-emption is a protocol message, never a kill: requests carry an `id`; the client writes
`{"type":"cancel","id":N}`; llama-helper's stdin reader thread records it, generation stops
between tokens (or after the prompt decode), and the helper still writes the **one** final
reply line for request N with `finish_reason: "cancelled"`. The pre-empted caller reads its
own reply and returns `Preempted`, so the line protocol never goes out of step and no caller
can read another's reply. A cancel for an id that is not running is ignored and produces no
output. The cancel is only sent once the helper has shown it understands ids (it echoes `id`
in replies); against an older helper copilot simply waits.

The timeline worker treats `Preempted` as "retry soon": the window's text is requeued, the
status stays `listening`, and the next attempt is 10 s later. Summary and title requests are
`Normal` and are never cancelled by copilot.

Copilot's own Stop and timeout also use the cancel message, so they never kill the shared
sidecar.

### Live transcription contention

Measured on the user's Mac: Nemotron alone runs at ~0.6× real time; back-to-back local
Qwen 3.5 4B generations push it past 1.0×. Copilot therefore:

- asks llama-helper for **2 generation threads while a recording is live**
  (`n_threads`; prompt evaluation keeps all threads, because on Qwen 3.5 part of it runs
  on the CPU and two threads made a cold prompt ~8× slower);
- keeps proactive work tiny and rate limited (5-token confirm, ≥ 8 s apart, one at a time,
  one card per 20 s) and **sheds it when the live worker reports more than 2 s of backlog**;
- relies on prefix reuse, so a typical request evaluates ~50–300 new prompt tokens;
- and the live timeline (background priority) waits **4 s after any built-in generation**
  while recording, so the model never runs back to back with Copilot.

### Failure handling

- 15 s timeout per copilot request, **including** time queued behind a Normal request.
- Auth (401/403) and 429: plain message ("Copilot's model refused the request: check the API
  key" / "rate limited"), no automatic retry; proactive suggestions pause for 2 minutes.
- Errors only change the copilot message's state; recording, transcript, timeline and the
  final pass are untouched (separate tasks, no shared locks except the sidecar queue, where
  copilot can only pre-empt background work).

### Persistence

Migration `20261016000300_add_chat_message_kind.sql` adds `kind TEXT`, `label TEXT` and
`audio_ms INTEGER` to `meeting_chat_messages`. A finished moment writes two rows: the user
side (question, quick-action label or quoted question) and the reply, with `kind` one of
`live_ask`, `live_action`, `live_suggestion`. Rows wait in memory until the meeting id is
bound, like timeline entries. `delete_meeting_with_transaction` explicitly deletes the
meeting's chat rows (the FK cascade covers it too). The AI chat panel labels them
"LIVE · [25:31] · WHAT SHOULD I SAY?".

### Events and commands

- `copilot-state`: snapshot `{ session_id, active, line_count, messages, card, debug,
  suggestions_enabled, debug_enabled }` on structural changes.
- `copilot-delta`: `{ id, delta }` per streamed chunk.
- Commands: `copilot_get_state`, `copilot_ask(action?, question?)`, `copilot_stop(id)`,
  `copilot_retry(id, useBuiltin)`, `copilot_shorter(id)`, `copilot_dismiss_card(id,
  notForMe)`, `copilot_set_notes(text)`, `get_copilot_settings`, `set_copilot_settings`.

### Settings

Settings → Summary → **Copilot** card: Suggestions toggle (`copilotSuggestionsEnabled`,
default on), Copilot model picker ("Same as summary model" or a provider + model), and a
developer "Show Copilot debug line" toggle (`copilotDebug`, default off).

## Pass criteria

Idle → 0 requests; quick actions usable once the first line exists; streaming visible; local
first words ≤ 3 s / cloud ≤ 1 s; minute 40 about as fast as minute 5 (cap + prefix reuse);
detector card ≤ 3 s after the line finalizes, Others only, one card per 20 s; low false
alarms; `[MM:SS]` links correct after the final transcript replaces the live one; "not
discussed" honesty; failure isolation; history saved; nothing after End meeting.

## Risks

- **Back-to-back local load.** A user who fires requests continuously still slows live
  transcription; the guards above only cover proactive and background work. See the PR for
  measured real-time factors.

- **Memory.** The cached copilot context (8k tokens) and one checkpoint stay resident while
  the helper is alive (it exits after 5 idle minutes).
- **Different copilot and summary built-in models** make the helper reload models when both
  are used during a meeting. The setting describes this.
- **Pre-emption granularity.** A background request is pre-empted between tokens; its prompt
  decode (≤ ~1.5k tokens) is not interruptible.
