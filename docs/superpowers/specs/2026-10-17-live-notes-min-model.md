# Live notes: the smallest model that does the job, and the cheapest way to run it

Date: 2026-10-17. Branch: `perf/live-notes-min-model`. Mac-only (Apple Silicon).
Builds on [the live summary timeline](2026-10-16-live-summary-timeline-design.md).

## Goal

Two competing objectives for the live notes written while a meeting records:

1. the **minimum viable model**: the smallest local model whose notes are still good enough;
2. **barely any usage**: as little GPU, CPU and memory per meeting-hour as possible, and no
   measurable slowdown of live transcription (Nemotron, on the CPU).

## The minimum-viable bar (defined before measuring)

A config passes when, on **both** replays (`sequoia-networking`, 27.8 min, and `m35`,
35.6 min), it meets every line:

| Metric | Bar |
| --- | --- |
| Notes narrating speakers (shown) | ≤ 5 % |
| Notes over 14 words | 0 |
| Notes with a number not in their transcript | 0 |
| Failed windows (error, truncated or unreadable) | ≤ 2 % |
| Coverage of the reference notes | ≥ 0.75 |
| Precision | ≥ 0.85 |
| Topics | a new chapter every 3–8 min on average |
| Actions | every action has an owner, and the reference's real actions are found |
| Repeats of the last 8 notes | ≤ 1 |

**The usage budget ("barely any usage")**, per meeting-hour, mean of 3 runs per replay:

