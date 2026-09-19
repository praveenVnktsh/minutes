# Minutes UX audit — 2026-09-18

## Summary

The inconsistency is structural as well as visual: navigation, recording identity, background work, settings, and persistence have multiple owners. A coordinated fix should establish those contracts before changing the screens that display them.

This source audit identifies **21 actionable findings: 9 P1, 11 P2, and 1 P3**. P1 affects core workflows, persistence confidence, or access to recording controls. P2 affects navigation, discoverability, recovery, accessibility, or visual consistency. P3 is verified cleanup debt.

**Evidence boundary:** findings below are traced through reachable React components, hooks, and relevant Rust commands/events. Reproduction steps are acceptance scenarios derived from code, not claims of executed desktop tests. No current screenshots, native interaction tests, or pixel/contrast measurements were collected. The checkout has no `frontend/node_modules`; no build or test run was performed for this documentation-only audit. Existing images under `docs/` were not treated as evidence of the current UI.

Read repository `CLAUDE.md`, all four route entry points, their layouts and main components, recording/configuration/navigation contexts and hooks, notes/export/search code, theme definitions, relevant native event producers, and the existing frontend test inventory. No repository `AGENTS.md` or `STYLEGUIDE.md` was found.

## Current screen map

| Surface | Current implementation | Observed responsibility |
|---|---|---|
| `/` | `app/page.tsx` | Idle hero, recording initiation, error/model dialogs, transcript recovery. Redirects to live workspace during recording. |
| `/meeting-details?id=…` | `app/meeting-details/{page,page-content}.tsx` | Saved meeting, raw/enhanced notes, transcript, chat; also live recording when a query flag is present. |
| `/settings` | `app/settings/page.tsx` | General, Recordings, Transcription, Summary, Integrations, Beta. Selected tab is local component state. |
| `/meeting-prompt` | `app/meeting-prompt/page.tsx` | Separate native meeting-detection window. This is an intentional separate surface. |
| Sidebar | `components/SimpleSidebar.tsx` | Home and Meetings both link to `/`; recent/pinned/archived entries, search, recording, import. |
| Command palette | `components/CommandPalette.tsx` | A separately implemented subset of navigation/actions, with different availability rules. |
| Model editing | Settings, summary-options dialog, chat dialog, recording setup modal | Some shared form implementation, but different state/save/close behavior. Multiple entry points are useful; divergent transaction behavior is the defect. |
| Onboarding | `components/onboarding/OnboardingFlow.tsx` | Replaces main app after an asynchronous startup check. |

Paths in the findings are relative to `frontend/src/` unless prefixed otherwise. Line references describe the audited revision.

## Findings and acceptance contracts

### UX-01 · P2 — Home and Meetings are the same destination

**Evidence:** `components/SimpleSidebar.tsx:185–193` routes both buttons to `/`, but marks Meetings active on `/meeting-details`. `app/page.tsx:223–233` renders an idle hero, not a meeting library.

**Impact:** clicking a primary navigation item does not open what its label promises. There is no main-area meeting library, and the sidebar becomes the only browsing surface.

**Fix contract:** use `/` as the canonical **Meetings** library, with a useful empty state and New meeting action. Remove the duplicate Home item; keep recent entries as shortcuts. A meeting workspace highlights its own entry and the Meetings section consistently. Preserve existing meeting URLs.

### UX-02 · P1 — Active recording UI depends on how the meeting was opened

**Evidence:** `app/meeting-details/page.tsx:349` passes `recording === '1'`; `page-content.tsx:91,413–445` uses that flag plus global recording status to choose live notes/transcript and Stop controls. `SimpleSidebar.tsx:101–104` and `CommandPalette.tsx:135–137` open the same ID without the flag. Conversely, the condition does not compare the displayed ID to the active session ID.

**Scenario:** start recording; open its row in Recent or the command palette. The URL loses `recording=1`, and the saved-meeting branch replaces the live view even while recording continues.

