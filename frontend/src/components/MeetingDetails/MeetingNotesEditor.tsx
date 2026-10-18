'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { BlockNotesEditor } from '@/components/BlockNotesEditor';
import { CitedMarkdown } from '@/components/MeetingDetails/CitedMarkdown';
import { SaveFeedback, StatusFeedback } from '@/components/ui/status-feedback';
import { completeAiNotes, hasAiNotes, legacyAiNotes, useShowAiNotes, WRAP_UP_ENTRY } from '@/lib/aiNotes';
import { createEmptyLiveNotesDocument, type LiveNotesDocument } from '@/lib/liveNotes';
import type { StoredTimelineEntry } from '@/lib/liveTimeline';
import {
  meetingNotesTarget,
  noteDocumentId,
  notePersistenceService,
  type NotePersistenceSnapshot,
} from '@/services/notePersistenceService';

/** An old meeting's Enhanced notes: read-only AI content below the notes. */
export function LegacyAiNotes({ markdown }: { markdown: string }) {
  return (
    <section
      aria-label="AI notes"
      data-testid="legacy-ai-notes"
      className="mt-8 border-t border-hairline pt-6 text-sm leading-6 text-ink-muted [&_h1]:mb-2 [&_h1]:mt-5 [&_h1]:font-semibold [&_h2]:mb-2 [&_h2]:mt-5 [&_h2]:font-semibold [&_h3]:mb-1 [&_h3]:mt-4 [&_h3]:font-semibold [&_li]:my-1 [&_ol]:list-decimal [&_ol]:pl-5 [&_p]:my-2 [&_ul]:list-disc [&_ul]:pl-5"
    >
      <p className="mb-2 text-[10px] font-semibold uppercase tracking-[0.08em] text-ink-subtle">AI</p>
      <CitedMarkdown>{markdown}</CitedMarkdown>
    </section>
  );
}

/**
 * The meeting's notes after recording: the same document the user typed in
 * during the meeting, AI lines included. When the meeting has ended, the
 * live notes not written yet and its Decisions and Action items are added
 * once (no model). An old meeting without AI lines shows its old Enhanced
 * notes below, read-only.
 */
export function MeetingNotesEditor({
  meetingId,
  legacyNotes = '',
}: {
  meetingId: string;
  /** The stored Enhanced notes (Markdown) of an old meeting. */
  legacyNotes?: string;
}) {
  const target = useMemo(() => meetingNotesTarget(meetingId), [meetingId]);
  const [showAi] = useShowAiNotes();
  const [snapshot, setSnapshot] = useState<NotePersistenceSnapshot>(() => ({
    ...notePersistenceService.getSnapshot(target),
  }));
  const visibleSnapshot = noteDocumentId(snapshot.target) === noteDocumentId(target)
    ? snapshot
    : { ...notePersistenceService.getSnapshot(target) };

  const loadNotes = useCallback(async () => {
    await notePersistenceService.loadNotes(target, { createEmpty: createEmptyLiveNotesDocument });
  }, [target]);

  useEffect(() => {
    setSnapshot({ ...notePersistenceService.getSnapshot(target) });
    const unsubscribe = notePersistenceService.subscribeNotes(target, () => {
      setSnapshot({ ...notePersistenceService.getSnapshot(target) });
    });
    void loadNotes();
    return () => {
      unsubscribe();
      void notePersistenceService.flushNotes(target).catch(() => {});
    };
  }, [loadNotes, target]);

  const wrapUpStarted = useRef<string | null>(null);

  // The live-notes capture unmounts on stop, before the meeting row is finalized.
  // Refetch once the stop pipeline has persisted the notes so nothing disappears.
  useEffect(() => {
    const handler = (event: Event) => {
      const id = (event as CustomEvent<{ meetingId?: string }>).detail?.meetingId;
      if (id && id !== meetingId) return;
      wrapUpStarted.current = null;
      void loadNotes();
    };
    window.addEventListener('meetily:recording-finalized', handler);
    window.addEventListener('meetily:transcription-complete', handler);
    return () => {
      window.removeEventListener('meetily:recording-finalized', handler);
      window.removeEventListener('meetily:transcription-complete', handler);
    };
  }, [meetingId, loadNotes]);

  // The meeting has ended: finish the AI lines once (Decisions, Action items).
  const document = visibleSnapshot.document;
  const needsWrapUp = Boolean(document && hasAiNotes(document) && !document.aiEntries?.includes(WRAP_UP_ENTRY));
  useEffect(() => {
    if (!needsWrapUp || visibleSnapshot.loadState !== 'ready' || wrapUpStarted.current === meetingId) return;
    wrapUpStarted.current = meetingId;
    void invoke<StoredTimelineEntry[]>('list_meeting_timeline_entries', { meetingId })
      .then((entries) => {
        const current = notePersistenceService.getSnapshot(target).document;
        const completed = current ? completeAiNotes(current, entries ?? []) : null;
        if (completed) notePersistenceService.saveNotes(target, completed);
      })
      .catch((error) => console.warn('Could not add decisions and action items:', error));
  }, [meetingId, needsWrapUp, target, visibleSnapshot.loadState]);

  const handleChange = useCallback((next: LiveNotesDocument) => {
    notePersistenceService.saveNotes(target, next);
  }, [target]);

  if (visibleSnapshot.loadState === 'loading' && !document) {
    return <div className="flex h-full items-center justify-center text-sm text-[var(--ink-subtle)]">Loading your notes…</div>;
  }
  if (visibleSnapshot.loadState === 'error' && !document) {
    return (
      <div className="flex h-full items-center justify-center px-8 text-center">
        <div>
          <p className="text-sm font-medium text-error">Could not load your notes</p>
          <p className="mt-1 text-xs text-[var(--ink-subtle)]">The existing document was not replaced. Retry when storage is available.</p>
          <button type="button" className="mt-3 text-xs font-semibold text-info underline" onClick={() => void loadNotes()}>Retry</button>
        </div>
      </div>
    );
  }

  const oldAiNotes = legacyAiNotes(document, legacyNotes, showAi);
  if (!document) {
    return oldAiNotes ? (
      <div className="h-full overflow-y-auto">
        <div className="mx-auto w-full max-w-[860px] px-10 pb-24 pt-6"><LegacyAiNotes markdown={oldAiNotes} /></div>
      </div>
    ) : (
      <div className="flex h-full items-center justify-center px-8 text-center">
        <p className="text-sm text-[var(--ink-muted)]">No notes were taken</p>
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto">
      <div className="sticky top-0 z-10 flex justify-end bg-[var(--surface-0)] px-10 py-2">
        {visibleSnapshot.loadState === 'error' && (
          <StatusFeedback tone="error" actionLabel="Retry" onAction={() => void loadNotes()} className="mr-auto">
            Could not refresh notes; showing your local draft
          </StatusFeedback>
        )}
        <SaveFeedback
          state={visibleSnapshot.saveState}
          actionLabel={visibleSnapshot.saveState === 'error' ? 'Retry' : undefined}
          onAction={visibleSnapshot.saveState === 'error' ? () => void notePersistenceService.retryNotes(target).catch(() => {}) : undefined}
        />
      </div>
      <div className="meeting-notes-editor raw-notes-editor mx-auto w-full max-w-[860px] px-10 pb-24 pt-6">
        <BlockNotesEditor
          key={meetingId}
          document={document}
          onChange={handleChange}
        />
        {oldAiNotes && <LegacyAiNotes markdown={oldAiNotes} />}
      </div>
    </div>
  );
}
