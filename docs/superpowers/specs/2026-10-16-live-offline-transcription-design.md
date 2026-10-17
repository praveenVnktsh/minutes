# Live and final transcription models

Date: 2026-10-16. Status: approved direction, implemented on `enhance/live-offline-transcription`.

## Goal

Split transcription into two model families:

1. **Live**: streams text while recording. Default: NVIDIA Nemotron Speech Streaming
   0.6B (cache-aware FastConformer RNN-T, English), run through sherpa-onnx's
   `OnlineRecognizer`.
2. **Final**: runs over the whole recording after the meeting ends. Its output replaces the
   live transcript, and diarization then reruns on it. Default: Whisper
   `large-v3-turbo-q5_0`. Parakeet TDT 0.6B v2 and the other Whisper models remain selectable.

## Model source

- The export is `csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25`
  on Hugging Face, by the sherpa-onnx author.
- Downloads are pinned to revision `52056fdc070914a48dcd68b31b44d6a6f5b85902`. Each file
  is checked against its size and the LFS SHA-256 (see
  `audio/live_streaming/model.rs`).
- The int8 encoder is 653 MB. The decoder, joiner and tokens add 9 MB.
- The 560 ms chunk export matches the latency the paper reports (~0.56 s).
- It was verified with sherpa-onnx 1.13.8, the version the app already links: the export
  loads in about 2 s and transcribes the bundled test clips correctly, with punctuation and
  casing.
- Two streams (microphone plus system) decoded in a batch ran at RTF 0.64 combined. That was
  measured with the machine at load average ~25, so real use will be faster.

## Why Whisper turbo is the final default

The user's setting is accented English on Apple Silicon with 16 GB or more. Their pain points
are names and jargon, overall accuracy, and speaker attribution.

- **Names and jargon.** The custom transcription vocabulary only reaches Whisper, through
  its initial prompt. Parakeet has no biasing hook. This is the only lever the app has for
  this pain point.
- **Accents.** Whisper's 680k-hour multilingual, multi-accent training is the most robust
  option for accented English. Parakeet v2 scores a slightly better average WER on the Open
  ASR leaderboard, but that set is mostly native-accent read and broadcast speech.
- **Cost.** Turbo keeps almost all of large-v3's accuracy at a fraction of the decode time,
  and it runs on Metal. A one-hour meeting finishes in a few minutes, which suits an
  automatic pass after every meeting. `large-v3-q5_0` is one click away for users who want
  maximum accuracy.
- **Attribution.** This improves because diarization reruns on the final transcript's
  segment boundaries, which are cleaner than the boundaries of live 20-second forced splits.

## Settings and migration

| Setting | Storage | Default |
| --- | --- | --- |
| Live model (`nemotron-streaming` or `chunked`) | `store.json` key `liveTranscriptionModel` | `nemotron-streaming` when the key is absent |
| When the final pass runs ("Improve transcript after meetings") | `store.json` key `finalTranscriptTiming`: `automatic`, `when-plugged-in` or `off` | `when-plugged-in`; without the key, an old `autoFinalTranscriptPass: false` means `off` |
| Final model | the existing SQLite transcript config row (provider and model) | Whisper `large-v3-turbo-q5_0` |

- **Live model.** Because an absent key means Nemotron, existing users get Nemotron without
  any migration.
- **Final model.** The SQLite row is what retranscription and deferred mode already use.
- **One-time migration** (flag `finalTranscriptModelMigrated`), which runs once onboarding
  is complete:
  - A row that is missing, or still the onboarding default `parakeet` /
    `parakeet-tdt-0.6b-v2-int8`, becomes Whisper turbo.
  - Onboarding always wrote that row, so it cannot be told apart from an explicit choice.
    The migration therefore treats it as a default.
  - Any other choice is kept.
- **Background downloads.** After onboarding, and on each start until they succeed, the
  defaults that are not yet on disk are downloaded in the background. Nemotron reports
  through a `live-model-download-progress` event and Whisper through the existing whisper
  download events. Both appear in the global download toast and in settings.

## Data flow

```
capture ─▶ pipeline (unchanged mixing → recording file)
              │ per-source aligned 48 kHz windows
              ├─ VAD mode (chunked live model): Silero segments → chunk worker (unchanged)
              └─ streaming mode (Nemotron): continuous windows → streaming worker
                     ├─ silence gate (skips digital/near silence, keeps 0.3 s pre-roll)
                     ├─ OnlineStream per source, batched decode
                     ├─ partial text → `transcript-partial` (display only)
                     └─ endpoint → `transcript-update` (final, deduped, sequence id)
stop ─▶ frontend saves live transcript ─▶ `start_final_transcript_pass`
              ─▶ TranscriptionQueue Retranscribe task (final_pass = true)
              ─▶ VAD + final engine over audio.mp4 → replace rows in one transaction
              ─▶ diarization rerun → `final-transcript-pass-complete`
```