**Fix contract:** derive live mode from authoritative `activeMeetingId` and session state, not URL hints. Sidebar, library, palette, deep links, and Return to recording must produce the same view. Other meetings must never show another session's live notes. Show persistent recording/paused status and a reliable return/stop path while elsewhere in the app.

### UX-03 · P1 — First summary generation is unreachable through the summary UI

**Evidence:** `MeetingWorkspace.tsx:277–307` disables Enhanced when no summary exists and renders raw notes instead of `SummaryPanel`. The Generate empty state exists in `SummaryPanel.tsx:73–79`, behind that gate. `SummaryGeneratorButtonGroup.tsx:83–120` renders model/language controls but no Generate, Stop, or template selector despite accepting those props. Re-enhance only exists when `canShowEnhanced` is true.

**Scenario:** open a transcribed meeting with no summary and auto-summary off, or one whose initial summary failed. There is no visible first-generation action in the summary workflow; chat is a separate workaround.

**Fix contract:** show a primary **Generate summary** action before a summary exists, with model/setup guidance if blocked. During generation show progress and Stop; on failure show the reason and Retry; after completion show Re-enhance. Make template/language controls discoverable where supported. Preserve an existing summary during regeneration.

### UX-04 · P1 — Background transcription failure never reaches its workspace handler

**Evidence:** `page-content.tsx:197–214` uses `window.addEventListener('transcription-queue-error', …)`. Native `frontend/src-tauri/src/audio/transcription_queue.rs:625–655` emits a Tauri event. `components/shared/TranscriptionProgressToast.tsx:24–40` forwards only successful completion into a DOM event. No frontend bridge/listener for the queue-error event was found. Native queue error payloads identify a task but do not include a meeting ID.

**Impact:** deferred transcription can fail while the workspace remains in a processing state without an actionable failure. Simply forwarding the existing event would still leave it insufficiently scoped.

**Fix contract:** one Tauri event adapter maps task and meeting identity, including imports before a meeting ID exists. Persist/display failed and cancelled terminal states; offer a retry when appropriate. A failure must affect only the matching task/meeting and remain discoverable after navigation.

### UX-05 · P1 — Meeting progress has several competing sources of truth

**Evidence:** summary polling lives in `app/meeting-details/page.tsx:258–294`, `page-content.tsx:229–276`, and `SidebarProvider.tsx:226–316`, alongside `useSummaryGeneration` and `AutoSummaryProvider`. Workspace phase initially comes from route flags and advances on any available transcript (`page-content.tsx:83–85,188–195`). `useTranscriptionProgress.ts:80–129` starts from no progress on mount and listens only to future events; it accepts untagged events at line 90.

**Impact:** reopening a processing meeting can lose progress context; old transcript content can be mistaken for completion; timeout/terminal-state behavior differs between watchers. This is a confirmed ownership defect; exact timing-dependent symptoms need native verification.

**Fix contract:** one meeting/task activity owner hydrates a native snapshot, then consumes scoped events. It owns queued, recording, paused, saving, transcribing, summarizing, ready, failed, and cancelled presentation states without collapsing distinct simultaneous jobs into an ambiguous flag. Deduplicate summary orchestration/polling by process ID. Navigation and reload must recover current progress and terminal outcomes. Unknown progress is indeterminate, not an invented percentage.

### UX-06 · P1 — Note persistence has no visible failure state

**Evidence:** `MeetingRawNotesEditor.tsx:12,63–89` maintains a save state but never renders it; failed saves are console warnings and unmount saves swallow errors. Loading errors seed a blank editor at lines 35–38. `LiveNotesPad.tsx:42–60` also logs failed persistence; the workspace uses `bare` mode, which omits its only Saved/Saving indicator (`103–120`). Raw-note writes are debounced but are not serialized or revision-acknowledged.

**Impact:** users cannot tell whether notes are durable. A read failure looks like an empty document. Overlapping edits and in-flight saves have no explicit latest-revision guarantee.

