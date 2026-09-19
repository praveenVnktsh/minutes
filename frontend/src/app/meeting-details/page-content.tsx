"use client";
import { useState, useEffect, useRef, useCallback } from 'react';
import { motion } from 'framer-motion';
import { MeetingSummary, SummaryProcessResponse } from '@/types';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import Analytics from '@/lib/analytics';
import { invoke } from '@tauri-apps/api/core';
import { Download, Loader2, MoreHorizontal } from 'lucide-react';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { TranscriptPanel } from '@/components/MeetingDetails/TranscriptPanel';
import { SummaryPanel } from '@/components/MeetingDetails/SummaryPanel';
import { SummaryGeneratorButtonGroup } from '@/components/MeetingDetails/SummaryGeneratorButtonGroup';
import { SummaryUpdaterButtonGroup } from '@/components/MeetingDetails/SummaryUpdaterButtonGroup';
import { SummaryLanguagePill } from '@/components/MeetingDetails/SummaryLanguagePill';
import { MeetingWorkspace, type NotesMode } from '@/components/MeetingDetails/MeetingWorkspace';
import { MeetingAssistantPanel } from '@/components/MeetingDetails/MeetingAssistantPanel';
import { MeetingRawNotesEditor } from '@/components/MeetingDetails/MeetingRawNotesEditor';
import { LiveTranscriptPanel } from '@/components/MeetingDetails/LiveTranscriptPanel';
import { FloatingRecordingControls } from '@/components/MeetingDetails/FloatingRecordingControls';
import { LiveNotesPad } from '@/components/LiveNotesPad';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import { liveNotesTarget, meetingNotesTarget, notePersistenceService } from '@/services/notePersistenceService';
import { toast } from 'sonner';

// Custom hooks
import { useMeetingData } from '@/hooks/meeting-details/useMeetingData';
import { useSummaryGeneration } from '@/hooks/meeting-details/useSummaryGeneration';
import { useTemplates } from '@/hooks/meeting-details/useTemplates';
import { useCopyOperations } from '@/hooks/meeting-details/useCopyOperations';
import { useMeetingOperations } from '@/hooks/meeting-details/useMeetingOperations';
import { useConfig } from '@/contexts/ConfigContext';
import { useRouter } from 'next/navigation';
import { settingsHref } from '@/components/settings/settingsSections';

