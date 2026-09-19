'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import dynamic from 'next/dynamic';
import { invoke } from '@tauri-apps/api/core';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { SaveFeedback, StatusFeedback } from '@/components/ui/status-feedback';
import { createEmptyLiveNotesDocument } from '@/lib/liveNotes';
import {
  liveNotesTarget,
  notePersistenceService,
  type NotePersistenceSnapshot,
} from '@/services/notePersistenceService';

const BlockNotesEditor = dynamic(
  () => import('@/components/BlockNotesEditor').then((module) => module.BlockNotesEditor),
  {
    ssr: false,
    loading: () => <div className="text-sm text-[var(--ink-subtle)]">Opening notes…</div>,
  },
);

export function LiveNotesPad({ bare = false }: { bare?: boolean } = {}) {
  const { isRecording, recordingDuration } = useRecordingState();
  const [folderPath, setFolderPath] = useState<string | null>(null);
  const [folderError, setFolderError] = useState(false);
  const durationRef = useRef(0);
  const folderRequestRef = useRef(0);
  durationRef.current = recordingDuration ?? 0;
  const target = useMemo(() => folderPath ? liveNotesTarget(folderPath) : null, [folderPath]);
  const [snapshot, setSnapshot] = useState<NotePersistenceSnapshot | null>(null);

  const findFolder = useCallback(async () => {
    if (!isRecording) return;
    const request = ++folderRequestRef.current;
    setFolderError(false);
    let path: string | null = null;
    for (let attempt = 0; attempt < 8 && !path; attempt += 1) {
      path = await invoke<string | null>('get_meeting_folder_path').catch(() => null);
      if (!path) await new Promise((resolve) => setTimeout(resolve, 250));
    }
    if (request !== folderRequestRef.current) return;
    if (path) setFolderPath(path);
    else setFolderError(true);
  }, [isRecording]);

  useEffect(() => {
    folderRequestRef.current += 1;
    setFolderPath(null);
    setSnapshot(null);
    if (isRecording) void findFolder();
    return () => { folderRequestRef.current += 1; };
  }, [findFolder, isRecording]);

  useEffect(() => {
    if (!target) return;
    setSnapshot({ ...notePersistenceService.getSnapshot(target) });
    const unsubscribe = notePersistenceService.subscribeNotes(target, () => {
      setSnapshot({ ...notePersistenceService.getSnapshot(target) });
    });
    void notePersistenceService.loadNotes(target, {
      createEmpty: () => createEmptyLiveNotesDocument(Date.now() - (durationRef.current * 1000)),
    });
    return () => {
      unsubscribe();
      void notePersistenceService.flushNotes(target).catch(() => {});
    };
  }, [target]);

  const handleDocumentChange = useCallback((next: Parameters<typeof notePersistenceService.saveNotes>[1]) => {
    if (target) notePersistenceService.saveNotes(target, next);
  }, [target]);

  const reloadNotes = useCallback(() => {
    if (!target) return Promise.resolve();
    return notePersistenceService.loadNotes(target, {
      createEmpty: () => createEmptyLiveNotesDocument(Date.now() - (durationRef.current * 1000)),
    }).then(() => undefined);
  }, [target]);

  if (folderError) {
    return (
      <div className="flex h-full items-center justify-center px-8 text-center">
        <div>
          <p className="text-sm font-medium text-error">Could not open this meeting’s notes</p>
          <p className="mt-1 text-xs text-[var(--ink-subtle)]">No unscoped draft was created, so another meeting’s notes cannot be reused.</p>
          <button type="button" className="mt-3 text-xs font-semibold text-info underline" onClick={() => void findFolder()}>Retry</button>
        </div>
      </div>
    );
  }
  if (snapshot?.loadState === 'error' && !snapshot.document) {
    return (
      <div className="flex h-full items-center justify-center px-8 text-center">
        <div>
          <p className="text-sm font-medium text-error">Could not load your notes</p>
          <p className="mt-1 text-xs text-[var(--ink-subtle)]">The existing document was not replaced.</p>
          <button type="button" className="mt-3 text-xs font-semibold text-info underline" onClick={() => void reloadNotes()}>Retry</button>
        </div>
      </div>
    );
  }
  if (!snapshot?.document) {
    return <div className="flex h-full items-center justify-center text-sm text-ink-subtle">Preparing notes…</div>;
  }

  if (bare) {
    return (
      <div className="meeting-notes-editor raw-notes-editor h-full overflow-y-auto">
        <div className="sticky top-0 z-10 flex justify-end bg-[var(--surface-0)] px-10 py-2">
          {snapshot.loadState === 'error' && (
            <StatusFeedback tone="error" actionLabel="Retry" onAction={() => void reloadNotes()} className="mr-auto">
              Could not refresh notes; showing your local draft
            </StatusFeedback>
          )}
          <SaveFeedback
            state={snapshot.saveState}
            actionLabel={snapshot.saveState === 'error' ? 'Retry' : undefined}
            onAction={snapshot.saveState === 'error' ? () => target && void notePersistenceService.retryNotes(target).catch(() => {}) : undefined}
          />
        </div>
        <div className="mx-auto w-full max-w-[860px] px-10 pb-24 pt-6">
          <BlockNotesEditor document={snapshot.document} onChange={handleDocumentChange} />
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col bg-[var(--surface-0)]">
      <div className="flex items-center justify-between px-8 py-4">
        <div>
          <div className="text-sm font-medium text-ink">Your notes</div>
          <div className="text-xs text-[var(--ink-subtle)]">Use / for blocks and Markdown · AI will enrich these after the meeting</div>
        </div>
        <div className="flex items-center gap-3">
          {snapshot.loadState === 'error' && (
            <StatusFeedback tone="error" actionLabel="Retry" onAction={() => void reloadNotes()}>
              Showing local draft
            </StatusFeedback>
          )}
          <SaveFeedback
            state={snapshot.saveState}
            actionLabel={snapshot.saveState === 'error' ? 'Retry' : undefined}
            onAction={snapshot.saveState === 'error' ? () => target && void notePersistenceService.retryNotes(target).catch(() => {}) : undefined}
          />
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-36 pt-3">
        <div className="mx-auto min-h-full max-w-[820px] rounded-2xl bg-[var(--surface-raised)] px-8 py-8 shadow-[0_1px_0_rgba(45,43,37,0.04)]">
          <BlockNotesEditor document={snapshot.document} onChange={handleDocumentChange} />
        </div>
      </div>
    </div>
  );
}
