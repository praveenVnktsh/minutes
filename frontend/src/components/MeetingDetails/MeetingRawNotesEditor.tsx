'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import { BlockNotesEditor } from '@/components/BlockNotesEditor';
import { SaveFeedback, StatusFeedback } from '@/components/ui/status-feedback';
import { createEmptyLiveNotesDocument, type LiveNotesDocument } from '@/lib/liveNotes';
import {
  meetingNotesTarget,
  notePersistenceService,
  type NotePersistenceSnapshot,
} from '@/services/notePersistenceService';

export function MeetingRawNotesEditor({ meetingId }: { meetingId: string }) {
  const target = useMemo(() => meetingNotesTarget(meetingId), [meetingId]);
  const [snapshot, setSnapshot] = useState<NotePersistenceSnapshot>(() => ({
    ...notePersistenceService.getSnapshot(target),
  }));

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

  // The live-notes capture unmounts on stop, before the meeting row is finalized.
  // Refetch once the stop pipeline has persisted the notes so nothing disappears.
  useEffect(() => {
    const handler = (event: Event) => {
      const id = (event as CustomEvent<{ meetingId?: string }>).detail?.meetingId;
      if (id && id !== meetingId) return;
      void loadNotes();
    };
    window.addEventListener('meetily:recording-finalized', handler);
    window.addEventListener('meetily:transcription-complete', handler);
    return () => {
      window.removeEventListener('meetily:recording-finalized', handler);
      window.removeEventListener('meetily:transcription-complete', handler);
    };
  }, [meetingId, loadNotes]);

  const handleChange = useCallback((next: LiveNotesDocument) => {
    notePersistenceService.saveNotes(target, next);
    // Let the workspace glow the re-enhance control; enhancement stays manual.
    window.dispatchEvent(new CustomEvent('meetily:raw-notes-changed', { detail: { meetingId } }));
  }, [meetingId, target]);

  if (snapshot.loadState === 'loading' && !snapshot.document) {
    return <div className="flex h-full items-center justify-center text-sm text-[var(--ink-subtle)]">Loading your notes…</div>;
  }
  if (snapshot.loadState === 'error' && !snapshot.document) {
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
  if (!snapshot.document) {
    return (
      <div className="flex h-full items-center justify-center px-8 text-center">
        <div>
          <p className="text-sm font-medium text-[var(--ink-muted)]">No raw notes were taken</p>
          <p className="mt-1 text-xs text-[var(--ink-subtle)]">Notes you type during a meeting will remain available here unchanged.</p>
        </div>
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto">
      <div className="sticky top-0 z-10 flex justify-end bg-[var(--surface-0)] px-10 py-2">
        {snapshot.loadState === 'error' && (
          <StatusFeedback tone="error" actionLabel="Retry" onAction={() => void loadNotes()} className="mr-auto">
            Could not refresh notes; showing your local draft
          </StatusFeedback>
        )}
        <SaveFeedback
          state={snapshot.saveState}
          actionLabel={snapshot.saveState === 'error' ? 'Retry' : undefined}
          onAction={snapshot.saveState === 'error' ? () => void notePersistenceService.retryNotes(target).catch(() => {}) : undefined}
        />
      </div>
      <div className="meeting-notes-editor raw-notes-editor mx-auto w-full max-w-[860px] px-10 pb-24 pt-6">
        <BlockNotesEditor
          key={meetingId}
          document={snapshot.document}
          onChange={handleChange}
        />
      </div>
    </div>
  );
}
