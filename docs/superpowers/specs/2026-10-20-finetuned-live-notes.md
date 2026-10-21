# Fine-tuned live notes and tidy pass

Date: 2026-10-20. Branch: `praveenVnktsh/finetuned-live-notes`. Mac-only. Builds on
[live notes min model](2026-10-17-live-notes-min-model.md) and
[one notes document](2026-10-17-one-notes-document.md).

## Behaviour

- **Live notes on Gemma 4 E2B** run with a LoRA adapter trained for the job
  (`live_summary/finetuned.rs`). Each pause-aligned window the model reads the notes so far (ids,
  the user's lines as `[you]`), the topic and the window's transcript, and answers with compact
  ops: `T=` topic, `+` note, `>` sub-bullet, `~` edit, `-` drop, `.` nothing. The first token is
  gated: `.` only when P(`.`) >= 0.9, else `.` is banned for that token. A sub-bullet that cannot
  attach becomes a note; near-duplicates of existing or user notes are dropped. Action items stay in the
  timeline (the model keeps them in its notes, as trained, and the wrap-up reads them) but are
  never written into the notes document (`pendingGroups`); the wrap-up lists them when the meeting
  ends. Without the
  adapter, live notes use the prompt they used before.
- **The tidy pass** (`summary/tidy.rs`) runs once when a meeting ends, with the second adapter on
  the same E2B: the live notes and the full transcript in, final notes out (Summary, Decisions,
  Action items, 3-7 subject sections, Open questions). The answer is checked (sections, length
  cap with 15% slack, grounded numbers, action format); a stray `- None` next to real bullets is
  dropped, and an action owner the meeting never names is left out (the adapter otherwise invents
  AMI roles such as "Marketing expert"). A failed check, a cut-off answer or a prompt over ~18.5k tokens keeps the wrap-up from
  the live notes. The call skips the live notes prompt cache (`one_off`), so it gets its own
  20k context instead of the cache's 8k.
- **The notes document** shows the tidy pass's wrap-up when it is stored, else the live notes'
  Decisions and Action items, upgraded when `tidy-notes-ready` arrives or on the next open.
- **Adapters** (`summary_engine/adapters.rs`): `minutes-notes-lora.gguf` and
  `minutes-tidy-lora.gguf` in the summary models folder, downloaded from Hugging Face
  (`praveenvnktsh00/minutes-notes-gemma4-e2b`, `.../minutes-tidy-gemma4-e2b`, SHA-256 pinned)
  after the E2B download and at startup when E2B is on disk. `MEETILY_NOTES_LORA` /
  `MEETILY_TIDY_LORA` point at development copies.
- **llama-helper** applies a LoRA per request (`lora_path`, cached per path) and the gate
  (`gate_token`, `gate_threshold`; the response carries `gate_prob`). A request with another
  context size keeps the loaded model when the GPU offload is the same.

## Results

On 10 held-out AMI meetings: live notes closed loop, 0.85 of ops pass the validator, 5.1 topics
per meeting; tidy pass 8/10 pass the checker (base E2B 0/10). Replay of a 27.8 min recording: 37
live notes, live transcription RTF max 0.81, Copilot first words median 3.0 s. A 36 min meeting's
tidy pass (11.9k prompt tokens) takes 46 s on an M5.

## Checks

- `cargo test -p meetily --lib` (finetuned fixtures match the training-side Python byte for byte).
- Ignored: `the_adapter_writes_checked_notes_for_a_whole_meeting` (real helper and adapter on a
  full meeting; see its doc comment).
