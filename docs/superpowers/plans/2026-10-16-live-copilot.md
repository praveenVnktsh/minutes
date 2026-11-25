# Live Copilot implementation plan

Spec: `docs/superpowers/specs/2026-10-16-live-copilot-design.md`. Branch `enhance/live-copilot`.

Hands off (another branch owns them): `summary/processor.rs`, summary budget/chunking, Ollama
`num_ctx`/budget logic, speaker identity/diarization, `transcription_queue` and the final pass.

## Order

1. **llama-helper protocol** (TDD on pure parts)
   - Stdin reader thread; `cancel {id}` recorded in an atomic, never answered.
   - `id`, `stream`, `cache_prompt`, `cache_prefix_chars`, `cache_context_size` on `generate`.
   - Token lines + final response with `id`, `reused_tokens`, `first_token_ms`,
     `tokens_per_second`, `finish_reason: cancelled`.
   - Persistent cached context + checkpoint (state_seq get/set). Non-cached path unchanged.
   - Tests: request parsing, stop-token holdback for streamed text, common-prefix/stable
     boundary maths, checkpoint selection, cancel id matching.
2. **Sidecar priority queue** (TDD with the fake helper script)
   - `ExchangeQueue` with Background/Normal/Interactive; interactive waiters jump the queue.
   - Pre-emption by cancel line; `Preempted` error; cancel-on-abandon for non-stop policies.
   - Streaming round trip that forwards token lines and returns the final line.
   - Tests: interactive jumps queued background, pre-empts in-flight background, never
     pre-empts normal, replies never cross, abandoned copilot request does not kill helper.
3. **client.rs**: `generate_with_builtin_stream` (ids, cache fields, token callback, stats).
   `OnAbandon::Yield` for the timeline; worker treats `Preempted` as a quiet requeue.
4. **copilot pure modules** (TDD): `detector`, `context`, `prompt`, `stream_parse`.
5. **copilot streaming** for Ollama / Claude / OpenAI-compatible / built-in.
6. **copilot session + commands + events + persistence** (migration, delete cascade).
7. **Frontend**: `lib/copilot.ts` (types, API, citation parsing, delta reducer; bun tests),
   `CopilotPanel`, transcript "Question for you" tag, workspace pill label, AI chat labels
   and citation links, Settings card.
8. **Verify**: `cargo test -p llama-helper`, `bash scripts/pr-check.sh`, smoke test with the
   local Qwen 3.5 4B GGUF (40-minute synthetic transcript, cold vs warm first-word latency,
   tok/s), local app bundle.