**Fix contract:** display Unsaved / Saving / Saved / Could not save with Retry, driven by acknowledged revision. Serialize/coalesce writes per document, preserve the latest local draft, and flush pending changes across navigation, raw/enhanced switching, stop, export, and summary generation. Failed loads must not silently offer an apparently empty replacement document. Preserve editing and recovery when persistence fails.

### UX-07 · P1 — Renaming drops meeting metadata and never rolls back

**Evidence:** `hooks/meeting-details/useMeetingData.ts:55–64` replaces the matching catalog item with `{ id, title }`, dropping `pinned`, `archived`, `debug`, and `created_at`. `page-content.tsx:114–122` updates that state before persisting; failure only produces a toast.

**Scenario:** rename a pinned, archived, or debug meeting. Its local catalog classification/date changes immediately; a failed rename still appears applied until a reload/refetch.

**Fix contract:** preserve all metadata when patching a title. Commit or optimistically roll back the title consistently in header and catalog. Pending/failed state must be visible without losing the user's edit. Apply the same transaction discipline to pin/archive.

### UX-08 · P1 — Model dialogs change committed state before Save and close on failure

**Evidence:** summary and chat dialogs pass global `setModelConfig` into `ModelSettingsModal` (`SummaryGeneratorButtonGroup.tsx:106–115`, `MeetingAssistantPanel.tsx:120–129`). The form edits that state and updates it again before its non-awaited `onSave` call (`ModelSettingsModal.tsx:637–668`). `page-content.tsx:141–160` catches persistence failure without rejecting, so callers awaiting it still close their dialog. Settings has another save implementation in `SummaryModelSettings.tsx:101–123`.

**Impact:** dismissing a form can leave uncommitted model choices visible to other components. Save can fail yet close the form as if successful. Duplicate clicks have no form-level saving state.

**Fix contract:** one configuration owner provides committed values and an awaited save operation. Reusable model forms edit isolated drafts; Cancel restores the committed view. Disable repeat submission, keep the draft open on failure, and close only after successful persistence. Refresh all entry points from the committed result.

### UX-09 · P1 — Meeting export can omit user notes or report partial success

**Evidence:** `hooks/meeting-details/useCopyOperations.ts:186–225` exports summary and transcripts only, although Export appears in the raw-notes workspace. `fetchAllTranscripts` catches failures and returns `[]` at lines 53–57; export can continue with only the summary and announce success.

**Scenario:** export a notes-only meeting: it says Nothing to export. Export a summarized meeting while transcript loading fails: a partial file can be reported as Meeting exported.

**Fix contract:** define export contents explicitly and include original notes, enhanced notes when available, and the complete transcript. Flush drafts first. Propagate failed reads; do not call a silently incomplete export successful. Await clipboard writes and surface failure consistently, including palette link copying (`CommandPalette.tsx:117–118`).

### UX-10 · P2 — Transcript search reports no matches before searching the whole transcript

**Evidence:** `TranscriptPanel.tsx:225–232` searches only `convertedSegments`; `usePaginatedTranscripts.ts:5` loads 100 rows per page. When no loaded match exists, `TranscriptPanel.tsx:385–389` replaces the virtualized list, which contains the load-more path.

**Scenario:** search for text beyond the first loaded page. No matches is displayed, and the search view offers no way to load later pages.

**Fix contract:** search the complete meeting via native search if its API supports the required scope/offsets; otherwise explicitly load/search all pages with cancellable progress. Jump to actual matches and communicate result scope. Changing the query/meeting must invalidate stale results.

### UX-11 · P2 — Catalog load, empty, search, and failure states are conflated

**Evidence:** `SidebarProvider.tsx:119–122` clears meetings on load failure. Search at lines 205–224 has no request-version check and converts errors into empty results. `SimpleSidebar.tsx:355–356` uses Your meetings will appear here for a zero-result search. Pin/archive failures at lines 106–124 are console-only.

