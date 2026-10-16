# Summary consistency — Phase 1 design

Date: 2026-10-16. Branch: `fix/summary-consistency`.

## Problem

Summaries are "flaky and inconsistent". The causes we verified in code:

| # | Cause | Verified? |
|---|-------|-----------|
| 1 | llama-helper seeds sampling from the current time. Qwen runs at temp 0.5 with presence penalty 0.3, Gemma at temp 1.0. Cloud and Ollama calls send no temperature. | Yes |
| 2 | The Ollama threshold uses the model's *max* context from `/api/show`, but the request goes to `/v1/chat/completions`, which cannot set `num_ctx`. Ollama's own docs say: "The OpenAI API does not have a way of setting the context size". So Ollama runs at its server default and silently drops the start of the prompt. | Yes |
| 3 | Built-in AI has no context budget. The chunk size is `ctx - 300` and ignores the 4096 output tokens, the system prompt and the notes. On overflow the decode fails, and the retry sends the same input again. | Yes |
| 4 | Truncation goes undetected: hitting `max_tokens` counts as success in llama-helper, and nobody checks `finish_reason`, `stop_reason` or `done_reason`. | Yes |
| 5 | Auto-summary sends bare text, while the manual path sends `[mm:ss] [Speaker] text`. When the manual path falls back, it inserts the raw stored timestamp string with no brackets. | Yes. Stored timestamps can be RFC 3339 strings, and they get pasted in unbracketed. |
| 6 | English normalisation runs whenever the detected language is unknown. | Yes |
| 7 | The Gemma template puts the system prompt in its own user turn, then adds a second user turn. | Yes. Qwen BOS: `AddBos::Always` maps to llama.cpp `add_special`, which only adds BOS when the GGUF says `add_bos_token`. Qwen's GGUF says false and Gemma's says true, and neither template writes a literal `<bos>`, so there is no double BOS. No change. |
| 8 | "You"/"Others" are never explained, and the empty-notes case gets one vague line. | Yes |
| 9 | The template picker is dead (`let _ = (template_id, template);`). | Yes |

## Decisions

### Deterministic generation
- **llama-helper** takes an optional `seed` and defaults to the fixed seed 42, so the same prompt, model and settings give the same output.
- **Built-in presets:**
  - Qwen 3.5: temp 0.2, top_k 20, top_p 0.8, presence 0, plus a mild repeat penalty (1.05 over the last 256 tokens) to guard against low-temperature loops.
  - Gemma 3: temp 0.2, top_k 64, top_p 0.95, no penalties.
- **OpenAI / Groq / OpenRouter:** `temperature: 0.2` and `seed: 42`.
- **Custom OpenAI:** the user's settings win, and temperature defaults to 0.2 when unset.
- **Fallback:** if a provider rejects a sampling field with a 400 (OpenAI reasoning models reject `temperature`), we retry once without the defaulted sampling fields.
- **Claude:** no temperature. Current Claude models reject `temperature` with a 400.
- **Ollama:** `options.temperature = 0.2`, `options.seed = 42`.

### Ollama: native `/api/chat`
- Ollama moves to `POST /api/chat` with `stream: false`, `think: false`, and `options { num_ctx, num_predict, temperature, seed }`.
- `num_ctx = min(model max context, 16384)`, so a 128k model cannot exhaust memory. The chunk budget uses the same number.
- Truncation is detected through `done_reason == "length"`.
- A 400 that rejects `think` is retried once without it. This replaces the old `reasoning_effort` retry.

### Context budget (`summary/budget.rs`, pure)
- `ContextBudget { n_ctx, max_output, margin }`
  - `transcript_budget(overhead) = n_ctx - max_output - overhead - margin`
  - `margin = max(256, n_ctx / 32)`
- **Overhead** is the size of the formatted final prompt (system, notes and wrappers) with an empty transcript.
- **Built-in AI** counts with the real tokenizer through a new llama-helper `tokenize` request. Chunk sizes in characters come from the measured chars-per-token ratio of the transcript.
- **Ollama** has no tokenize endpoint, so we estimate conservatively per script: ASCII ≈ 1 token per 3 chars, other characters 1 token each.
- **Cloud providers** stay single-pass.
- **Reduce step:** chunk summaries are combined hierarchically, in groups that fit the budget.
- **Overflow retry:** llama-helper rejects `prompt + max_tokens > n_ctx` up front with `error_kind: "context_overflow"`. A context-overflow error from any provider re-splits that chunk at 60% of its size and continues. It does not resend the same input.

### Truncation
- `LlmCompletion` gains `truncated`.
- A truncated built-in or Ollama call is retried once with double the output budget, when that still fits in the context. Otherwise it fails with "The summary was cut off …".
- A truncated output is never saved as a completed summary.

### Transcript format
- `frontend/src/lib/summaryTranscript.ts` → `formatSummaryTranscript(transcripts)` is the one formatter for both paths: `[mm:ss] [You|Others|Speaker] text`.
- When `audio_start_time` is missing, the line has no timestamp (we never paste raw wall-clock or ISO strings).
- `autoSummary.ts` changes only on its `text:` line.

### Language gate
- Normalisation runs only when the transcript is confidently detected as a known non-English language. Unknown or low-confidence returns English as-is.

### Prompts
- Every summary prompt defines `[You]` (the person recording) and `[Others]` (remote participants, possibly several people).
- **Empty notes** get a dedicated system prompt with these sections, omitting any that are empty, plus one short example:
  - an H1 title of at most 6 words
  - `## Summary` (2–3 sentences)
  - `## Decisions`
  - `## Action items` (owner, plus the due date if stated)
  - `## Open questions`
- **With notes**, the "enrich my notes" prompt is unchanged.
- **User override:** a saved prompt override still applies whether or not notes exist.

### Template picker
- `SUMMARY_TEMPLATE_PICKER_ENABLED = false` in `frontend/src/lib/summaryFeatures.ts` hides the dropdown and skips fetching templates.
- The backend template APIs and JSON files stay.

## Out of scope
Structured JSON, GBNF, template wiring, a separate title call, model swaps, an eval harness.
