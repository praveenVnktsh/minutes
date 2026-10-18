"use client";
import { useState, useEffect, useRef, useCallback } from 'react';
import { motion } from 'framer-motion';
import { MeetingSummary, SummaryProcessResponse } from '@/types';
import type { MeetingActivity, MeetingActivityStatus } from '@/types/meetingActivity';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import Analytics from '@/lib/analytics';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Copy, Download, Loader2, MoreHorizontal, Sparkles, Square } from 'lucide-react';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { TranscriptPanel } from '@/components/MeetingDetails/TranscriptPanel';
import { MeetingWorkspace } from '@/components/MeetingDetails/MeetingWorkspace';
import { MeetingAssistantPanel } from '@/components/MeetingDetails/MeetingAssistantPanel';
import { CopilotPanel } from '@/components/MeetingDetails/CopilotPanel';
import { MeetingNotesEditor } from '@/components/MeetingDetails/MeetingNotesEditor';
import { LiveTranscriptPanel } from '@/components/MeetingDetails/LiveTranscriptPanel';
import { LiveNotes, LiveNotesNotice } from '@/components/MeetingDetails/LiveNotes';
import { FloatingRecordingControls } from '@/components/MeetingDetails/FloatingRecordingControls';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import {
  liveNotesTarget,
  meetingNotesTarget,
  noteDocumentId,
  notePersistenceService,
} from '@/services/notePersistenceService';
import { hasAiNotes, useShowAiNotes } from '@/lib/aiNotes';
import { storedSummaryMarkdown } from '@/lib/meetingExport';
import { toast } from 'sonner';

// Custom hooks
import { useMeetingData } from '@/hooks/meeting-details/useMeetingData';
import { useSummaryGeneration } from '@/hooks/meeting-details/useSummaryGeneration';
import { useFinalPassWaiting } from '@/hooks/useFinalPassWaiting';
import { useTemplates } from '@/hooks/meeting-details/useTemplates';
import { useCopyOperations } from '@/hooks/meeting-details/useCopyOperations';
import { useTitleRegeneration } from '@/hooks/meeting-details/useTitleRegeneration';
import { useMeetingOperations } from '@/hooks/meeting-details/useMeetingOperations';
import { useConfig } from '@/contexts/ConfigContext';
import { useRouter } from 'next/navigation';
import { settingsHref } from '@/components/settings/settingsSections';
import { badge } from '@/lib/theme-classes';