**Impact:** a transient failure looks like missing data; an older search can overwrite a newer one; users receive no reliable action feedback.

**Fix contract:** model initial loading, refresh, empty library, no search results, and failure separately. Preserve known data on refresh failure. Use latest-query ownership, actionable retries, and pending/failed mutation feedback.

### UX-12 · P2 — Palette actions disagree with sidebar availability and labels

**Evidence:** `CommandPalette.tsx:81–104` always says Start recording and always offers Import audio; the sidebar says Return to recording when active and gates import on beta configuration (`SimpleSidebar.tsx:245–260`). `layout.tsx:63–66` does not mount the import dialog when disabled, so the palette action has no visible result. Palette recent meetings at lines 127–143 bypass sidebar archived/debug scoping.

**Fix contract:** reuse action definitions and catalog selectors across library/sidebar/palette. Match labels, feature gates, enabled reasons, and archive/debug visibility. Retain the distinction between Start, Return, and Stop instead of calling them all toggles.

### UX-13 · P1 — Recording actions and failure feedback depend on the mounted route

**Evidence:** only Home mounts `useRecordingStart` and `useModalState` (`app/page.tsx:43–45`). The layout's recording request handler forwards a DOM event (`layout.tsx:124–139`) whose listener is in that Home hook. RecordingControls delegates structured error feedback to `useModalState` (`RecordingControls.tsx:269–293`), which is no longer mounted in the live workspace. Pause/resume failures use browser `alert()` at lines 211 and 229; start failures also differ by entry path. `useModalState.ts:168–191` fails to return its asynchronous download-listener cleanup from the effect.

**Impact:** global requests and recording errors can be ignored off Home; repeated mounts/config changes can accumulate download listeners. Equivalent failures use unrelated UI patterns.

**Fix contract:** mount one recording command/controller and error adapter above routes. All recording entry points share readiness, pending locks, native outcomes, and recovery actions. Route components render state rather than owning native lifecycle listeners. Replace browser alerts with the standard feedback contract. Ensure late subscription setup is disposed correctly.

### UX-14 · P2 — Startup renders the app before deciding whether setup is complete

**Evidence:** `layout.tsx:84–111` initializes `showOnboarding` false, then asynchronously checks status. The main app is therefore mounted first (`269–275`). `CommandPalette` is mounted in both onboarding and normal branches (`277–278`). A status-read failure is interpreted as first-run setup.

**Fix contract:** explicit checking / setup-required / ready / startup-error states. Show a stable boot surface until the status is known, retry a failed check, and enable application commands only when ready. Verify no main-screen flash or setup-time navigation bypass.

### UX-15 · P2 — Meeting prompt hides even when starting fails

**Evidence:** `app/meeting-prompt/page.tsx:42–45` swallows command rejection and hides. Its Start button has no pending state. Native `meeting_prompt.rs` currently forwards a recording request rather than returning an acknowledged recording result.

**Fix contract:** disable duplicate Start requests, acknowledge acceptance/failure through the recording controller, and show a recoverable error or bring the main window's error state into view. Do not imply recording started merely because the prompt disappeared.

### UX-16 · P2 — Theme styling is split across incompatible token systems

**Evidence:** `app/globals.css:79–157` defines shadcn black/white tokens separately from warm Minutes surfaces. Settings uses blue active indicators (`app/settings/page.tsx:100–110`), model Save uses hardcoded blue, and recording/general settings render unconditional light blue/yellow callouts. `frontend/src-tauri/tauri.conf.json:20` fixes native theme to Light while the app defaults dark. `meeting-prompt/page.tsx:13–35` reads theme only on mount.

**Fix contract:** map common UI tokens onto one semantic palette, including selected, focus, info, success, warning, error, recording, and paused states. Use shared control variants and consistent typography/spacing. Synchronize main content, dialogs, editors, native chrome where supported, and the already-mounted prompt when theme changes. Measure text/focus contrast in both themes during visual verification.

