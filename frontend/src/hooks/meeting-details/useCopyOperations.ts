import { useCallback } from 'react';
import { invoke as invokeTauri } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import type { MeetingSummary, Transcript } from '@/types';
import type { LiveNotesDocument } from '@/lib/liveNotes';
import Analytics from '@/lib/analytics';
import { writeClipboardText } from '@/lib/clipboard';
import { legacyAiNotes, notesMarkdown } from '@/lib/aiNotes';
import {
  buildMeetingExport,
  exportFileName,
  fetchCompleteTranscripts,
  storedSummaryMarkdown,
  transcriptMarkdown,
  type TranscriptPage,
} from '@/lib/meetingExport';
import {
  meetingNotesTarget,
  notePersistenceService,
  type NoteDocumentTarget,
} from '@/services/notePersistenceService';

interface UseCopyOperationsProps {
  meeting: { id: string; title?: string; created_at?: string };
  transcripts: Transcript[];
  meetingTitle: string;
  /** The stored Enhanced notes: shown, copied and exported only for a
   * meeting whose notes have no AI lines. */
  aiSummary: MeetingSummary | null;
  /** "Show AI notes": off copies and exports the user's lines only. */
  showAiNotes: boolean;
  notesTarget?: NoteDocumentTarget;
  currentNotesDocument?: LiveNotesDocument | null;
}

async function readAllTranscripts(meetingId: string): Promise<Transcript[]> {
  return fetchCompleteTranscripts(meetingId, (args) => invokeTauri<TranscriptPage>(
    'api_get_meeting_transcripts',
    args,
  ));
}

function trackCopy(type: 'summary' | 'transcript', metadata: Record<string, string>): void {
  void Analytics.trackCopy(type, metadata).catch((error) => {
    console.error('Failed to record copy analytics:', error);
  });
}

export function useCopyOperations({
  meeting,
  meetingTitle,
  aiSummary,
  showAiNotes,
  notesTarget,
  currentNotesDocument,
}: UseCopyOperationsProps) {
  const title = meetingTitle || meeting.title || 'Untitled meeting';

  /** The notes as shown: the document (AI lines only when shown), and an old
   * meeting's Enhanced notes when they are shown below it. */
  const readNotes = useCallback(async (): Promise<{ notes: string; aiNotes: string }> => {
    const target = notesTarget ?? meetingNotesTarget(meeting.id);
    await notePersistenceService.flushNotes(target);
    let document = currentNotesDocument;
    if (document === undefined) {
      const snapshot = await notePersistenceService.loadNotes(target);
      if (snapshot.loadState === 'error') throw snapshot.loadError;
      document = snapshot.document;
    }
    return {
      notes: notesMarkdown(document, showAiNotes),
      aiNotes: legacyAiNotes(document, storedSummaryMarkdown(aiSummary), showAiNotes),
    };
  }, [aiSummary, currentNotesDocument, meeting.id, notesTarget, showAiNotes]);

  const handleCopyTranscript = useCallback(async () => {
    try {
      const allTranscripts = await readAllTranscripts(meeting.id);
      if (allTranscripts.length === 0) {
        toast.error('No transcript available to copy');
        return;
      }

      const date = meeting.created_at ? `\n\n_${new Date(meeting.created_at).toLocaleString()}_` : '';
      await writeClipboardText(`# Transcript: ${title}${date}\n\n${transcriptMarkdown(allTranscripts)}\n`);
      toast.success('Transcript copied to clipboard');
      const wordCount = allTranscripts.reduce((count, transcript) => (
        count + transcript.text.trim().split(/\s+/).filter(Boolean).length
      ), 0);
      trackCopy('transcript', {
        meeting_id: meeting.id,
        transcript_length: String(allTranscripts.length),
        word_count: String(wordCount),
      });
    } catch (error) {
      console.error('Failed to copy transcript:', error);
      toast.error('Failed to copy transcript');
    }
  }, [meeting.created_at, meeting.id, title]);

  const handleCopyNotes = useCallback(async () => {
    try {
      const { notes, aiNotes } = await readNotes();
      const body = [notes.trim(), aiNotes.trim()].filter(Boolean).join('\n\n');
      if (!body) {
        toast.error('No notes to copy');
        return;
      }
      await writeClipboardText(`# ${title}\n\n${body}\n`);
      toast.success('Notes copied');
      trackCopy('summary', { meeting_id: meeting.id, with_ai_notes: String(showAiNotes) });
    } catch (error) {
      console.error('Failed to copy notes:', error);
      toast.error('Failed to copy notes');
    }
  }, [meeting.id, readNotes, showAiNotes, title]);

  const handleExportMarkdown = useCallback(async () => {
    try {
      const { notes, aiNotes } = await readNotes();
      const allTranscripts = await readAllTranscripts(meeting.id);
      if (!notes && !aiNotes && allTranscripts.length === 0) {
        toast.error('Nothing to export yet');
        return;
      }

      const contents = buildMeetingExport({
        title,
        createdAt: meeting.created_at,
        notes,
        aiNotes,
        transcripts: allTranscripts,
      });
      const path = await invokeTauri<string>('save_text_export', {
        fileName: exportFileName(title),
        contents,
      });
      toast.success('Meeting exported', { description: path });
    } catch (error) {
      console.error('Failed to export meeting:', error);
      toast.error('Failed to export meeting');
    }
  }, [meeting.created_at, meeting.id, readNotes, title]);

  return { handleCopyTranscript, handleCopyNotes, handleExportMarkdown };
}
