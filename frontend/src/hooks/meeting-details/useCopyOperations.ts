import { useCallback, type RefObject } from 'react';
import { invoke as invokeTauri } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import type { MeetingSummary, Transcript } from '@/types';
import type { BlockNoteSummaryViewRef } from '@/components/AISummary/BlockNoteSummaryView';
import Analytics from '@/lib/analytics';
import { writeClipboardText } from '@/lib/clipboard';
import {
  buildMeetingExport,
  exportFileName,
  fetchCompleteTranscripts,
  originalNotesMarkdown,
  storedSummaryMarkdown,
  transcriptMarkdown,
  type TranscriptPage,
} from '@/lib/meetingExport';
import {
  meetingNotesTarget,
  notePersistenceService,
} from '@/services/notePersistenceService';

interface UseCopyOperationsProps {
  meeting: { id: string; title?: string; created_at?: string };
  transcripts: Transcript[];
  meetingTitle: string;
  aiSummary: MeetingSummary | null;
  blockNoteSummaryRef: RefObject<BlockNoteSummaryViewRef>;
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
  blockNoteSummaryRef,
}: UseCopyOperationsProps) {
  const title = meetingTitle || meeting.title || 'Untitled meeting';

  const buildSummaryMarkdown = useCallback(async (): Promise<string> => {
    const editor = blockNoteSummaryRef.current;
    if (editor) {
      if (editor.isDirty) await editor.saveSummary();
      return (await editor.getMarkdown()).trim();
    }
    return storedSummaryMarkdown(aiSummary);
  }, [aiSummary, blockNoteSummaryRef]);

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

  const handleCopySummary = useCallback(async () => {
    try {
      const summaryMarkdown = await buildSummaryMarkdown();
      if (!summaryMarkdown) {
        toast.error('No enhanced notes available to copy');
        return;
      }

      const date = meeting.created_at ? `\n\n_${new Date(meeting.created_at).toLocaleString()}_` : '';
      await writeClipboardText(`# Enhanced notes: ${title}${date}\n\n${summaryMarkdown}\n`);
      toast.success('Enhanced notes copied to clipboard');
      trackCopy('summary', {
        meeting_id: meeting.id,
        has_markdown: String(Boolean(aiSummary && 'markdown' in aiSummary)),
      });
    } catch (error) {
      console.error('Failed to copy enhanced notes:', error);
      toast.error('Failed to copy enhanced notes');
    }
  }, [aiSummary, buildSummaryMarkdown, meeting.created_at, meeting.id, title]);

  const handleExportMarkdown = useCallback(async () => {
    try {
      const target = meetingNotesTarget(meeting.id);
      await notePersistenceService.flushNotes(target);
      const notesSnapshot = await notePersistenceService.loadNotes(target);
      if (notesSnapshot.loadState === 'error') throw notesSnapshot.loadError;

      const [enhancedNotes, allTranscripts] = await Promise.all([
        buildSummaryMarkdown(),
        readAllTranscripts(meeting.id),
      ]);
      const originalNotes = originalNotesMarkdown(notesSnapshot.document);
      if (!originalNotes && !enhancedNotes && allTranscripts.length === 0) {
        toast.error('Nothing to export yet');
        return;
      }

      const contents = buildMeetingExport({
        title,
        createdAt: meeting.created_at,
        originalNotes,
        enhancedNotes,
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
  }, [buildSummaryMarkdown, meeting.created_at, meeting.id, title]);

  return { handleCopyTranscript, handleCopySummary, handleExportMarkdown };
}