### UX-17 · P2 — Compact layout can start with the wrong panels and squeeze notes

**Evidence:** `MeetingWorkspace.tsx:79,177–187` initializes transcript open but only closes it on a transition into compact mode. Mounting a meeting while the shell is already compact does not trigger that transition. The dock remains a right rail (`322–326`) with a minimum 320px width (`24–26`), even when notes cannot retain their stated minimum. The native window starts at 1100px and has no configured minimum width. Settings tabs are a non-wrapping padded row inside a clipping shell.

**Fix contract:** initialize panel visibility from the current breakpoint. At narrow widths show one useful document/panel surface at a time or a real overlay, with a visible return control. Keep settings sections, toolbars, and recording controls reachable at supported sizes and increased text scale. Resizing must preserve sensible user choices.

### UX-18 · P2 — Important custom controls lack consistent keyboard/state semantics

**Evidence:** workspace dividers are pointer-only divs (`MeetingWorkspace.tsx:311–340`); Raw/Enhanced and Transcript/Chat buttons expose no pressed/selected state (`239–297`). Reachable recording model/error dialogs in `app/_components/SettingsModal.tsx` are custom fixed div overlays without dialog focus trapping/restoration. Several text inputs remove outlines without an equivalent focus treatment.

**Fix contract:** use shared accessible dialog and segmented-control primitives; expose state programmatically; implement keyboard resizing or equivalent size controls; establish visible focus, accessible field names, and announced status/error changes. Verify Escape, focus restoration, keyboard-only navigation, and reduced-motion behavior.

### UX-19 · P2 — Settings cannot preserve or deep-link to the relevant section

**Evidence:** `app/settings/page.tsx:34` always initializes General; the route ignores section query/hash state. Error/setup surfaces instead embed copies of settings in dialogs, while full-page settings and quick editors use different save ownership.

**Fix contract:** canonical settings section IDs in the URL, predictable Back behavior, and a helper to open the relevant section from errors. Keep focused quick edits where useful, backed by the same forms/config owner. Preserve or explicitly discard drafts on section changes.

### UX-20 · P2 — Save feedback sometimes describes the wrong operation

**Evidence:** `RecordingSettings.tsx:154–173` uses Device preferences saved with microphone/system details for a shared preferences save function also used by recording policy changes. Preference shortcuts update visible state before persistence without rollback (`PreferenceSettings.tsx:83–100`); other preference/folder failures are console-only. Model forms use explicit Save while many surrounding fields autosave without a consistent saved/pending indication.

**Fix contract:** define autosave versus explicit save per form and label it. Keep routine saved feedback next to the changed control; use toasts for meaningful completed actions. Messages must name the actual change. Preserve drafts or roll back on rejection, and show errors with recovery near the affected control.

### UX-21 · P3 — Obsolete UI implementations remain beside live ones

**Evidence:** source-reference search found isolated `MainNav`, `MessageToast`, `MeetingLiveNotes`, and the `CustomDialog` → `SettingTabs` chain without a reachable app consumer. `SettingTabs` describes a different settings taxonomy. `SettingsModal.tsx` also retains legacy preference/device/language branches with no current show-call found, alongside the reachable model/error branches.

**Fix contract:** verify imports, dynamic references, exports, and tests, then remove unused branches/files after replacement flows are working. These are maintenance duplicates, not evidence that all those screens are currently displayed. Do not remove the separate native meeting-prompt route or useful contextual editors as alleged duplicates.

## Target interaction contract

- **One Meetings destination:** library at `/`, one workspace per meeting ID, one canonical settings route, and the native prompt.
- **One active-session identity:** shared by all entry points; recording controls remain available while browsing.
- **One owner per asynchronous process:** UI consumes scoped snapshots/events, including terminal outcomes and retries.
- **One persistence contract:** drafts remain editable and recoverable, and Saved means the latest revision was acknowledged.
- **One feedback vocabulary:** local pending/saved states for edits; persistent inline error/action for blocked work; progress near the task plus a compact global activity surface when elsewhere; toast for completed discrete actions; dialog for a required focused decision.
- **One visual foundation:** shared semantics for selected, hover, focus, disabled, pending, success, warning, and error in both themes.

