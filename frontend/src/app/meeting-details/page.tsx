"use client"
import { useSidebar } from "@/components/Sidebar/SidebarProvider";
import { useState, useEffect, Suspense } from "react";
import { MeetingSummary, SummaryProcessResponse, Transcript } from "@/types";
import PageContent from "./page-content";
import { useRouter, useSearchParams } from "next/navigation";
import Analytics from "@/lib/analytics";
import { invoke } from "@tauri-apps/api/core";
import { LoaderIcon } from "lucide-react";
import { usePaginatedTranscripts } from "@/hooks/usePaginatedTranscripts";
import { parseSummaryContent } from "@/lib/summary-content";
import { Button } from "@/components/ui/button";
import { toneText } from "@/lib/theme-classes";

interface MeetingDetailsResponse {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  transcripts: Transcript[];
  folder_path?: string;
}

function MeetingDetailsContent() {
  const searchParams = useSearchParams();
  const meetingId = searchParams.get('id');
  const { setCurrentMeeting, refetchMeetings } = useSidebar();
  const router = useRouter();
  const [meetingDetails, setMeetingDetails] = useState<MeetingDetailsResponse | null>(null);
  const [summaryResponse, setSummaryResponse] = useState<SummaryProcessResponse | null>(null);
  const [meetingSummary, setMeetingSummary] = useState<MeetingSummary | null>(null);
  const [summaryLoadError, setSummaryLoadError] = useState<string | null>(null);
  const [summaryReadRevision, setSummaryReadRevision] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState<boolean>(true);

  // Use pagination hook for efficient transcript loading
  const {
    metadata,
    segments,
    transcripts,
    isLoading: isLoadingTranscripts,
    isLoadingMore,
    hasMore,
    totalCount,
    loadedCount,
    loadMore,
    refetch,
    refresh,
    error: transcriptError,
  } = usePaginatedTranscripts({ meetingId: meetingId || '' });

  // Sync meeting metadata from pagination hook to meeting details state
  useEffect(() => {
    if (isLoadingTranscripts || !meetingId || meetingId === 'intro-call' || metadata?.id !== meetingId) {
      // Keep the current view until the selected meeting's first transcript page is ready.
      return;
    }

    if (metadata) {
      console.log('Meeting metadata loaded:', metadata);

      // Build meeting details from metadata and paginated transcripts
      setMeetingDetails({
        id: metadata.id,
        title: metadata.title,
        created_at: metadata.created_at,
        updated_at: metadata.updated_at,
        transcripts: transcripts, // Paginated transcripts from hook
        folder_path: metadata.folder_path, // For retranscription feature
      });

      // Sync with sidebar context
      setCurrentMeeting({ id: metadata.id, title: metadata.title });
    }
  }, [metadata, transcripts, meetingId, isLoadingTranscripts, setCurrentMeeting]);

  // Handle transcript loading errors
  useEffect(() => {
    if (transcriptError) {
      console.error('Error loading transcripts:', transcriptError);
      setError(transcriptError);
    }
  }, [transcriptError]);

  // Reset states when meetingId changes (prevent race conditions)
  useEffect(() => {
    setMeetingDetails(null);
    setMeetingSummary(null);
    setSummaryResponse(null);
    setSummaryLoadError(null);
    setError(null);
    setIsLoading(true);
  }, [meetingId]);

  useEffect(() => {
    console.log('MeetingDetails useEffect triggered - meetingId:', meetingId);

    if (!meetingId || meetingId === 'intro-call') {
      console.warn('No valid meeting ID in URL - meetingId:', meetingId);
      setError("No meeting selected");
      setIsLoading(false);
      Analytics.trackPageView('meeting_details');
      return;
    }

    console.log('Valid meeting ID found, fetching details for:', meetingId);

    let cancelled = false;
    const fetchMeetingSummary = async () => {
      try {
        const response = await invoke<SummaryProcessResponse>('api_get_summary', {
          meetingId,
        });
        if (cancelled) return;
        setSummaryLoadError(null);
        setSummaryResponse(response);
        const summary = parseSummaryContent(response.data);
        setMeetingSummary(response.status === 'idle' ? null : summary);
      } catch (error) {
        if (cancelled) return;
        console.error('FETCH SUMMARY: Error fetching meeting summary:', error);
        setMeetingSummary(null);
        setSummaryResponse(null);
        setSummaryLoadError(error instanceof Error ? error.message : String(error));
      }
    };

    const loadData = async () => {
      try {
        await fetchMeetingSummary();
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    };

    loadData();
    return () => { cancelled = true; };
  }, [meetingId, summaryReadRevision]);

  if (error) {
    return (
      <div className="flex items-center justify-center h-screen">
        <div className="text-center">
          <p className={`${toneText.error} mb-4`}>{error}</p>
          <Button
            onClick={() => router.push('/')}
          >
            Go Back
          </Button>
        </div>
      </div>
    );
  }

  // Show loading spinner while initial data loads
  if (isLoading || !meetingDetails || meetingDetails.id !== meetingId) {
    return <div className="flex items-center justify-center h-screen">
      <LoaderIcon className="animate-spin size-6 " />
    </div>;
  }

  return <PageContent
    key={meetingId}
    initialSummary={summaryResponse}
    initialSummaryError={summaryLoadError}
    onRetryInitialSummary={() => setSummaryReadRevision((revision) => revision + 1)}
    meeting={meetingDetails}
    summaryData={meetingSummary}
    onMeetingUpdated={async () => {
      await refetchMeetings();
    }}
    onRefetchTranscripts={refetch}
    onRefreshTranscripts={refresh}
    // Pagination props for efficient transcript loading
    segments={segments}
    hasMore={hasMore}
    isLoadingMore={isLoadingMore}
    totalCount={totalCount}
    loadedCount={loadedCount}
    onLoadMore={loadMore}
  />;
}

export default function MeetingDetails() {
  return (
    <Suspense fallback={
      <div className="flex items-center justify-center h-screen">
        <LoaderIcon className="animate-spin size-6" />
      </div>
    }>
      <MeetingDetailsContent />
    </Suspense>
  );
}
