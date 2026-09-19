import { useState, useCallback, useRef, useEffect } from 'react';
import { MeetingSummary, SummaryProcessResponse, Transcript } from '@/types';
import { ModelConfig } from '@/components/ModelSettingsModal';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import { invoke as invokeTauri } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import Analytics from '@/lib/analytics';

import {
  detectAndCacheSummaryLanguage,
  readMeetingSummaryLanguage,
  readCachedDetectedSummaryLanguage,
} from '@/lib/summary-language-preferences';
import { parseSummaryContent, readSummaryMetadata } from '@/lib/summary-content';
import { loadSummaryNotesContext, validateSummaryModel } from '@/lib/autoSummary';
import { fetchCompleteTranscripts } from '@/lib/meetingExport';
import { useConfig } from '@/contexts/ConfigContext';

async function resolveSummaryLanguage(
  meetingId: string,
  transcriptTexts: string[]
): Promise<string | null> {
  try {
    const perMeeting = await readMeetingSummaryLanguage(meetingId);
    if (perMeeting.language) return perMeeting.language;
  } catch (err) {
    console.warn('Failed to load meeting summary language:', err);
    toast.warning('Could not load saved summary language', {
      description: 'Using Auto for this generation.',
    });
  }

  try {
    const cachedDetected = await readCachedDetectedSummaryLanguage(meetingId);
    if (cachedDetected) return cachedDetected;
  } catch (err) {
    console.warn('Failed to load cached detected summary language:', err);
  }

  try {
    const detection = await detectAndCacheSummaryLanguage(meetingId, transcriptTexts);
    if (detection.reason === 'tie') {
      toast.warning('Bilingual transcript detected', {
        description: 'Pick a summary language manually if Auto chooses the wrong fallback.',
      });
    }
    return detection.language;
  } catch (err) {
    console.warn('Failed to detect transcript summary language:', err);
    return null;
  }
}

type SummaryStatus = 'idle' | 'processing' | 'summarizing' | 'regenerating' | 'completed' | 'error';

function restoredSummaryStatus(response?: SummaryProcessResponse | null): SummaryStatus {
  if (!response) return 'idle';
  if (response.status === 'pending' || response.status === 'processing') return 'processing';
  if (response.status === 'error' || response.status === 'failed') return 'error';
  if (parseSummaryContent(response.data)) return 'completed';
  if (response.status === 'completed') return 'error';
  return 'idle';
}

interface UseSummaryGenerationProps {
  initialSummary?: SummaryProcessResponse | null;
  meeting: {
    id: string;
    created_at: string;
  };
  transcripts: Transcript[];
  modelConfig: ModelConfig;
  isModelConfigLoading: boolean;
  isModelConfigSaving?: boolean;
  modelConfigSaveError?: Error | null;
  selectedTemplate: string;
  onMeetingUpdated?: () => Promise<void>;
  updateMeetingTitle: (title: string) => void;
  setAiSummary: (summary: MeetingSummary | null) => void;
  onOpenModelSettings?: () => void;
}