## Implementation handoff

Architecture and dependency order: [UX consolidation graph](../plans/2026-09-18-app-ux-consolidation.md). The graph is the implementation plan; the contracts below supply its component detail and acceptance criteria. New files are proposed ownership boundaries, not existing implementations. Native changes are limited to identity, command acknowledgement, and observable activity contracts; the recording/transcription algorithms are not prerequisites for this UX correction.

| Graph node | Component contract / principal files | Findings |
|---|---|---|
| c1 | Semantic tokens and accessible feedback/control primitives: `app/globals.css`, `frontend/tailwind.config.js`, `components/ui/*`, `ThemedToaster.tsx`. | 16, 18, 20 |
| c2 | Committed configuration and awaited save API: `contexts/ConfigContext.tsx`, `services/configService.ts`, proposed `types/modelConfig.ts`. | 08, 12, 19, 20 |
| c3 | Catalog selectors, latest-query search, metadata-preserving mutations/navigation: `Sidebar/SidebarProvider.tsx`, `hooks/useNavigation.ts`, proposed `lib/meetingCatalog.ts`. Move summary polling to c6. | 01, 07, 11, 12 |
| c4 | Revision-aware note persistence and editors: proposed `contexts/NotePersistenceContext.tsx`, `services/notePersistenceService.ts`; `lib/liveNotes.ts`, `LiveNotesPad.tsx`, `BlockNotesEditor.tsx`, `MeetingDetails/MeetingRawNotesEditor.tsx`. Legacy `MeetingLiveNotes.tsx` cleanup belongs here. | 06, 09, 21 |
| c5 | Native session/task identity, activity snapshot/event adapter, correlated prompt requests: proposed Rust `meeting_activity.rs`, relevant `lib.rs`, `meeting_prompt.rs`, `tray.rs`, `audio/{recording_commands,transcription_queue,import,retranscription,diarization}.rs`; frontend `recordingService.ts`, proposed `meetingActivityService.ts` and `types/meetingActivity.ts`. | 02, 04, 05, 13, 15 |
| c6 | Meeting/task activity owner: proposed `MeetingActivityContext.tsx`, existing `RecordingStateContext.tsx` compatibility facade, `useTranscriptionProgress`, `useRecordingStateSync`, `useSummaryGeneration`, `AutoSummaryProvider`, `lib/autoSummary.ts`, `shared/TranscriptionProgressToast.tsx`, `StatusOverlays.tsx`. | 03, 04, 05, 13 |
| c7 | Global recording commands and recovery: proposed `RecordingControllerContext.tsx`; `RecordingPostProcessingProvider`, `useRecordingStart`, `useRecordingStop`, `useModalState`, `useTranscriptRecovery`, `RecordingControls`, `RecordingStatusBar`, `TranscriptRecovery/`, `MeetingDetails/FloatingRecordingControls.tsx`. | 02, 06, 13, 15 |
| c8 | Export and clipboard operations: `hooks/meeting-details/useCopyOperations.ts`, proposed `lib/meetingExport.ts`, `lib/clipboard.ts`. | 09, 12 |
| c9 | Settings route and shared transactional forms: `app/settings/page.tsx`, `app/_components/SettingsModal.tsx`, existing top-level settings components, `ModelSettingsModal.tsx`, `useModelConfiguration.ts`, proposed `settings/ModelConfigForm.tsx`. `SettingTabs`/`CustomDialog` cleanup belongs here. | 08, 16, 18, 19, 20, 21 |
| c10 | Shell, Meetings library, startup, palette, prompt theme/error UI: `app/{layout,page}.tsx`, `app/meeting-prompt/page.tsx`, `ShellContext`, `ImportDialogContext`, `SimpleSidebar`, `CommandPalette`, `MeetingDetectedPrompt`, `MainContent/index.tsx`, proposed `MeetingsLibrary.tsx`, `lib/theme.ts`, native `tauri.conf.json`. `MainNav`/`MessageToast` cleanup belongs here. | 01, 02, 12, 14, 15, 16, 17, 21 |
| c11 | Workspace presentation, summary actions, search, responsive panels: meeting-details route files, `MeetingDetails/*` excluding c4/c7 editors/controls, `VirtualizedTranscriptView`, `usePaginatedTranscripts`, `useMeetingData`, `useMeetingOperations`, `useTemplates`. | 02, 03, 05, 07, 08, 10, 16, 17, 18 |

