# Attendee context: people from Linear, Notion and calendar invites

Date: 2026-11-26. Status: foundation merged; four parallel workstreams.

## Goal

Know who is in a meeting (names, roles, teams, active projects) and use it to
make the transcript, speaker labels, notes and Copilot more accurate. People
data is fetched from Linear and Notion with the user's own keys and stored
locally. **No meeting content is ever sent to Linear or Notion.**

## Foundation (done, in `main`)

- Migration `20261126000000_add_people.sql`: `people`, `person_sources`,
  `meeting_attendees` (in `MEETING_TABLES`, so deleting a meeting drops its
  attendance; people outlive meetings).
- Rust `src/people/mod.rs`:
  - `upsert_from_source(pool, &PersonInput)`: match by (source, external id),
    then by lowercased email, else create. Empty incoming fields never clear
    stored ones; fields the user edited (`locked_fields`) are never
    overwritten; aliases are unioned; only that source's projects are
    replaced.
  - `create_manual_person`, `update_person` (locks the fields it sets; one
    `is_me`), `delete_person`, `forget_source(pool, source)` (on disconnect).
  - `add_meeting_attendee(pool, meeting, person, source)`,
    `remove_meeting_attendee`, `meeting_attendees`.
  - `meeting_context(pool, meeting) -> MeetingContext { attendees, me,
    vocabulary }`; `vocabulary_for(&[Person])` (names, aliases, teams, project
    titles; deduped; max 200).
  - `render_context_block(&ctx, max_chars)`: one prompt line per attendee.
  - `resolve_name(&ctx, "Dev")`: unique match on full name, first name or
    alias.
- Commands: `people_list/create/update/delete`,
  `meeting_attendees_get/add/remove`, `meeting_people_context`.
- Frontend client: `frontend/src/lib/people.ts` (`peopleApi`, types).

Sources constants: `linear`, `notion`, `calendar`, `manual`.

## Workstream A: Linear and Notion sync + People settings

- Settings → new "People" section:
  - Connect Linear (personal API key) and Notion (internal integration token
    plus a chosen team-directory database).
  - "Sync now", last-synced time, Disconnect (calls `forget_source`).
  - A people list with search; edit role, team, aliases, notes; mark "This is
    me"; add a person by hand; delete.
- Keys are stored the same way the app stores provider API keys.
- Linear (GraphQL `https://api.linear.app/graphql`):
  - Active users: `id` → external_id, `name`/`displayName` (alias), `email`.
  - Team names → `team`, joined if several.
  - Projects the user leads or is a member of, and the titles of in-progress
    assigned issues (cap 5) → `projects` with `source: linear`.
- Notion:
  - Query the chosen database. Map properties by type and name, with
    overridable defaults:
    - title → name
    - email-type property or one named "Email" → email
    - "Role"/"Title"/"Position" → role
    - "Team"/"Department" → team
    - relation/multi-select "Projects" → projects
  - page id → external_id.
- Sync runs on demand and once a day after app start. Errors are shown per
  source and never block the app.

## Workstream B: Attendees from calendar invites + meeting UI

- `calendar.rs`: parse ATTENDEE (CN, mailto) and ORGANIZER into
  `CalendarEventOut.attendees: Vec<{name, email}>`.
- When a recording's meeting is created, find the calendar event overlapping
  now (prefer the one whose title matches, else the one with the latest
  start). Upsert each attendee as a person (`source: calendar`, external id =
  lowercased email), then `add_meeting_attendee(.., "calendar")`.
  - Skip resource rooms (CUTYPE=ROOM/RESOURCE).
  - Mark the organizer or "me" if their email matches the configured "my
    email" setting.
- Meeting page header: attendee chips (name, role on hover) with add (people
  picker / quick-create) and remove.

## Workstream C: Transcript accuracy (vocabulary boosting)

- The final pass (Parakeet TDT v2) gets the meeting's
  `MeetingContext.vocabulary`. Use FluidAudio CTC vocabulary boosting
  ("Approach 2": separate Parakeet CTC 110M encoder,
  `Documentation/ASR/CustomVocabulary.md`). The CTC model is a pinned
  download like the other speech models, and only loaded when the vocabulary
  is non-empty.
- `speech-helper` transcribe op accepts optional vocabulary terms.
- Live transcription (Nemotron streaming) has no boosting in FluidAudio
  0.18; out of scope.
- Measure on a sample where a name is misrecognized; a test fixture proves
  terms reach the helper.

## Workstream D: Using the context

- Copilot: prepend `render_context_block` (≤ 1,500 chars) to its system
  context for the meeting.
- Speakers: in the speaker rename UI, offer the meeting's attendees first.
  After diarization:
  - Label the mic cluster as `me`'s name when `me` is set.
  - In a two-person meeting with exactly one other attendee, label the
    other cluster with that attendee.
- Wrap-up action item owners: map a bare first name or alias to the
  attendee's full name with `resolve_name` (only on a unique match). Never
  invent owners.
- The fine-tuned live-notes and wrap-up prompts are NOT changed (they're
  trained on a fixed format).
- Meeting title generation may use attendee names only if its prompt is a
  general model's.

## Non-goals

Sending meeting content to Linear/Notion; writing back to them; live-stream
vocabulary boosting; Google/Microsoft people APIs.
