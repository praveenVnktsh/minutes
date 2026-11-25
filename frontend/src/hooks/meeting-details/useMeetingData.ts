import { useState, useCallback, useRef, useEffect } from 'react';
import { MeetingSummary } from '@/types';

interface UseMeetingDataProps {
  meeting: any;
  summaryData: MeetingSummary | null;
  onMeetingUpdated?: () => Promise<void>;
}

/** The meeting page's data: its transcript, title and the stored AI summary
 * (an old meeting's Enhanced notes, shown read-only below its notes). */
export function useMeetingData({ meeting, summaryData }: UseMeetingDataProps) {
  // Use prop directly since summary generation fetches transcripts independently
  const transcripts = meeting.transcripts;
  const [meetingTitle, setMeetingTitle] = useState(meeting.title || '+ New Call');
  const [aiSummary, setAiSummaryState] = useState<MeetingSummary | null>(summaryData);
  const summaryDataKey = JSON.stringify(summaryData);
  const summaryDataKeyRef = useRef(summaryDataKey);

  // Sync aiSummary state when summaryData prop changes (fixes display of fetched summaries)
  useEffect(() => {
    if (summaryDataKeyRef.current === summaryDataKey) return;
    summaryDataKeyRef.current = summaryDataKey;
    setAiSummaryState(summaryData);
  }, [summaryData, summaryDataKey]);

  const setAiSummary = useCallback((summary: MeetingSummary | null) => {
    setAiSummaryState(summary);
  }, []);

  // Update meeting title from external source (e.g., AI summary)
  const updateMeetingTitle = useCallback((newTitle: string) => {
    setMeetingTitle(newTitle);
  }, []);

  return {
    transcripts,
    meetingTitle,
    aiSummary,
    setMeetingTitle,
    setAiSummary,
    updateMeetingTitle,
  };
}