export function useSummaryGeneration({
  initialSummary,
  meeting,
  transcripts: _transcripts,
  modelConfig,
  isModelConfigLoading,
  isModelConfigSaving = false,
  modelConfigSaveError = null,
  selectedTemplate,
  onMeetingUpdated,
  updateMeetingTitle,
  setAiSummary,
  onOpenModelSettings,
}: UseSummaryGenerationProps) {
  const committedConfig = useConfig();
  const effectiveModelConfig = committedConfig.modelConfig ?? modelConfig;
  const modelConfigurationLoading = isModelConfigLoading || committedConfig.isModelConfigLoading;
  const modelConfigurationSaving = isModelConfigSaving || committedConfig.isModelConfigSaving;
  const modelConfigurationError = modelConfigSaveError ?? committedConfig.modelConfigSaveError;
  const restored = initialSummary?.meeting_id === meeting.id ? initialSummary : null;
  const [summaryStatus, setSummaryStatus] = useState<SummaryStatus>(() => restoredSummaryStatus(restored));
  const [summaryError, setSummaryError] = useState<string | null>(() =>
    restoredSummaryStatus(restored) === 'error'
      ? restored?.error || 'Summary generation failed. Please retry.'
      : null,
  );
  const mountedRef = useRef(true);
  const visibleMeetingIdRef = useRef(meeting.id);
  visibleMeetingIdRef.current = meeting.id;
  const generationIdRef = useRef(0);
  const activeProcessIdRef = useRef<string | null>(null);
  const pollSubscriptionRef = useRef<(() => void) | null>(null);
  const trackedAttemptRef = useRef<{
    generationId: number;
    startedAt: number;
    provider: ModelConfig['provider'];
    model: ModelConfig['model'];
    finished: boolean;
  } | null>(null);
  const {
    cancelSummary,
    hydrateSummary,
    startSummary,
    startSummaryPolling,
  } = useMeetingActivity();

  const getSummaryStatusMessage = useCallback((status: SummaryStatus) => {
    switch (status) {
      case 'processing':
        return 'Processing transcript...';
      case 'summarizing':
        return 'Generating summary...';
      case 'regenerating':
        return 'Regenerating summary...';
      case 'completed':
        return 'Summary completed';
      case 'error':
        return 'Error generating summary';
      default:
        return '';
    }
  }, []);

  const finishGeneration = useCallback(async (
    generationId: number,
    outcome: 'ok' | 'fallback' | 'generation_error' | 'empty_result' | 'cancelled'
  ) => {
    const attempt = trackedAttemptRef.current;
    if (!attempt || attempt.generationId !== generationId || attempt.finished) {
      return;
    }
    attempt.finished = true;
    await Analytics.trackSummaryGenerationCompleted(
      attempt.provider,
      attempt.model,
      outcome === 'ok' || outcome === 'fallback',
      (Date.now() - attempt.startedAt) / 1000,
      outcome === 'ok' ? undefined : outcome,
    );
  }, []);

  const failGeneration = useCallback(async (
    generationId: number,
    isRegeneration: boolean,
    message: string,
    outcome: 'generation_error' | 'empty_result' = 'generation_error',
  ) => {
    if (!mountedRef.current || visibleMeetingIdRef.current !== meeting.id || generationId !== generationIdRef.current) {
      return;
    }
    activeProcessIdRef.current = null;
    setSummaryError(message);
    setSummaryStatus('error');
    toast.error(`Failed to ${isRegeneration ? 'regenerate' : 'generate'} summary`, {
      description: message,
    });
    await finishGeneration(generationId, outcome);
  }, [finishGeneration, meeting.id]);

  const handlePollingResult = useCallback(async (
    pollingResult: SummaryProcessResponse,
    generationId: number,
    isRegeneration: boolean,
  ) => {
    if (!mountedRef.current || visibleMeetingIdRef.current !== meeting.id || generationId !== generationIdRef.current) {
      return;
    }
    if (pollingResult.status === 'cancelled') {
      let existing: SummaryProcessResponse;
      try {
        existing = await invokeTauri<SummaryProcessResponse>('api_get_summary', {
          meetingId: meeting.id,
        });
      } catch (error) {
        console.error('Failed to reload summary after cancellation:', error);
        await failGeneration(generationId, isRegeneration,
          'Summary generation was cancelled, but the saved summary could not be reloaded. Please reopen this meeting.');
        return;
      }
      if (!mountedRef.current || visibleMeetingIdRef.current !== meeting.id || generationId !== generationIdRef.current) {
        return;
      }
      const restoredSummary = parseSummaryContent(existing.data);
      setAiSummary(restoredSummary);
      setSummaryStatus(restoredSummary ? 'completed' : 'idle');
      setSummaryError(null);
      activeProcessIdRef.current = null;
      await finishGeneration(generationId, 'cancelled');
      return;
    }

    if (pollingResult.status === 'error' || pollingResult.status === 'failed') {
      const errorMessage = pollingResult.error
        || `Summary ${isRegeneration ? 'regeneration' : 'generation'} failed`;
      if (isRegeneration) {
        let existing: SummaryProcessResponse;
        try {
          existing = await invokeTauri<SummaryProcessResponse>('api_get_summary', {
            meetingId: meeting.id,
          });
        } catch (error) {
          console.error('Failed to reload previous summary after generation failure:', error);
          await failGeneration(generationId, isRegeneration,
            `${errorMessage}. The saved summary could not be reloaded. Please reopen this meeting.`);
          return;
        }
        if (!mountedRef.current || visibleMeetingIdRef.current !== meeting.id || generationId !== generationIdRef.current) {
          return;
        }
        const restoredSummary = parseSummaryContent(existing.data);
        if (restoredSummary) {
          setAiSummary(restoredSummary);
          setSummaryStatus('completed');
          setSummaryError(null);
          toast.error('Failed to regenerate summary', {
            description: `${errorMessage}. Your previous summary has been restored.`,
          });
          activeProcessIdRef.current = null;
          await finishGeneration(generationId, 'generation_error');
          return;
        }
      }
      await failGeneration(generationId, isRegeneration, errorMessage);
      return;
    }

    if (pollingResult.status === 'completed') {
      const summary = parseSummaryContent(pollingResult.data);
      if (!summary) {
        await failGeneration(generationId, isRegeneration,
          'Summary generation completed without visible content. Please retry.',
          'empty_result',
        );
        return;
      }
      const metadata = readSummaryMetadata(pollingResult.data);
      const meetingName = metadata.meetingName || pollingResult.meetingName;
      if (meetingName) {
        updateMeetingTitle(meetingName);
      }
      setAiSummary(summary);
      setSummaryStatus('completed');
      activeProcessIdRef.current = null;
      setSummaryError(null);
      if (metadata.normalizationFallback) {
        toast.warning('Summary generated with fallback', {
          description: 'English normalization failed, so the original sanitized summary was kept.',
        });
      } else {
        toast.success('Summary generated successfully!', {
          description: metadata.reasoningStripped
            ? 'Your meeting summary is ready. Model reasoning was filtered out of the notes.'
            : 'Your meeting summary is ready',
          duration: 4000,
        });
      }
      if (meetingName && onMeetingUpdated) {
        await onMeetingUpdated();
      }
      await finishGeneration(
        generationId,
        metadata.normalizationFallback ? 'fallback' : 'ok',
      );
    }
  }, [failGeneration, finishGeneration, meeting.id, onMeetingUpdated, setAiSummary, updateMeetingTitle]);

  // Keep polling attached across ordinary rerenders without retaining stale view callbacks.
  const pollingResultRef = useRef(handlePollingResult);
  pollingResultRef.current = handlePollingResult;

  const subscribeToProcess = useCallback((
    processId: string,
    generationId: number,
    isRegeneration: boolean,
  ) => {
    pollSubscriptionRef.current?.();
    pollSubscriptionRef.current = startSummaryPolling(
      meeting.id,
      processId,
      result => pollingResultRef.current(result, generationId, isRegeneration),
    );
  }, [meeting.id, startSummaryPolling]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      pollSubscriptionRef.current?.();
      pollSubscriptionRef.current = null;
    };
  }, [meeting.id]);

  useEffect(() => {
    if (!initialSummary || initialSummary.meeting_id !== meeting.id) return;
    hydrateSummary(initialSummary);
    const status = restoredSummaryStatus(initialSummary);
    setSummaryStatus(status);
    setSummaryError(status === 'error'
      ? initialSummary.error || 'Summary generation failed. Please retry.'
      : null);
    if (status !== 'processing' || !initialSummary.start) return;

    const generationId = ++generationIdRef.current;
    activeProcessIdRef.current = initialSummary.start;
    const isRegeneration = !!parseSummaryContent(initialSummary.data);
    subscribeToProcess(initialSummary.start, generationId, isRegeneration);
  }, [hydrateSummary, initialSummary, meeting.id, subscribeToProcess]);

  const processSummary = useCallback(async ({
    transcriptText,
    transcriptTexts,
    customPrompt = '',
    isRegeneration = false,
  }: {
    transcriptText: string;
    transcriptTexts?: string[];
    customPrompt?: string;
    isRegeneration?: boolean;
  }) => {
    const previousAttempt = trackedAttemptRef.current;
    if (previousAttempt && !previousAttempt.finished) {
      await finishGeneration(previousAttempt.generationId, 'cancelled');
    }

    const generationId = ++generationIdRef.current;
    activeProcessIdRef.current = null;
    setSummaryStatus(isRegeneration ? 'regenerating' : 'processing');
    setSummaryError(null);

    try {
      if (!transcriptText.trim()) {
        await failGeneration(generationId, isRegeneration, 'No transcript text available. Please add some text first.', 'empty_result');
        return;
      }

      const timeSinceRecording = (Date.now() - new Date(meeting.created_at).getTime()) / 60000;
      trackedAttemptRef.current = {
        generationId,
        startedAt: Date.now(),
        provider: effectiveModelConfig.provider,
        model: effectiveModelConfig.model,
        finished: false,
      };
      await Analytics.trackSummaryGenerationStarted(
        effectiveModelConfig.provider,
        effectiveModelConfig.model,
        transcriptText.length,
        timeSinceRecording,
      );
      if (customPrompt.trim()) {
        await Analytics.trackCustomPromptUsed(customPrompt.trim().length);
      }

      const summaryLanguage = await resolveSummaryLanguage(
        meeting.id,
        transcriptTexts?.length ? transcriptTexts : [transcriptText],
      );
      if (!mountedRef.current || visibleMeetingIdRef.current !== meeting.id || generationId !== generationIdRef.current) {
        return;
      }

      const result = await startSummary({
        text: transcriptText,
        model: effectiveModelConfig.provider,
        modelName: effectiveModelConfig.model,
        meetingId: meeting.id,
        chunkSize: 40000,
        overlap: 1000,
        customPrompt,
        templateId: selectedTemplate,
        summaryLanguage,
        replaceExisting: isRegeneration,
      });
      const processId = result.processId;
      if (!processId) {
        if (result.response) await handlePollingResult(result.response, generationId, isRegeneration);
        return;
      }
      // The owner registers the process before resolving this call. If this
      // route disappeared meanwhile, global tracking continues without it.
      if (!mountedRef.current || visibleMeetingIdRef.current !== meeting.id) return;
      if (generationId !== generationIdRef.current) return;
      activeProcessIdRef.current = processId;

      subscribeToProcess(processId, generationId, isRegeneration);
    } catch (error) {
      await failGeneration(generationId, isRegeneration, error instanceof Error ? error.message : 'Summary generation failed.');
    }
  }, [
    failGeneration,
    finishGeneration,
    handlePollingResult,
    meeting.created_at,
    meeting.id,
    effectiveModelConfig,
    selectedTemplate,
    startSummary,
    subscribeToProcess,
  ]);

  // Helper function to fetch ALL transcripts for summary generation
  const fetchAllTranscripts = useCallback(async (meetingId: string): Promise<Transcript[]> => {
    return fetchCompleteTranscripts(
      meetingId,
      (args) => invokeTauri('api_get_meeting_transcripts', args),
    );
  }, []);

  const buildSummaryTranscriptPayload = useCallback((allTranscripts: Transcript[]) => {
    const formatTime = (seconds: number | undefined, fallbackTimestamp: string): string => {
      if (seconds === undefined) {
        return fallbackTimestamp;
      }
      const totalSecs = Math.floor(seconds);
      return `[${Math.floor(totalSecs / 60).toString().padStart(2, '0')}:${(totalSecs % 60).toString().padStart(2, '0')}]`;
    };

    return {
      transcriptText: allTranscripts
        .map((transcript) => `${formatTime(transcript.audio_start_time, transcript.timestamp)}${transcript.speaker ? ` [${transcript.speaker === 'mic' ? 'You' : transcript.speaker === 'system' ? 'Others' : transcript.speaker}]` : ''} ${transcript.text}`)
        .join('\n'),
      transcriptTexts: allTranscripts.map((transcript) =>
        `${transcript.speaker ? `[${transcript.speaker === 'mic' ? 'You' : transcript.speaker === 'system' ? 'Others' : transcript.speaker}] ` : ''}${transcript.text}`
      ),
    };
  }, []);

  const withLiveNotesContext = useCallback(async (customPrompt: string): Promise<string> => {
    const notesContext = await loadSummaryNotesContext(meeting.id);
    return [customPrompt, notesContext].filter(Boolean).join('\n\n');
  }, [meeting.id]);

  const showPreflightError = useCallback((message: string) => {
    setSummaryError(message);
    setSummaryStatus('error');
    toast.error(message);
  }, []);

  const handleGenerateSummary = useCallback(async (customPrompt: string = '') => {
    if (modelConfigurationLoading || modelConfigurationSaving) {
      toast.info('Loading model configuration, please wait...');
      return;
    }
    if (modelConfigurationError) {
      showPreflightError('Model configuration could not be saved. Retry saving it before generating a summary.');
      return;
    }
    let allTranscripts: Transcript[];
    try {
      allTranscripts = await fetchAllTranscripts(meeting.id);
    } catch (error) {
      showPreflightError(`Could not read the complete transcript: ${error instanceof Error ? error.message : String(error)}`);
      return;
    }
    if (!allTranscripts.length) {
      showPreflightError('No transcripts available for summary');
      return;
    }

    try {
      await validateSummaryModel(effectiveModelConfig);
    } catch (error) {
      console.error('Failed to validate summary model:', error);
      showPreflightError(error instanceof Error ? error.message : 'Failed to validate summary model. Please check model settings.');
      onOpenModelSettings?.();
      return;
    }

    try {
      await processSummary({
        ...buildSummaryTranscriptPayload(allTranscripts),
        customPrompt: await withLiveNotesContext(customPrompt),
      });
    } catch (error) {
      showPreflightError(`Could not save current notes before summary generation: ${error instanceof Error ? error.message : String(error)}`);
    }
  }, [
    buildSummaryTranscriptPayload,
    fetchAllTranscripts,
    effectiveModelConfig,
    meeting.id,
    modelConfigurationError,
    modelConfigurationLoading,
    modelConfigurationSaving,
    onOpenModelSettings,
    processSummary,
    showPreflightError,
    withLiveNotesContext,
  ]);

  // Public API: Regenerate summary from the current saved transcript
  const handleRegenerateSummary = useCallback(async () => {
    if (modelConfigurationLoading || modelConfigurationSaving) {
      toast.info('Loading model configuration, please wait...');
      return;
    }
    if (modelConfigurationError) {
      showPreflightError('Model configuration could not be saved. Retry saving it before regenerating a summary.');
      return;
    }
    let allTranscripts: Transcript[];
    try {
      allTranscripts = await fetchAllTranscripts(meeting.id);
    } catch (error) {
      showPreflightError(`Could not read the complete transcript: ${error instanceof Error ? error.message : String(error)}`);
      return;
    }

    if (!allTranscripts.length) {
      console.error('No transcripts available for regeneration');
      toast.error('No transcripts available for summary regeneration');
      return;
    }

    try {
      await validateSummaryModel(effectiveModelConfig);
      await processSummary({
        ...buildSummaryTranscriptPayload(allTranscripts),
        customPrompt: await withLiveNotesContext(''),
        isRegeneration: true
      });
    } catch (error) {
      showPreflightError(`Could not save current notes before summary regeneration: ${error instanceof Error ? error.message : String(error)}`);
    }
  }, [
    buildSummaryTranscriptPayload,
    fetchAllTranscripts,
    effectiveModelConfig,
    meeting.id,
    modelConfigurationError,
    modelConfigurationLoading,
    modelConfigurationSaving,
    processSummary,
    showPreflightError,
    withLiveNotesContext,
  ]);

  // Public API: Stop ongoing summary generation
  const handleStopGeneration = useCallback(async () => {
    const generationId = generationIdRef.current;
    const processId = activeProcessIdRef.current;
    if (!processId) {
      generationIdRef.current += 1;
      activeProcessIdRef.current = null;
      setSummaryStatus('idle');
      setSummaryError(null);
      await finishGeneration(generationId, 'cancelled');
      toast.info('Summary generation stopped', {
        description: 'You can generate a new summary anytime',
        duration: 3000,
      });
      return;
    }

    try {
      const cancelled = await cancelSummary(meeting.id, processId);
      if (!mountedRef.current || visibleMeetingIdRef.current !== meeting.id || generationId !== generationIdRef.current) {
        return;
      }
      if (cancelled) {
        generationIdRef.current += 1;
        activeProcessIdRef.current = null;
        pollSubscriptionRef.current?.();
        pollSubscriptionRef.current = null;
        toast.info('Summary generation stopped', {
          description: 'You can generate a new summary anytime',
          duration: 3000,
        });
      } else if (activeProcessIdRef.current === processId) {
        toast.info('Summary is already finishing', {
          description: 'Waiting for the latest result.',
        });
      }
    } catch (error) {
      console.error('Failed to cancel summary generation:', error);
      if (
        generationId === generationIdRef.current
        && activeProcessIdRef.current === processId
      ) {
        toast.error('Failed to stop summary generation', {
          description: 'Generation is still running; waiting for its latest status.',
        });
      }
    }
  }, [cancelSummary, finishGeneration, meeting.id]);

  return {
    summaryStatus,
    summaryError,
    handleGenerateSummary,
    handleRegenerateSummary,
    handleStopGeneration,
    getSummaryStatusMessage,
  };
}