**Ownership rule:** c1 owns shared primitives, c2 committed config, and c9 form consumers. c3 owns catalog mutation semantics while c11 calls them. c4 owns note persistence while c7 requests transition flushes. c5 owns native registration and event contracts, c6 the frontend process state, c7 recording commands, and c10 provider mounting. Touch each shared file through its owner rather than implementing competing versions.

**Native contract decisions to settle before UI wiring:** queue snapshots currently need more identity/terminal-state information; imports need task-to-meeting correlation; active recording identity needs native binding because the frontend currently creates the row after capture starts; prompt acceptance must distinguish a forwarded request from a successful start. Reuse existing APIs where possible and add only the missing contract.

## Verification gates for implementation

Existing focused tests cover summary generation, pagination/refresh, recovery, audio playback, progress mapping, and some empty-state rendering. They do not prove route-to-workspace composition, full lifecycle transitions, native event bridging, keyboard operation, or theme/resize behavior. In particular, a passing isolated empty-state test would not catch UX-03 because its parent never renders it.

Add meaningful regression coverage for:

1. Opening the active session through Recent, library, palette, URL, and Return to recording; opening a different meeting while capture continues.
2. No-summary → Generate → pending → success/failure/cancel → Retry; existing summary remains readable during regeneration.
3. Task progress/failure while mounted, unmounted, returning, and reloading; unrelated/out-of-order events; matching task/meeting identity.
4. Latest-note revision, failed load/save, rapid edit/navigation, raw/enhanced switching, stop, and retry without losing the draft.
5. Metadata preservation and rollback on title/pin/archive failures.
6. Settings draft Cancel, rejected Save, pending double submit, successful commit visible in every entry point.
7. Notes-only export; complete paginated export; failed transcript read; rejected clipboard write.
8. Search hits after row 100, stale catalog queries, empty library versus no matches versus failure, matching feature gates across action surfaces.
9. Startup-check failure, onboarding command availability, and prompt Start failure/duplicate requests.

After targeted tests pass, run the frontend's existing `pnpm run test`, `pnpm run typecheck`, `pnpm run lint`, and `pnpm run build`; use focused Rust tests and `cargo check` for the changed native contract with the project's platform feature selection. Install dependencies before these checks. Verify the packaged Tauri app, because a browser-only render cannot validate native events or window behavior.

Desktop acceptance matrix:

- Light and dark themes, including open dialogs, editors, native chrome, and the already-open meeting prompt.
- Default 1100×700 window; compact widths around 900px; wide 1440px; minimum supported size established by the implementation; increased text scale. Check settings tabs, toolbar wrapping, note width, and floating recording controls.
- Fresh setup, existing user, missing permissions/devices, model unavailable/downloading, no meetings, archive-only library, failed refresh.
- Recording and paused states; start/stop from supported entry points; navigate away and return; deferred transcription/summary success, failure, and cancellation.
- Keyboard-only navigation, visible focus, dialog Escape/focus restoration, screen-reader names/state announcements, keyboard panel sizing, reduced motion.

Completion means these findings have verified acceptance evidence, not merely that duplicate components were removed or screenshots look consistent. The audit and graph are ready for implementation; application changes are not part of this audit artifact.