export default function PageContent({
  meeting,
  summaryData,
  initialSummary,
  initialSummaryError,
  onRetryInitialSummary,
  onMeetingUpdated,
  onRefetchTranscripts,
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
  const customPrompt = '';
  const [notesMode, setNotesMode] = useState<NotesMode>('enhanced');
  // True when raw notes changed after the last enhancement, so the workspace can
  // nudge the user to re-enhance instead of regenerating automatically.
  const [notesDirtySinceSummary, setNotesDirtySinceSummary] = useState(false);
  const activity = useMeetingActivity();
  const meetingActivities = activity.getMeetingActivities(meeting.id);
  const latestMeetingActivity = meetingActivities.reduce<(typeof meetingActivities)[number] | null>(
    (latest, item) => !latest || item.revision > latest.revision ? item : latest,
    null,
  );
  const isRecordingThisMeeting = activity.activeMeetingId === meeting.id
    && activity.recording !== null
    && ['starting', 'recording', 'paused', 'saving'].includes(activity.recording.status);
  const isTranscribing = latestMeetingActivity?.status === 'queued' || latestMeetingActivity?.status === 'transcribing';
  const activityError = latestMeetingActivity?.status === 'failed' ? latestMeetingActivity.error : null;
  const summaryActivity = activity.getSummaryActivity(meeting.id);
  const [liveFolderPath, setLiveFolderPath] = useState<string | null>(null);

  // Ref to store the modal open function from SummaryGeneratorButtonGroup
  const autoSwitchedSummaryMeetingIdsRef = useRef(new Set<string>());
  const manuallySelectedViewMeetingIdsRef = useRef(new Set<string>());

  // Sidebar context
  const { renameMeeting, meetingMutations } = useSidebar();
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
    blockNoteSummaryRef: meetingData.blockNoteSummaryRef,
    notesTarget: isRecordingThisMeeting && liveFolderPath ? liveNotesTarget(liveFolderPath) : undefined,
  });

  const meetingOperations = useMeetingOperations({
    meeting,
  });

  const handleNotesModeChange = useCallback(async (mode: NotesMode) => {
    if (mode === notesMode) return;
    try {
      if (notesMode === 'raw') {
        await notePersistenceService.flushNotes(meetingNotesTarget(meeting.id));
      } else if (meetingData.blockNoteSummaryRef.current?.isDirty) {
        await meetingData.blockNoteSummaryRef.current.saveSummary();
      }
      setNotesMode(mode);
    } catch (error) {
      toast.error('Could not switch notes view', {
        description: `Your current draft remains open. ${error instanceof Error ? error.message : String(error)}`,
      });
    }
  }, [meeting.id, meetingData.blockNoteSummaryRef, notesMode]);

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

  const previousActivityRevision = useRef<number | null>(null);
  useEffect(() => {
    if (latestMeetingActivity?.status === 'ready' && previousActivityRevision.current !== latestMeetingActivity.revision) {
      void onRefetchTranscripts?.();
    }
    previousActivityRevision.current = latestMeetingActivity?.revision ?? null;
  }, [latestMeetingActivity, onRefetchTranscripts]);

  useEffect(() => {
    const handleFinalized = (event: Event) => {
      const finalizedMeetingId = (event as CustomEvent<{ meetingId?: string }>).detail?.meetingId;
      if (finalizedMeetingId === meeting.id) void onRefetchTranscripts?.();
    };
    window.addEventListener('meetily:recording-finalized', handleFinalized);
    return () => window.removeEventListener('meetily:recording-finalized', handleFinalized);
  }, [meeting.id, onRefetchTranscripts]);

  // Glow the re-enhance control when the user's notes changed after the last
  // enhancement. Enhancement itself stays manual.
  useEffect(() => {
    const handler = (event: Event) => {
      const id = (event as CustomEvent<{ meetingId?: string }>).detail?.meetingId;
      if (id && id !== meeting.id) return;
      setNotesDirtySinceSummary(true);
    };
    window.addEventListener('meetily:raw-notes-changed', handler);
    return () => window.removeEventListener('meetily:raw-notes-changed', handler);
  }, [meeting.id]);

  useEffect(() => {
    if (summaryGeneration.summaryStatus === 'completed') {
      setNotesDirtySinceSummary(false);
    }
  }, [summaryGeneration.summaryStatus]);

  const isSummaryActive = summaryGeneration.summaryStatus === 'processing'
    || summaryGeneration.summaryStatus === 'summarizing'
    || summaryGeneration.summaryStatus === 'regenerating';
  const canShowEnhanced = Boolean(meetingData.aiSummary) || summaryGeneration.summaryStatus === 'completed';
  const showAssistant = true;
  const effectiveNotesMode: NotesMode = notesMode;
  const peopleCount = new Set(
    meetingData.transcripts.map((t: any) => t.speaker_id ?? t.speaker).filter(Boolean)
  ).size;
  const statusBanner = isSummaryActive ? (
    <span className="flex shrink-0 items-center gap-1.5 rounded-full bg-surface-2 px-3 py-1.5 text-[11px] font-medium text-ink-muted">
      <Loader2 className="h-3.5 w-3.5 animate-spin" /> Generating summary…
    </span>
  ) : summaryActivity?.status === 'cancelled' ? (
    <span className="shrink-0 rounded-full bg-warning-soft px-3 py-1.5 text-[11px] font-medium text-warning" role="status">Summary generation cancelled</span>
  ) : summaryActivity?.status === 'failed' ? (
    <span className="shrink-0 rounded-full bg-error-soft px-3 py-1.5 text-[11px] font-medium text-error" role="alert">
      {summaryActivity.error || 'Summary generation failed'}
    </span>
  ) : isTranscribing ? (
    <span className="flex shrink-0 items-center gap-1.5 rounded-full bg-info-soft px-3 py-1.5 text-[11px] font-medium text-info" role="status">
      <Loader2 className="h-3.5 w-3.5 animate-spin" /> {latestMeetingActivity?.progress_percentage == null ? 'Transcribing…' : `Transcribing ${Math.round(latestMeetingActivity.progress_percentage)}%`}
    </span>
  ) : activityError ? <span role="alert" className="text-xs font-medium text-error">{activityError}</span> : null;

  const summaryToolbarActions = (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          title="Summary options"
          aria-label="Summary options"
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full border border-hairline text-ink-muted hover:bg-surface-2 hover:text-ink"
        >
          <MoreHorizontal className="h-4 w-4" />
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-[340px] space-y-3 p-3">
        <div className="flex items-center gap-2 text-[11px] text-ink-subtle">
          <span>{new Date(meeting.created_at).toLocaleDateString([], { month: 'short', day: 'numeric', year: 'numeric' })}</span>
          {peopleCount > 0 && (
            <>
              <span>·</span>
              <span>{peopleCount} {peopleCount === 1 ? 'person' : 'people'}</span>
            </>
          )}
        </div>
        <SummaryGeneratorButtonGroup
          modelConfig={modelConfig}
          onGenerateSummary={summaryGeneration.handleGenerateSummary}
          onRegenerateSummary={summaryGeneration.handleRegenerateSummary}
          onStopGeneration={summaryGeneration.handleStopGeneration}
          customPrompt={customPrompt}
          summaryStatus={summaryGeneration.summaryStatus}
          availableTemplates={templates.availableTemplates}
          selectedTemplate={templates.selectedTemplate}
          onTemplateSelect={templates.handleTemplateSelection}
          hasTranscripts={meetingData.transcripts.length > 0}
          hasSummary={canShowEnhanced}
          isModelConfigLoading={isModelConfigLoading}
          summaryReadUnavailable={Boolean(initialSummaryError)}
          languageSlot={<SummaryLanguagePill meetingId={meeting.id} />}
        />
        {canShowEnhanced && (
          <SummaryUpdaterButtonGroup onCopy={copyOperations.handleCopySummary} />
        )}
      </PopoverContent>
    </Popover>
  );

  const exportButton = (
    <button
      type="button"
      onClick={copyOperations.handleExportMarkdown}
      title="Export meeting as Markdown"
      aria-label="Export meeting as Markdown"
      className="flex h-8 shrink-0 items-center gap-1.5 rounded-full border border-hairline px-3 text-xs text-ink-muted transition-colors hover:bg-surface-2 hover:text-ink"
    >
      <Download className="h-3.5 w-3.5" /> Export
    </button>
  );

  // Track page view
  useEffect(() => {
    Analytics.trackPageView('meeting_details');
  }, []);

  useEffect(() => {
    if (
      (meetingData.aiSummary || summaryGeneration.summaryStatus === 'completed')
      && !autoSwitchedSummaryMeetingIdsRef.current.has(meeting.id)
      && !manuallySelectedViewMeetingIdsRef.current.has(meeting.id)
    ) {
      autoSwitchedSummaryMeetingIdsRef.current.add(meeting.id);
      setNotesMode('enhanced');
    }
  }, [meeting.id, meetingData.aiSummary, summaryGeneration.summaryStatus]);

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
          notesMode="raw"
          onNotesModeChange={() => {}}
          canShowEnhanced={false}
          summary={null}
          rawNotes={<LiveNotesPad bare />}
          transcript={<LiveTranscriptPanel />}
          assistant={
            <MeetingAssistantPanel
              meetingId={meeting.id}
              modelConfig={modelConfig}
              onNotesUpdated={(markdown) => meetingData.setAiSummary({ markdown })}
              onTranscriptUpdated={onRefetchTranscripts}
            />
          }
          showAssistant
          peopleCount={0}
          onTitleChange={handleTitleChange}
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
          notesMode={effectiveNotesMode}
          onNotesModeChange={(mode) => {
            manuallySelectedViewMeetingIdsRef.current.add(meeting.id);
            void handleNotesModeChange(mode);
          }}
          canShowEnhanced
          hasEnhancedContent={canShowEnhanced}
          showAssistant={showAssistant}
          peopleCount={peopleCount}
          statusBanner={statusBanner}
          toolbarActions={<>{summaryToolbarActions}{exportButton}</>}
          onTitleChange={handleTitleChange}
          titlePending={meetingMutations[meeting.id]?.rename?.status === 'pending'}
          titleError={meetingMutations[meeting.id]?.rename?.error}
          onRegenerate={() => {
            setNotesDirtySinceSummary(false);
            void summaryGeneration.handleRegenerateSummary();
          }}
          onStopGeneration={summaryGeneration.handleStopGeneration}
          isGenerating={isSummaryActive}
          notesDirty={notesDirtySinceSummary}
          transcript={
            <TranscriptPanel
              transcripts={meetingData.transcripts}
              onCopyTranscript={copyOperations.handleCopyTranscript}
              onOpenMeetingFolder={meetingOperations.handleOpenMeetingFolder}
               isRecording={false}
              isTranscribing={isTranscribing}
              locked={isSummaryActive}
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
          rawNotes={<MeetingRawNotesEditor meetingId={meeting.id} />}
          assistant={
            <MeetingAssistantPanel
              meetingId={meeting.id}
              modelConfig={modelConfig}
              onNotesUpdated={(markdown) => {
                meetingData.setAiSummary({ markdown });
                setNotesMode('enhanced');
              }}
              onTranscriptUpdated={onRefetchTranscripts}
            />
          }
          summary={
            <SummaryPanel
              meeting={meeting}
              meetingTitle={meetingData.meetingTitle}
              summaryRef={meetingData.blockNoteSummaryRef}
              isSaving={meetingData.isSaving}
              isSummaryDirty={meetingData.isSummaryDirty}
              onCopySummary={copyOperations.handleCopySummary}
              aiSummary={meetingData.aiSummary}
              summaryStatus={summaryGeneration.summaryStatus}
              transcripts={meetingData.transcripts}
              modelConfig={modelConfig}
              onGenerateSummary={summaryGeneration.handleGenerateSummary}
              onStopGeneration={summaryGeneration.handleStopGeneration}
              customPrompt={customPrompt}
              onSaveSummary={meetingData.handleSaveSummary}
              onSummaryChange={meetingData.handleSummaryChange}
              onDirtyChange={meetingData.setIsSummaryDirty}
              summaryError={summaryGeneration.summaryError}
              summaryReadError={initialSummaryError}
              onRetrySummaryRead={onRetryInitialSummary}
              onRegenerateSummary={summaryGeneration.handleRegenerateSummary}
              getSummaryStatusMessage={summaryGeneration.getSummaryStatusMessage}
              availableTemplates={templates.availableTemplates}
              selectedTemplate={templates.selectedTemplate}
              onTemplateSelect={templates.handleTemplateSelection}
              isModelConfigLoading={isModelConfigLoading}
              onOpenModelSettings={() => void handleOpenModelSettings()}
            />
          }
        />
      </div>
    </motion.div>
  );
}
