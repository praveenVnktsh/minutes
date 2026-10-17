# Enhanced notes written live, from the live notes

Date: 2026-10-17. Branch: `enhance/live-enhanced-notes`. Mac-only.
Builds on [the live summary timeline](2026-10-16-live-summary-timeline-design.md) and
[live notes on the minimum model](2026-10-17-live-notes-min-model.md). Supersedes #104,
which wrote the Enhanced notes from the live notes only after the meeting, on the summary
model; its transcript and notes parsing, links, follow-up evidence and fallbacks are reused
(`summary/live_outline.rs`).

## Problems

1. The Enhanced pane stayed empty during the meeting.
2. After every meeting the summary model (Qwen 3.5 4B) read the whole transcript: 1–3 min at
   full GPU, and the fans spun.
3. The UI had to stay simple: Raw and Enhanced, nothing new to manage.

## Behaviour

- **During the meeting.** When the notetaker starts a new topic, the topic that just closed
  gets its Enhanced section, written in the background on the live notes model (Gemma 4 E2B
  once downloaded, else the summary model), one section at a time, from that topic's live
  notes, its slice of the live transcript, and the user's Raw notes typed during it. The
  section is stored at once (`meeting_enhanced_sections`), so a crash keeps it. The meeting
  page's Enhanced view fills in as topics close; the topic under way shows "Writing…".
- **Sections.** `## Topic`; the user's own notes from that time first, word for word; then at
  most 4 bullets, facts first, each ending in a `[mm:ss]` link to the transcript line it
  comes from (links are checked against the topic's time and snapped to a real line; a
  bullet without one links to the topic's first line). Narration of who spoke is removed,
  and the prompt forbids naming who said or did something unless the line itself shows it.
  A topic with at most 2 notes is not sent to the model: its notes go under "Also
  discussed". A topic the meeting returns to is one section.
- **At stop.** The summary job waits for the live worker (its last window and the section in
  flight, at most 45 s), writes only what is missing (normally the last topic), asks for a
  title, and puts the rest together without a model: "Also discussed", "Decisions" (the
  decision notes), "Action items" (`Owner — task`, kept only when a transcript line at the
  note's time shows someone taking it on; the owner is who said that line) and "Open
  questions" (reported by the section calls). No pass over the whole transcript.
- **The final transcript pass.** When it takes over, the stop no longer waits for it: notes
  built from live notes start at once (`has_live_enhanced_notes`). When the pass finishes,
  nothing is regenerated; Re-enhance writes every section again from the improved transcript.
- **Re-enhance** (a meeting with finished notes) rewrites every section the same way.
- **The old path** (the whole transcript on the summary model) still writes the notes when
  the meeting has no live notes, notes without topics, a transcript without times, fewer
  than one note per 5 minutes, a saved custom summary prompt, or non-English notes.
- **Settings.** None new for this: it is the behaviour. Generation is deterministic (the live
  notes' temperature 0.2 and fixed seed, 448 output tokens, 8 lines at most).

## Raw notes: "Write live notes into my notes" (Beta, off by default)

Live notes are added to the Raw notes under their topic heading, muted with a small "AI"
tag, after the user has paused typing for 2 s, only ever below the cursor: under the topic's
last AI line when that is below the cursor, else at the end (with the heading again). User
lines are never touched; editing an AI line makes it the user's; a deleted AI line does not
come back. The insertion is its own undo step. Provenance is stored in the notes document
(`aiBlocks`, `aiEntries`) and AI lines are left out of the user's notes everywhere they are
read (the summary, the stop's "has notes" check, the sections).

## Evaluation

`summary::enhanced_eval` (ignored test) replays a saved live-notes run over a replay's live
transcript: the old summary, then the new path (sections as topics close, then the stop),
twice for determinism. Its report quotes meeting content and stays on the machine.