**Streaming mode.** The pipeline chooses streaming mode when the session's live plan is
Nemotron. In that mode it sends every aligned microphone and system window, with the
microphone echo-cancelled when that beta is on, instead of VAD segments. A flush (pause or
stop) sends an empty marker chunk per source, and the worker closes the utterance in
progress. The recording path is untouched.

**Segment timing.** sherpa's `start_time` is absolute on the stream's timeline and survives
`reset`, and token timestamps are relative to it. So a segment runs from `start_time` plus
the first token time to `start_time` plus the last token time, plus the resume offset and
small padding.

**Partials.** The frontend's transcript store appends by `sequence_id` and ignores
`is_partial`. Partials therefore travel on their own event, the live panel renders them as
an in-progress line, and they are never persisted.

## Final pass

`start_final_transcript_pass(meetingId, meetingFolderPath, language, force)` decides
whether to run and with which engine:

1. If the timing is `off` and `force` is not set (deferred mode and "Improve now" set it),
   it is skipped. With `when-plugged-in` on battery or in Low Power Mode it waits instead: the row in
   `pending_final_passes` gets `waiting_for_power = 1`, the live transcript is diarized and
   summarized at once, and a watcher queues the pass once the Mac is on power, out of Low Power Mode and not
   recording. The meeting header shows one line, "Transcript improves when plugged in ·
   Improve now".
2. If the meeting has `transcript_revisions` rows, it is skipped, so user edits are never
   clobbered. Speaker overrides survive anyway: they are resolved by time overlap when the
   transcript id no longer matches.
3. It chooses an engine with `choose_final_engine`: the configured model if downloaded, then
   Whisper turbo, then the largest downloaded Whisper, then Parakeet v2, then any Parakeet.
   A fallback is reported to the user. With no engine at all, the pass is skipped with a
   reason.
4. It enqueues a `Retranscribe` task with `final_pass: true` and the title "Final
   transcript".

**Completion.**

- **Success.** The rows are replaced, diarization reruns, and the activity becomes `ready`.
  The existing auto-summary then runs on the final transcript.
- **Failure or cancellation.** The live transcript stays. The transaction is only committed
  on success, so nothing is lost. Diarization runs on the live transcript and the activity
  still becomes `ready`, with a warning, so the summary is not lost either.
- **Event.** Every outcome emits `final-transcript-pass-complete`
  `{ meeting_id, task_id, outcome: "replaced" | "kept_live", provider, model, warning }`.
  This is the hook the upcoming summarization rework should trigger from.

When the pass is queued, the frontend skips its immediate summary and diarization for the
live transcript and lets the pass's `ready` drive them.

## Running quietly

- **Core ML encoder.** whisper.cpp runs the encoder on the Neural Engine when
  `ggml-<model>-encoder.mlmodelc` sits next to the model. The app downloads it for the final
  model in the background (pinned revision, size and SHA-256 checked, unpacked through a
  staging folder) and compiles it once, since the first Core ML load takes minutes.
- **Reused decoding state.** The engine keeps one Whisper state per loaded model instead of
  creating one per segment, which reloaded the Core ML encoder and reallocated GPU buffers
  every time.
- **Utility QoS.** The final pass's decode, VAD and Whisper calls, and diarization, run on
  threads at `QOS_CLASS_UTILITY` (`quiet_work.rs`), so macOS may slow them under load or heat.

## Failure modes

- **Nemotron not downloaded or fails to load.** Live falls back to chunked mode with the best
  available offline engine, and the user gets a non-blocking notice. With no engine at all,
  recording continues without live text and the final pass transcribes later. Recording never
  fails because of a missing model. A missing ONNX runtime still blocks, as before.
- **Live decode falls behind.** If more than 20 s of audio is queued, the worker drops
  backlog windows for the live view, warns once, and relies on the final pass.
- **Model download interrupted.** The file is written to `.part` and only moved into place
  after its size and hash check, so a broken file is never loaded.
- **Final pass during a new recording.** The queue is sequential. Whisper runs on Metal while
  Nemotron runs on CPU. The engine unload after the batch is already skipped while a
  recording is live.
- **Resumed meetings.** The pass re-transcribes the concatenated `audio.mp4`, so the
  timeline stays consistent.