| Resource | Budget |
| --- | --- |
| Model busy (wall time a request holds the model) | ≤ 3 % of the hour (108 s) |
| GPU time of the live notes helper | ≤ 2 % of the hour (72 s) |
| Live transcription (Nemotron) real-time factor | ≤ 2 % slower |
| Peak extra memory (the helper's physical footprint) | ≤ 0.6 GB |

The RTF impact is the back-to-back slowdown (live notes requests with no gap, the worst case)
times the share of the hour the model is busy. Memory is the footprint macOS charges the
process (Activity Monitor's "Memory"); the mapped GGUF weights (2.9 GiB, clean and file-backed)
come on top in RSS and are reported separately.

**Reference.** Current production: Qwen 3.5 4B Q4_K_M, the notetaker JSON prompt with the
topic step, a fresh 32k context per request, llama-cpp-2 0.1.146.

**Coverage**: share of reference notes with a candidate note starting within ±90 s whose
content words agree (Dice ≥ 0.3 over lowercased, lightly stemmed words without stopwords;
the threshold was calibrated by hand on pairs printed by `MEETILY_BENCH_PAIRS=1`).
**Precision**: share of candidate notes that match a reference note that way, or are
supported by the transcript (≥ 60 % of their content words appear within 90 s before to 70 s
after, and every figure is there, as `facts.rs` judges it).

**Cost** per meeting-hour, from the helper process itself: GPU seconds (IOKit's per-process
`accumulatedGPUTime`), CPU seconds (`ps`), wall time the model is busy, peak memory footprint
(`/usr/bin/time -l`), per-window prompt/reused/generated tokens, first-token and generation
milliseconds, and live transcription's real-time factor alone vs. with live notes running
back to back.

## Tooling

`live_summary/bench.rs` (ignored tests, developer only):

- `live_notes_bench`: per (model, runtime config) from a JSON list, drives its own
  llama-helper under `/usr/bin/time -l` over the replays with the worker's loop (real
  scheduler on a simulated clock, real topic step, prompts and parser), writes the notes and
  per-window costs to `<replay>/bench/<label>.json` (meeting text: local only) and one
  aggregate row per run to a table.
- `live_notes_rescore`: rescores saved runs against the current reference and metrics.
- `live_notes_rtf`: Nemotron's RTF alone and with a config's requests back to back.

## Results

Apple M5, 24 GB, macOS, on a busy machine (other workloads ran throughout: latencies and
RTF are noisy, GPU seconds are per-process and robust). Per meeting-hour figures are
scaled from each replay. Aggregates only: the notes stay on the machine.

### Minimum viable: Gemma 4 E2B Q4_K_M

Gemma 4 E2B (unsloth Q4_K_M, 2.9 GiB file), the compact prompt with the topic step, the
system prompt's state reused between windows (8k cached context), flash attention left to
llama.cpp (on for Metal), f16 KV, 2 generation threads, no grammar, llama-cpp-2 0.1.160.
Two runs gave identical notes.

| | Baseline (4B, JSON, old runtime) | E2B, final | Bar |
| --- | --- | --- | --- |
| Narration (shown) | 0 % / 9 % | 0 % / 2 % | ≤ 5 % |
| Notes over 14 words | 0 / 0 | 0 / 0 | 0 |
| Unsupported numbers | 0 / 0 | 0 / 0 | 0 |
| Failed windows | 0 % / 0 % | 0 % / 0 % | ≤ 2 % |
| Coverage | (reference) | **0.64** / 0.76 | ≥ 0.75 |
| Precision | (reference) | 0.95 / 0.93 | ≥ 0.85 |
| Minutes per chapter | 2.8 / 5.1 | 4.0 / 5.1 | 3–8 |
| Actions (found / real / owned) | 2/2/2 · 0/0/0 | 3/3/3 · 3/2/3 | owned |
| Notes | 28 / 34 | 56 / 68 | |
| GPU, % of the hour | 5.0 % / 4.8 % | **2.0 % / 1.8 %** | |
| Model busy, % of the hour | 9.7 % / 9.8 % | 2.9 % / 2.6 % | |
| CPU, % of a core-hour | 0.6 % / 0.6 % | 0.3 % / 0.3 % | |
| Window latency mean / max | 5.8 / 10.4 s | 1.7 / 2.8 s | |
| Prompt / reused / generated tokens | 1572 / 0 / 52 | 965 / 434 / 59 | |
| Peak footprint (RSS incl. mapped weights) | 1266 MB (3.8 GB) | 395 MB (3.2 GB) | |

(sequoia / m35.) It misses the coverage bar on sequoia. That bar turned out to sit at the
noise floor: **the reference model itself**, re-run on the new runtime, scores 0.82 / 0.71,
and E2B variants that differ only in runtime details range 0.57–0.82 because window
boundaries move with latency. E2B writes about two notes a window where the 4B writes one,
so half its notes match no reference note (yet 93–95 % are reference-matched or supported
by the transcript).

Hand review of the sequoia 06:30–13:30 excerpt (15 E2B notes vs 7 reference notes): 11
correct and specific, 3 correct but vague, 1 wrong attribution (an offer by the other side
filed as the user's action), 0 invented facts or numbers, 0 narration. Its extra notes are mostly
facts and figures the reference skipped, each checked against the transcript. Two m35
actions were two-word fragments; notes under three words are now dropped.

### Candidates (production flow: JSON prompt + topic step, new runtime)

| Model | Coverage | Precision | Notes / topics | GPU % of hour | Verdict |
| --- | --- | --- | --- | --- | --- |
| Qwen 3.5 0.8B Q4_K_M / Q8_0 | 0.25 / 0.25–0.32 | 0.92–0.97 | narration 41–86 % raw, 40 % shown | 1.3–1.9 | fail |
| Gemma 3 1B Q4_K_M / Q8_0 | 0.00–0.04 | – | 2–6 notes per meeting | 1.1–1.6 | fail |
| Qwen 3.5 2B Q4_K_M | 0.29 / 0.32 | 0.89 / 0.87 | sparse, no actions | 1.7 | fail |
| Gemma 4 E2B Q4_K_M | 0.50 / 0.53 | 0.98 / 1.00 | good, 2 notes/window | 4.9 / 4.1 | quality ok, JSON too costly |
| Gemma 4 E4B Q4_K_M | 0.50 / 0.50 | 0.92 / 0.83 | | 4.1 / 3.8 | no better than E2B |
| Qwen 3.5 4B Q4_K_M | 0.82 / 0.71 | 1.00 / 0.94 | | 3.6 / 3.5 | the reference |

With the compact prompt and topic step (the final flow): Qwen 2B fails (46 % narration on
m35, coverage 0.36); E4B loses topics on m35 (one chapter) at 2.5–2.7 % GPU; the 4B reaches
0.75 / 0.79 coverage but 0.72 precision on m35 at 2.0–2.7 % GPU. E2B Q8_0 and Google's QAT
Q4_0 score like Q4_K_M at more GPU (Q8) or the same (QAT).

### What each change bought (E2B unless noted)

| Change | Effect |
| --- | --- |
| llama-cpp-2 0.1.146 → 0.1.160, all layers on Metal | 4B, same prompt: GPU 5.0 → 3.6 % of the hour, first token 2.8 → 2.3 s. E2B: CPU 14 % → 1.5 % of a core-hour (the old size heuristic left two of its 35 layers on the CPU), window 6.8 → 4.6 s |
| Compact answer (`mm:ss\|kind\|owner\|note`) and a system prompt a third the size | generated tokens 157 → 59 per window; with the next row, GPU 4.9 → 2.0 % |
| Stable prefix reused between windows (8k cached context instead of a fresh 32k one) | first token 713 → 350 ms, GPU 2.2 → 2.0 %, footprint 845 → 395 MB |
| Topic step kept (the folded "topic: same" line) | folding it into the notes call failed for every model: they answer "same" for the whole meeting (1–2 chapters), or, naming the topic per note, start 43 chapters. The step costs ~15 calls an hour |
| Flash attention | already on by default on Metal; forcing it off costs ~10 % |
| KV cache q8_0 | −30 MB footprint, no speed change: not worth a separate cache key |
| 4 generation threads instead of 2 | no gain, more CPU |
| GBNF grammar (now supported by llama-helper) | no failures to prevent, 10–25 % slower generation: off |
| 90 s cadence | −20 % GPU, but coverage 0.54 / 0.56 and fewer chapters: kept 60 s |
| Fixed: llama-helper accepted each sampled token twice | doubled the Qwen repeat penalty's count; crashed any grammar |

### Live transcription (Nemotron) while live notes run

RTF of 90 s of two-source audio, alone 0.775 on the busy machine. With live notes
requests back to back (the worst case): 4B baseline 1.02 (×1.31, 21 requests); E2B 1.15
(×1.49, 72 requests). In a meeting the model is busy 9.8 % (4B) vs 2.6–2.9 % (E2B) of the
time, so the expected slowdown is about +3 % of RTF for the baseline and +1.4 % for E2B.

### Runtime

A faster Mac-only runtime (an MLX sidecar, LiteRT-LM for Gemma 4) was not tried: at
1.4–1.7 s and 2 % of the GPU per meeting-hour, the remaining cost is mostly generation of
~60 tokens, which those runtimes would shave by a fraction, against a second inference
stack to ship and maintain.

## Closing the goal

Branch `perf/live-notes-goal-close`. Same machine (Apple M5, 24 GB), still not quiet (another
project's test suite and builds ran: load average 5–20 during the runs), so latencies and RTF
are noisy; token counts are exact and GPU seconds varied ±5 % between identical runs.

### Noise: the reference against itself

The reference is now the production 4B config on the current runtime (JSON prompt, topic step,
fresh 32k context, llama-cpp-2 0.1.160), run 3× (seeds: production, 1, 2; with the same seed
two runs are byte-identical, so the seed is what varies). Coverage of run *k* against each
other run, and of each E2B run against each 4B run (mean [min–max] over the pairs):

| | sequoia | m35 |
| --- | --- | --- |
| 4B vs 4B (self-agreement, 6 pairs) | 0.79 [0.71–0.86] | 0.75 [0.71–0.79] |
| E2B as shipped in #105 vs 4B (9 pairs) | 0.75 [0.61–0.89] | 0.63 [0.53–0.72] |
| E2B final vs 4B (9 pairs) | **0.84 [0.74–0.93]** | **0.83 [0.76–0.89]** |
| precision, E2B final (9 pairs) | 0.97 [0.95–1.00] | 0.91 [0.89–0.93] |

The 4B agrees with itself at 0.75–0.79: the 0.75 bar is at the reference's own noise floor,
so a candidate has to cover the reference about as well as the reference covers itself. The
shipped E2B did not (0.63 on m35). The final E2B does on both replays, in every one of its 3
runs (per-run means 0.82–0.85).

Matcher review: the near misses (Dice 0.15–0.3) printed for m35 were different facts in all but
one case (a same-fact pair at 0.29), and every unmatched reference action was found in other
words (below). The matcher is, if anything, strict; it was left unchanged.

### Final E2B against every bar (3 runs per replay)

| Metric | sequoia | m35 | Bar | |
| --- | --- | --- | --- | --- |
| Narration (shown) | 0–1 % | 1–3 % | ≤ 5 % | pass |
| Notes over 14 words (longest) | 0 (11) | 0 (12) | 0 | pass |
| Unsupported numbers | 0 | 0 | 0 | pass |
| Failed windows | 0 % | 0 % | ≤ 2 % | pass |
| Coverage vs the 4B runs | 0.84 ± 0.10 | 0.83 ± 0.07 | ≥ 0.75 | pass |
| Precision | 0.97 ± 0.03 | 0.91 ± 0.02 | ≥ 0.85 | pass |
| Minutes per chapter | 4.0–5.6 | 5.1 | 3–8 | pass |
| Actions owned / reference actions found | all / 2 of 2 | all / 1 of 1 | all | pass |
| Repeats of the last 8 notes | 0 | 0 | ≤ 1 | pass |
| Notes | 83 | 100 | | (4B: 28–31 / 34–36) |

(± is half the min–max spread.) Reference actions: Dice found 1–2 of the 4B's 2 on sequoia;
by hand both are there in all 3 runs, as actions owned by Others, in other words than the
reference's (both are offers of introductions). E2B also files 2–3 offers per run that the
commitment check does not confirm.

### Usage against the budget (mean of 3 runs per replay)

| | sequoia | m35 | Budget | |
| --- | --- | --- | --- | --- |
| Model busy, % of the hour | 2.6 % (2.5–2.7) | 2.2 % (2.2–2.3) | ≤ 3 % | pass |
| GPU, % of the hour | 1.94 % (1.9–2.0) | 1.69 % (1.7) | ≤ 2 % | pass, 3 % margin |
| CPU, % of a core-hour | 0.2 % | 0.2 % | | |
| Live transcription RTF impact | ≤ 1.5 % | ≤ 1.2 % | ≤ 2 % | pass |
| Peak footprint | 396 MB | 396 MB | ≤ 0.6 GB | pass |
| RSS incl. mapped weights | 3.2–3.4 GB | 3.3 GB | (weights, not budgeted) | |
| Generated / new prompt tokens per window | 61 / 544 | 56 / 396 | | |
| Window latency mean / max | 1.6 / 2.2 s | 1.4 / 2.0 s | | |

RTF: Nemotron alone 0.49–0.70 on the busy machine; with E2B requests back to back ×0.77–×1.55
over 3 runs (the busy machine swamps it); the worst, ×1.55, at 2.6 % busy is +1.5 %.

The floor: generation is ~75 % of the GPU time, and generated tokens are set by the notes
written (three a window on dense speech, where coverage needs them, below). What could be cut
without losing notes was cut; what is left is ~60 tokens a window.

### What raised coverage, and what it cost

| Attempt (E2B, 1–3 seeds) | Coverage seq / m35 | Precision | GPU % seq | Kept |
| --- | --- | --- | --- | --- |
| #105 as shipped | 0.75 / 0.63 | 0.95 / 0.91 | 1.9 | |
| + parser: a speaker label written as a fifth column (`04:12\|point\|-\|Others\|…`) was kept as the note's first word, in 64 of 67 notes on m35; topic "none yet" taken as a title | 0.77 / 0.62 | 0.93 / 0.90 | 2.0 | yes |
| "cover every distinct new fact, decision or action, one note each", 2 notes | 0.73 / 0.68 | 0.98 / 0.92 | 2.0 | |
| same, 3 notes a window | 0.83 / 0.86 | 0.97 / 0.90 | 2.2–2.3 | yes |
| notes without times (placed by their words) | 0.69 / 0.66 | 0.96 / 0.90 | 1.7 | no: fewer, vaguer notes |
| no owner column on points | 0.76 / 0.82 | 0.97 / 0.89 | 2.2–2.4 | no: longer notes |
| "terse, 5 to 9 words, never more than 12" | 0.84 / 0.82 | 0.97 / 0.93 | 2.0–2.1 | yes |
| "4 to 8 words" | 0.83 / 0.80 | 0.95 / 0.92 | 2.1–2.3 | no: same length |
| 75 s / 90 s cadence | 0.64 / 0.68, 0.79 / 0.74 | | 1.9–2.1 | no: and 9–12 min chapters |
| no carried-over context / 45 words of it | 0.84 / 0.79, 0.79 / 0.83 | 0.98 / 0.87, 0.98 / 0.89 | 1.9–2.1 | no: precision for ~1 % |
| stop generating after the topic line and the notes kept (`max_lines`) | same notes | same | **1.9–2.0** | yes |

Dropping each window's third note after the fact costs 0.13–0.19 coverage: E2B does not
reliably put the reference's fact first, so it needs the third chance. It wrote 3.6 lines a
window where 3 are kept; llama-helper now stops at the line limit (`max_lines`) instead of
generating notes the parser drops: 68 → 61 tokens a window, the same notes.

## Shipping

- **Catalogue.** `gemma4:e2b` ("Gemma 4 E2B (Live notes)"), Gemma 4 chat template (a real
  system turn: `<|turn>system … <turn|>`). Every built-in model carries its SHA-256, checked
  after each download, and its URL names the Hugging Face commit that hash belongs to
  (`resolve/<40-hex revision>/…`, never `resolve/main`), so a re-upload upstream cannot break
  downloads.
- **Download opt-in, use by default.** Nothing downloads without asking. Onboarding shows the
  live notes model as an optional step with a checkbox, ticked by default only on Macs with
  16 GB or more; when ticked, the download starts after Parakeet and the summary model are on
  disk (the backend waits for them, across reloads and restarts). Once the file is on disk,
  live notes use it by default on Macs with 16 GB or more: no stored choice (`liveNotesModel`
  absent) means the default, so existing users who never picked get it too. Settings →
  Summary → *Live notes model* shows the model in use and offers "Same as summary model" or
  "Gemma 4 E2B (faster, lighter)", with download, progress and remove; picking either is
  stored and wins over the default. Removing the file is not a choice: live notes fall back
  to the summary model until it is downloaded again. An explicit Gemma choice still works on
  12 GB; 8 GB Macs never run two models.
- **Model choice.** With Built-in AI as the summary provider and Gemma 4 E2B in use (chosen or
  by default) and downloaded, live notes run on it, on **their own llama-helper process** with the compact,
  prefix-cached path, so Copilot (on the summary model) and live notes never reload each
  other's model. The cost is a second resident model (~0.4 GB of footprint plus its mapped
  weights), so Macs with 8 GB or less keep live notes on the summary model.
- **Everyone else is unchanged.** On the summary model (not opted in, not downloaded, 8 GB
  Macs) live notes keep the JSON prompt in a fresh context on the shared helper, as before:
  on Qwen 3.5 4B the compact prompt lost precision (0.72 on m35, under the 0.85 bar), and
  uncached requests cannot evict Copilot's prefix checkpoints. A summary model that is itself
  E2B gets the compact prompt, uncached on the shared helper.
- **Compact path notes.** Up to three notes a window (four from a merged backlog), one per
  distinct fact, decision or action, 5–9 words; the request stops after the topic line and
  those notes. A speaker label written as an extra column is dropped, an action's owner in
  the wrong column kept.
- **Fragments.** Only on the compact path: points under three words are dropped; actions and
  decisions keep two ("Email Sarah", "Budget approved") unless they point at nothing ("do
  that").
- **Settings changes** restart the main helper at once and the live notes helper only after a
  live recording stops.
- **Metal offload.** Every layer goes on Metal when the weights plus the KV estimate fit the
  safe memory budget; a model over it gets the existing sizing logic's partial offload.
- **Warm vs unload.** The helper stays loaded while windows arrive every minute and exits
  after the usual 5 idle minutes. Reloading per window would cost 1–3 s and page the weights
  back in each time, more than the 1.7 s a window takes.
- **Cloud providers and Ollama** keep the JSON prompt; only the live notes model gets the
  compact one.
