# One notes document

Date: 2026-10-17. Branch: `enhance/one-notes-doc`. Mac-only. Replaces the Raw/Enhanced split
and the Live summary panel.

## Behaviour

- **The meeting page** (recording and after): left, one notes document (the user's lines and the
  AI's, in the same BlockNote doc); right, the transcript with Copilot (recording) or Chat
  (after) below it. No Raw/Enhanced control, no Live summary section, no Chapters.
- **AI notes** are always written into the doc while recording (the former Beta "Write live
  notes into my notes" is now the behaviour; its rules from
  [live enhanced notes](2026-10-17-live-enhanced-notes.md) are unchanged). The old setting is
  ignored and dropped (`parseBetaFeatures`).
- **Show AI notes** (one switch above the notes, on by default, remembered in `localStorage`):
  off hides AI blocks with CSS and deletes nothing. Copy notes and Export follow it.
- **When the meeting ends**, the post-meeting notes editor completes a doc the AI wrote into,
  once (`completeAiNotes`, marker `wrap-up` in `aiEntries`): the live notes the editor had not
  written yet, then "Decisions" and "Action items" (`Owner — task`) from the live notes'
  decisions and actions, as AI blocks. No model is used.
- **Old meetings** (no AI blocks in the doc): with Show AI notes on, the stored Enhanced notes
  show read-only below the notes. Stored data is never rewritten. The ⋯ menu offers "Write AI
  notes" / "Rewrite AI notes" for these meetings only (the old summary path).
- **The #109 section writer** keeps running unchanged. It writes the stored summary and the
  title at stop without a pass over the whole transcript. Turning it off would move all section
  writing to the stop. Replacing AI blocks with its sections during the meeting would change
  lines above the cursor. Its view is no longer shown.