export default function PageContent({
  meeting,
  summaryData,
  initialSummary,
  initialSummaryError,
  onRetryInitialSummary,
  onMeetingUpdated,
  onRefetchTranscripts,
  onRefreshTranscripts,
  // Pagination props for efficient transcript loading
  segments,
  hasMore,
  isLoadingMore,
  totalCount,
  loadedCount,
  onLoadMore,
}: {
  meeting: any;
  summaryData: MeetingSummary | null;
  initialSummary: SummaryProcessResponse | null;
  initialSummaryError?: string | null;
  onRetryInitialSummary?: () => void;
  onMeetingUpdated?: () => Promise<void>;
  onRefetchTranscripts?: () => Promise<void>;
  /** Reload transcript rows in place, without clearing the panel. */
  onRefreshTranscripts?: () => Promise<void>;
  // Pagination props
  segments?: any[];
  hasMore?: boolean;
  isLoadingMore?: boolean;
  totalCount?: number;
  loadedCount?: number;
  onLoadMore?: () => void;
}) {
  console.log('📄 PAGE CONTENT: Initializing with data:', {
    meetingId: meeting.id,
    summaryDataKeys: summaryData ? Object.keys(summaryData) : null,
    transcriptsCount: meeting.transcripts?.length
  });

  // State
  const [showAiNotes] = useShowAiNotes();
  const activity = useMeetingActivity();
  const meetingActivities = activity.getMeetingActivities(meeting.id);
  // A meeting accumulates activities over time (an import, then retranscriptions), so each
  // derived value picks the newest activity whose status it actually cares about rather than
  // whichever task happens to hold the highest revision.
  const latestActivityWithStatus = (statuses: MeetingActivityStatus[]): MeetingActivity | null => (
    meetingActivities.reduce<MeetingActivity | null>(
      (latest, item) => statuses.includes(item.status) && (!latest || item.revision > latest.revision) ? item : latest,
      null,
    )
  );
  const inFlightActivity = latestActivityWithStatus(['queued', 'transcribing']);
  const latestTerminalActivity = latestActivityWithStatus(['ready', 'failed', 'cancelled']);
  const isRecordingThisMeeting = activity.activeMeetingId === meeting.id
    && activity.recording !== null
    && ['starting', 'recording', 'paused', 'saving'].includes(activity.recording.status);
  const isTranscribing = inFlightActivity !== null;
  // A queued or running pass supersedes an older failure, so a stale error cannot linger.
  const activityError = !isTranscribing && latestTerminalActivity?.status === 'failed'
    ? latestTerminalActivity.error
    : null;
  const summaryActivity = activity.getSummaryActivity(meeting.id);
  const finalPass = useFinalPassWaiting(meeting.id);
  const [liveFolderPath, setLiveFolderPath] = useState<string | null>(null);

  // Ref to store the modal open function from SummaryGeneratorButtonGroup
  // Sidebar context
  const { renameMeeting, refetchMeetings, meetingMutations } = useSidebar();
  const router = useRouter();

  // Get model config from ConfigContext
  const { modelConfig, isModelConfigLoading, isModelConfigSaving } = useConfig();

  // Custom hooks
  const meetingData = useMeetingData({ meeting, summaryData, onMeetingUpdated });
  const templates = useTemplates();
  const meetingTitle = meetingData.meetingTitle;
  const updateMeetingTitle = meetingData.updateMeetingTitle;
  const lifetimeRef = useRef({ meetingId: meeting.id, mounted: true });
  lifetimeRef.current.meetingId = meeting.id;
  useEffect(() => {
    const lifetime = lifetimeRef.current;
    lifetime.mounted = true;
    return () => { lifetime.mounted = false; };
  }, []);

  const handleTitleChange = useCallback(async (nextTitle: string) => {
    const trimmed = nextTitle.trim();
    if (!trimmed || trimmed === meetingTitle) return;
    await renameMeeting(meeting.id, trimmed);
    if (lifetimeRef.current.mounted && lifetimeRef.current.meetingId === meeting.id) {
      updateMeetingTitle(trimmed);
    }
  }, [meeting.id, meetingTitle, renameMeeting, updateMeetingTitle]);

  // The backend has already saved the new title; refresh the page and sidebar list to match
  const titleRegeneration = useTitleRegeneration({
    meetingId: meeting.id,
    onRegenerated: async (title) => {
      if (lifetimeRef.current.mounted && lifetimeRef.current.meetingId === meeting.id) {
        updateMeetingTitle(title);
      }
      await refetchMeetings();
    },
  });

  const handleOpenModelSettings = useCallback(async () => {
    try {
      await notePersistenceService.flushNotes(meetingNotesTarget(meeting.id));
      if (lifetimeRef.current.mounted && lifetimeRef.current.meetingId === meeting.id) {
        router.push(settingsHref('summary'));
      }
    } catch (error) {
      toast.error('Could not leave while notes are unsaved', { description: String(error) });
    }
  }, [meeting.id, router]);

  const summaryGeneration = useSummaryGeneration({
    initialSummary,
    meeting,
    transcripts: meetingData.transcripts,
    modelConfig: modelConfig,
    isModelConfigLoading,
    isModelConfigSaving,
    selectedTemplate: templates.selectedTemplate,
    onMeetingUpdated,
    updateMeetingTitle,
    setAiSummary: meetingData.setAiSummary,
    onOpenModelSettings: handleOpenModelSettings,
  });

  const copyOperations = useCopyOperations({
    meeting,
    transcripts: meetingData.transcripts,
    meetingTitle: meetingData.meetingTitle,
    aiSummary: meetingData.aiSummary,
    showAiNotes,
    notesTarget: isRecordingThisMeeting && liveFolderPath ? liveNotesTarget(liveFolderPath) : undefined,
  });

  const meetingOperations = useMeetingOperations({
    meeting,
  });

  // Whether the notes document has AI lines; an old meeting without them
  // shows its stored Enhanced notes below the notes instead.
  const notesHaveAi = useNotesHaveAi(meeting.id);
  const storedAiNotes = storedSummaryMarkdown(meetingData.aiSummary);

  useEffect(() => {
    if (!isRecordingThisMeeting) {
      setLiveFolderPath(null);
      return;
    }
    let cancelled = false;
    void invoke<string | null>('get_meeting_folder_path').then((folderPath) => {
      if (!cancelled) setLiveFolderPath(folderPath);
    }).catch(() => {
      if (!cancelled) setLiveFolderPath(null);
    });
    return () => { cancelled = true; };
  }, [isRecordingThisMeeting]);

  // A finished pass replaces every transcript row with fresh ids, so reload once per task
  // that reaches 'ready'. Cancelled and failed passes leave the existing rows in place.
  // The reload swaps rows in place (no empty flash), and the transcript view keeps
  // the reader's place by time.
  const refetchedTaskIdsRef = useRef(new Set<string>());
  const refetchedMeetingIdRef = useRef(meeting.id);
  useEffect(() => {
    if (refetchedMeetingIdRef.current !== meeting.id) {
      refetchedMeetingIdRef.current = meeting.id;
      refetchedTaskIdsRef.current = new Set<string>();
    }
    let hasNewlyReadyTask = false;
    for (const item of meetingActivities) {
      if (item.status !== 'ready' || refetchedTaskIdsRef.current.has(item.task_id)) continue;
      refetchedTaskIdsRef.current.add(item.task_id);
      hasNewlyReadyTask = true;
    }
    if (hasNewlyReadyTask) void (onRefreshTranscripts ?? onRefetchTranscripts)?.();
  }, [meeting.id, meetingActivities, onRefreshTranscripts, onRefetchTranscripts]);

  // The Rust core saves a pass's transcript before it starts diarizing, and saves
  // speaker labels when diarization finishes. Show each as soon as it lands.
  useEffect(() => {
    const reload = onRefreshTranscripts ?? onRefetchTranscripts;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<{ meeting_id: string }>('transcripts-updated', ({ payload }) => {
      if (payload.meeting_id === meeting.id) void reload?.();
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    }).catch((error) => console.warn('Could not listen for transcript updates:', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [meeting.id, onRefreshTranscripts, onRefetchTranscripts]);

  useEffect(() => {
    const handleFinalized = (event: Event) => {
      const finalizedMeetingId = (event as CustomEvent<{ meetingId?: string }>).detail?.meetingId;
      if (finalizedMeetingId === meeting.id) void onRefetchTranscripts?.();
    };
    window.addEventListener('meetily:recording-finalized', handleFinalized);
    return () => window.removeEventListener('meetily:recording-finalized', handleFinalized);
  }, [meeting.id, onRefetchTranscripts]);

  const isSummaryActive = summaryGeneration.summaryStatus === 'processing'
    || summaryGeneration.summaryStatus === 'summarizing'
    || summaryGeneration.summaryStatus === 'regenerating';
  const showAssistant = true;
  const peopleCount = new Set(
    meetingData.transcripts.map((t: any) => t.speaker_id ?? t.speaker).filter(Boolean)
  ).size;
  // The summary job after a meeting with AI lines only names the meeting, so
  // its failures are not the notes'. Without AI lines it writes the AI notes.
  const aiNotesFromSummary = !notesHaveAi;
  const statusBanner = isSummaryActive ? (
    <span className="flex shrink-0 items-center gap-1.5 rounded-full bg-surface-2 px-3 py-1.5 text-[11px] font-medium text-ink-muted">
      <Loader2 className="h-3.5 w-3.5 animate-spin" /> Finishing notes…
    </span>
  ) : aiNotesFromSummary && summaryActivity?.status === 'failed' ? (
    <span className={`shrink-0 rounded-full px-3 py-1.5 text-[11px] font-medium ${badge.error}`} role="alert">
      {summaryActivity.error || 'AI notes failed'}
    </span>
  ) : isTranscribing ? (
    <span className={`flex shrink-0 items-center gap-1.5 rounded-full px-3 py-1.5 text-[11px] font-medium ${badge.info}`} role="status">
      <Loader2 className="h-3.5 w-3.5 animate-spin" /> {(() => {
        // Over an existing transcript the pass is improving it, in the background.
        const label = meetingData.transcripts.length > 0 ? 'Improving transcript' : 'Transcribing';
        return inFlightActivity?.progress_percentage == null
          ? `${label}…`
          : `${label} ${Math.round(inFlightActivity.progress_percentage)}%`;
      })()}
    </span>
  ) : activityError ? <span role="alert" className="text-xs font-medium text-error">{activityError}</span>
  : finalPass.waiting ? (
    <span className={`flex shrink-0 items-center gap-1.5 rounded-full px-3 py-1.5 text-[11px] font-medium ${badge.neutral}`} role="status" title="Waiting until your Mac is plugged in and out of Low Power Mode">
      Transcript improves when plugged in ·
      <button
        type="button"
        className="font-semibold text-ink underline-offset-2 hover:underline disabled:opacity-60"
        disabled={finalPass.starting}
        onClick={() => void finalPass.improveNow()}
      >
        Improve now
      </button>
    </span>
  ) : null;

  // "Write AI notes" (or rewrite them) maps onto the old summary only for a
  // meeting whose notes have no AI lines; the result shows below the notes.
  const canWriteAiNotes = aiNotesFromSummary
    && meetingData.transcripts.length > 0
    && !initialSummaryError
    && !isModelConfigLoading;
  const menuItem = 'flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-xs text-ink hover:bg-surface-2 disabled:opacity-50';
  const notesMenu = (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          title="More"
          aria-label="Notes options"
          className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-ink-muted hover:bg-surface-2 hover:text-ink"
        >
          <MoreHorizontal className="h-4 w-4" />
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-48 p-1">
        <button type="button" className={menuItem} onClick={() => void copyOperations.handleCopyNotes()}>
          <Copy className="h-3.5 w-3.5 text-ink-muted" /> Copy notes
        </button>
        <button type="button" className={menuItem} onClick={() => void copyOperations.handleExportMarkdown()}>
          <Download className="h-3.5 w-3.5 text-ink-muted" /> Export
        </button>
        {canWriteAiNotes && (isSummaryActive ? (
          <button type="button" className={menuItem} onClick={() => void summaryGeneration.handleStopGeneration()}>
            <Square className="h-3.5 w-3.5 text-ink-muted" /> Stop
          </button>
        ) : (
          <button
            type="button"
            className={menuItem}
            onClick={() => void (storedAiNotes
              ? summaryGeneration.handleRegenerateSummary()
              : summaryGeneration.handleGenerateSummary(''))}
          >
            <Sparkles className="h-3.5 w-3.5 text-ink-muted" /> {storedAiNotes ? 'Rewrite AI notes' : 'Write AI notes'}
          </button>
        ))}
      </PopoverContent>
    </Popover>
  );

  // Track page view
  useEffect(() => {
    Analytics.trackPageView('meeting_details');
  }, []);

  if (isRecordingThisMeeting) {
    return (
      <motion.div
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        transition={{ duration: 0.2, ease: 'easeOut' }}
        className="relative flex h-screen min-w-0 flex-col bg-surface-0"
      >
        <MeetingWorkspace
          title={meetingData.meetingTitle}
          createdAt={meeting.created_at}
          notes={<LiveNotes />}
          notesNotice={<LiveNotesNotice />}
          transcript={<LiveTranscriptPanel />}
          assistant={
            <CopilotPanel
              notesTarget={liveFolderPath ? liveNotesTarget(liveFolderPath) : undefined}
            />
          }
          showAssistant
          assistantLabel="Copilot"
          peopleCount={0}
          onTitleChange={handleTitleChange}
          onRegenerateTitle={titleRegeneration.regenerateTitle}
          titleRegenerating={titleRegeneration.isRegenerating}
          titlePending={meetingMutations[meeting.id]?.rename?.status === 'pending'}
          titleError={meetingMutations[meeting.id]?.rename?.error}
        />
        <FloatingRecordingControls />
      </motion.div>
    );
  }

  return (
    <motion.div
      initial={{ opacity: 0, y: 20 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.3, ease: 'easeOut' }}
      className="flex h-screen min-w-0 flex-col bg-[var(--surface-0)]"
    >
      <div className="flex flex-1 min-w-0 overflow-hidden">
        <MeetingWorkspace
          title={meetingData.meetingTitle}
          createdAt={meeting.created_at}
          showAssistant={showAssistant}
          peopleCount={peopleCount}
          statusBanner={statusBanner}
          toolbarActions={notesMenu}
          onTitleChange={handleTitleChange}
          onRegenerateTitle={titleRegeneration.regenerateTitle}
          titleRegenerating={titleRegeneration.isRegenerating}
          titlePending={meetingMutations[meeting.id]?.rename?.status === 'pending'}
          titleError={meetingMutations[meeting.id]?.rename?.error}
          notesNotice={initialSummaryError ? (
            <button type="button" className="underline-offset-2 hover:underline" onClick={() => onRetryInitialSummary?.()}>
              Could not load AI notes · Retry
            </button>
          ) : undefined}
          transcript={
            <TranscriptPanel
              transcripts={meetingData.transcripts}
              onCopyTranscript={copyOperations.handleCopyTranscript}
              onOpenMeetingFolder={meetingOperations.handleOpenMeetingFolder}
              isRecording={false}
              isTranscribing={isTranscribing}
              locked={isSummaryActive && aiNotesFromSummary}
              disableAutoScroll={true}
              usePagination={true}
              segments={segments}
              hasMore={hasMore}
              isLoadingMore={isLoadingMore}
              totalCount={totalCount}
              loadedCount={loadedCount}
              onLoadMore={onLoadMore}
              meetingId={meeting.id}
              meetingFolderPath={meeting.folder_path}
              onRefetchTranscripts={onRefetchTranscripts}
            />
          }
          notes={<MeetingNotesEditor meetingId={meeting.id} legacyNotes={storedAiNotes} />}
          assistant={
            <MeetingAssistantPanel
              meetingId={meeting.id}
              modelConfig={modelConfig}
            />
          }
        />
      </div>
    </motion.div>
  );
}

/** Whether the meeting's notes document has AI lines, kept current as the
 * notes load and change. */
function useNotesHaveAi(meetingId: string): boolean {
  const target = meetingNotesTarget(meetingId);
  const key = noteDocumentId(target);
  const read = () => hasAiNotes(notePersistenceService.getSnapshot(meetingNotesTarget(meetingId)).document);
  const [value, setValue] = useState(read);
  useEffect(() => {
    const target = meetingNotesTarget(meetingId);
    const update = () => setValue(hasAiNotes(notePersistenceService.getSnapshot(target).document));
    update();
    return notePersistenceService.subscribeNotes(target, update);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return value;
}
