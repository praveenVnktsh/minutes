import { useState, useCallback, useRef, useEffect } from 'react';
import { MeetingSummary, Summary } from '@/types';
import { BlockNoteSummaryViewRef } from '@/components/AISummary/BlockNoteSummaryView';
import { invoke as invokeTauri } from '@tauri-apps/api/core';
import { hasVisibleSummaryContent, isManuallyClearedSummary } from '@/lib/summary-content';

interface UseMeetingDataProps {
  meeting: any;
  summaryData: MeetingSummary | null;
  onMeetingUpdated?: () => Promise<void>;
}

export function useMeetingData({ meeting, summaryData, onMeetingUpdated }: UseMeetingDataProps) {
  // State
  // Use prop directly since summary generation fetches transcripts independently
  const transcripts = meeting.transcripts;
  const [meetingTitle, setMeetingTitle] = useState(meeting.title || '+ New Call');
  const [aiSummary, setAiSummary] = useState<MeetingSummary | null>(summaryData);
  const [isSaving, setIsSaving] = useState(false);
  const [isSummaryDirty, setIsSummaryDirty] = useState(false);
  const summaryDataKey = JSON.stringify(summaryData);
  const summaryDataKeyRef = useRef(summaryDataKey);
  const saveQueueRef = useRef<Promise<void>>(Promise.resolve());
  const pendingSaveCountRef = useRef(0);

  // Ref for BlockNoteSummaryView
  const blockNoteSummaryRef = useRef<BlockNoteSummaryViewRef>(null);

  // Sync aiSummary state when summaryData prop changes (fixes display of fetched summaries)
  useEffect(() => {
    if (summaryDataKeyRef.current === summaryDataKey) return;
    summaryDataKeyRef.current = summaryDataKey;
    console.log('[useMeetingData] Syncing summary data from prop:', summaryData ? 'present' : 'null');
    setAiSummary(summaryData);
  }, [summaryData, summaryDataKey]);

  const handleSummaryChange = useCallback((newSummary: Summary) => {
    setAiSummary(newSummary);
  }, []);



  const handleSaveSummary = useCallback(async (summary: MeetingSummary) => {
    if (!hasVisibleSummaryContent(summary) && !isManuallyClearedSummary(summary)) {
      throw new Error('Summary contains no visible content to save.');
    }

    const formattedSummary = 'markdown' in summary || 'summary_json' in summary
      ? summary
      : { MeetingName: meetingTitle, ...summary };
    pendingSaveCountRef.current += 1;
    setIsSaving(true);
    const save = saveQueueRef.current.catch(() => {}).then(async () => {
      await invokeTauri('api_save_meeting_summary', {
        meetingId: meeting.id,
        summary: formattedSummary,
      });
      setAiSummary(formattedSummary);
    });
    saveQueueRef.current = save;
    try {
      await save;
    } finally {
      pendingSaveCountRef.current -= 1;
      if (pendingSaveCountRef.current === 0) setIsSaving(false);
    }
  }, [meeting.id, meetingTitle]);

  // Update meeting title from external source (e.g., AI summary)
  const updateMeetingTitle = useCallback((newTitle: string) => {
    console.log('📝 Updating meeting title to:', newTitle);
    setMeetingTitle(newTitle);
  }, []);

  return {
    // State
    transcripts,
    meetingTitle,
    aiSummary,
    isSaving,
    isSummaryDirty,
    blockNoteSummaryRef,

    // Setters
    setMeetingTitle,
    setAiSummary,
    setIsSummaryDirty,

    // Handlers
    handleSummaryChange,
    handleSaveSummary,
    updateMeetingTitle,
  };
}
