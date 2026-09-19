'use client';

import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { appDataDir } from '@tauri-apps/api/path';
import { useRouter } from 'next/navigation';
import { useConfig } from '@/contexts/ConfigContext';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import { RecordingStatus, useRecordingState } from '@/contexts/RecordingStateContext';
import { useTranscripts } from '@/contexts/TranscriptContext';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import { settingsHref } from '@/components/settings/settingsSections';
import { cachedDebugMode } from '@/lib/debugMode';
import {
  LIVE_TRANSCRIPTION_STORAGE_KEY,
  isLiveTranscriptionEnabled,
  shouldDeferTranscription,
} from '@/lib/liveTranscription';
import { LIVE_NOTES_FALLBACK_FOLDER_KEY, LIVE_NOTES_FALLBACK_KEY } from '@/lib/liveNotes';
import {
  getProviderCommands,
  hasDownloadingModel,
  type ModelWithStatus,
} from '@/lib/transcription-model-readiness';
import { markDeferredMeetingForAutoSummary } from '@/lib/autoSummary';
import Analytics from '@/lib/analytics';
import { showRecordingNotification } from '@/lib/recordingNotification';
import {
  applyPinnedSummaryLanguageToMeeting,
  detectAndCacheSummaryLanguage,
} from '@/lib/summary-language-preferences';
import { flushNotes } from '@/services/notePersistenceService';
import { meetingActivityService } from '@/services/meetingActivityService';
import { recordingService, type RecordingStoppedPayload } from '@/services/recordingService';
import { storageService } from '@/services/storageService';
import { transcriptService } from '@/services/transcriptService';
import { indexedDBService } from '@/services/indexedDBService';
import type { RecordingStartRequest } from '@/types/meetingActivity';
import { useTranscriptRecovery, type UseTranscriptRecoveryReturn } from '@/hooks/useTranscriptRecovery';

const ACTIVE_MEETING_KEY = 'active_recording_meeting_id';
const CONTROLLER_CLAIMANT = 'main-recording-controller';
const RUNTIME_ERROR_CODE = 'TRANSCRIPTION_RUNTIME_INITIALIZATION_FAILED';
const RUNTIME_ERROR_MESSAGE = 'Speech recognition could not initialize. Restart Minutes. If the problem continues, repair or reinstall the app.';

export type RecordingCommand = 'start' | 'stop' | 'pause' | 'resume' | 'finalize' | null;
export type RecordingFeedbackKind = 'setup' | 'capture' | 'persistence' | 'warning' | 'recovery';

export interface RecordingFeedbackState {
  kind: RecordingFeedbackKind;
  title: string;
  message: string;
  settingsSection?: 'recording' | 'transcription';
}

export interface StartRecordingOptions {
  requestId?: string;
  source?: string;
}

export interface StopRecordingOptions {
  nativeAlreadyStopped?: boolean;
  saveMeeting?: boolean;
}

export interface RecordingControllerValue {
  activeMeetingId: string | null;
  sessionId: string | null;
  command: RecordingCommand;
  isCommandPending: boolean;
  feedback: RecordingFeedbackState | null;
  startRecording: (options?: StartRecordingOptions) => Promise<void>;
  stopRecording: (options?: StopRecordingOptions) => Promise<void>;
  pauseRecording: () => Promise<void>;
  resumeRecording: () => Promise<void>;
  returnToRecording: () => Promise<void>;
  dismissFeedback: () => void;
  canRetryFeedback: boolean;
  retryFeedback: () => Promise<void>;
  openFeedbackSettings: () => Promise<void>;
  recovery: UseTranscriptRecoveryReturn;
  isRecoveryOpen: boolean;
  openRecovery: () => void;
  closeRecovery: () => void;
}

interface SessionRecord {
  sessionId: string;
  meetingId: string | null;
  title: string;
  folderPath: string | null;
  recordingSeconds: number;
}

interface TranscriptConfig {
  provider?: string;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function meetingTitleNow(): string {
  const now = new Date();
  const parts = [
    now.getDate(),
    now.getMonth() + 1,
    String(now.getFullYear()).slice(-2),
    now.getHours(),
    now.getMinutes(),
    now.getSeconds(),
  ].map((part) => String(part).padStart(2, '0'));
  return `Meeting ${parts[0]}_${parts[1]}_${parts[2]}_${parts[3]}_${parts[4]}_${parts[5]}`;
}

async function recordingFolderPath(): Promise<string | null> {
  for (let attempt = 0; attempt < 12; attempt += 1) {
    const folderPath = await invoke<string | null>('get_meeting_folder_path').catch(() => null);
    if (folderPath) return folderPath;
    if (attempt < 11) await new Promise((resolve) => setTimeout(resolve, 250));
  }
  return null;
}

const RecordingControllerContext = createContext<RecordingControllerValue | null>(null);

export function useRecordingController(): RecordingControllerValue {
  const context = useContext(RecordingControllerContext);
  if (!context) {
    throw new Error('useRecordingController must be used within a RecordingControllerProvider');
  }
  return context;
}

export function useOptionalRecordingController(): RecordingControllerValue | null {
  return useContext(RecordingControllerContext);
}

export function RecordingControllerProvider({ children }: { children: React.ReactNode }) {
  const router = useRouter();
  const { selectedDevices, betaFeatures, selectedLanguage, transcriptModelConfig } = useConfig();
  const { recording, activeMeetingId: authoritativeMeetingId, rehydrate } = useMeetingActivity();
  const recordingState = useRecordingState();
  const {
    transcriptsRef,
    flushBuffer,
    clearTranscripts,
    setMeetingTitle,
    markMeetingAsSaved,
  } = useTranscripts();
  const { setCurrentMeeting, setIsMeetingActive, refetchMeetings } = useSidebar();
  const recovery = useTranscriptRecovery();

  const [command, setCommand] = useState<RecordingCommand>(null);
  const [feedback, setFeedback] = useState<RecordingFeedbackState | null>(null);
  const [isRecoveryOpen, setIsRecoveryOpen] = useState(false);
  const startPromiseRef = useRef<Promise<void> | null>(null);
  const stopPromiseRef = useRef<Promise<void> | null>(null);
  const pausePromiseRef = useRef<Promise<void> | null>(null);
  const resumePromiseRef = useRef<Promise<void> | null>(null);
  const finalizePromisesRef = useRef(new Map<string, Promise<void>>());
  const sessionsRef = useRef(new Map<string, SessionRecord>());
  const latestSessionIdRef = useRef<string | null>(null);
  const stoppedMetadataRef = useRef<RecordingStoppedPayload | null>(null);
  const handledRequestsRef = useRef(new Set<string>());
  const requestHandlerRef = useRef<(request: RecordingStartRequest) => void>(() => {});
  const startHandlerRef = useRef<(options?: StartRecordingOptions) => Promise<void>>(async () => {});
  const stopHandlerRef = useRef<(options?: StopRecordingOptions) => Promise<void>>(async () => {});
  const retryActionRef = useRef<(() => Promise<void>) | null>(null);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);

  useEffect(() => {
    if (!recording) return;
    latestSessionIdRef.current = recording.session_id;
    const current = sessionsRef.current.get(recording.session_id);
    sessionsRef.current.set(recording.session_id, {
      sessionId: recording.session_id,
      meetingId: recording.meeting_id ?? current?.meetingId ?? null,
      title: current?.title ?? 'New Meeting',
      folderPath: current?.folderPath ?? null,
      recordingSeconds: current?.recordingSeconds ?? 0,
    });
  }, [recording]);

  const reportError = useCallback((
    kind: RecordingFeedbackKind,
    title: string,
    error: unknown,
    settingsSection?: RecordingFeedbackState['settingsSection'],
    retry?: () => Promise<void>,
  ) => {
    retryActionRef.current = retry ?? null;
    setFeedback({ kind, title, message: messageOf(error), settingsSection });
  }, []);

  const checkModelReady = useCallback(async (): Promise<'ready' | 'downloading' | 'missing'> => {
    const config = await invoke<TranscriptConfig | null>('api_get_transcript_config').catch(() => null);
    const commands = getProviderCommands(config?.provider || 'parakeet');
    if (!commands) return 'missing';
    try {
      await invoke(commands.initialize);
      if (await invoke<boolean>(commands.hasAvailableModels)) return 'ready';
      const models = await invoke<ModelWithStatus[]>(commands.getAvailableModels).catch(() => []);
      return hasDownloadingModel(models) ? 'downloading' : 'missing';
    } catch {
      return 'missing';
    }
  }, []);

  const persistStartedSession = useCallback(async (session: SessionRecord) => {
    try {
      session.folderPath = await recordingFolderPath();
      const created = await storageService.createMeeting(
        session.title,
        session.folderPath,
        cachedDebugMode(),
      );
      if (!created.meeting_id) throw new Error('The meeting row was not created.');
      session.meetingId = created.meeting_id;
      sessionsRef.current.set(session.sessionId, session);
      await meetingActivityService.bindActiveRecordingMeeting(session.sessionId, created.meeting_id);
      sessionStorage.setItem(ACTIVE_MEETING_KEY, created.meeting_id);
      setCurrentMeeting({ id: created.meeting_id, title: session.title });
      await refetchMeetings();
      await flushNotes();
      router.push(`/meeting-details?id=${encodeURIComponent(created.meeting_id)}`);
    } catch (error) {
      reportError(
        'persistence',
        'Recording started, but its meeting could not be prepared',
        `${messageOf(error)} Recording is still active and can be stopped safely.`,
      );
    }
  }, [refetchMeetings, reportError, router, setCurrentMeeting]);

  const performStart = useCallback(async (options: StartRecordingOptions = {}) => {
    if (recording && ['starting', 'recording', 'paused', 'saving'].includes(recording.status)) {
      if (recording.meeting_id) await flushNotes().then(() => router.push(`/meeting-details?id=${encodeURIComponent(recording.meeting_id!)}`));
      return;
    }

    setCommand('start');
    setFeedback(null);
    const liveTranscription = isLiveTranscriptionEnabled(
      localStorage.getItem(LIVE_TRANSCRIPTION_STORAGE_KEY),
      betaFeatures.liveTranscription,
    );

    try {
      await invoke('set_live_transcription_enabled', { enabled: liveTranscription });
      if (liveTranscription) {
        const readiness = await checkModelReady();
        if (readiness !== 'ready') {
          const error = readiness === 'downloading'
            ? 'The transcription model is still downloading. Wait for it to finish, then try again.'
            : 'Download a transcription model before starting live transcription.';
          reportError('setup', 'Transcription setup required', error, 'transcription');
          if (options.requestId) {
            await meetingActivityService.acknowledgeRecordingRequest(options.requestId, false, error);
          }
          return;
        }
      }

      const title = meetingTitleNow();
      setMeetingTitle(title);
      recordingState.setStatus(RecordingStatus.STARTING, 'Initializing recording...');
      const result = await recordingService.startRecordingWithDevices(
        selectedDevices?.micDevice ?? null,
        selectedDevices?.systemDevice ?? null,
        title,
        options.requestId,
      );
      const session: SessionRecord = {
        sessionId: result.session_id,
        meetingId: null,
        title,
        folderPath: null,
        recordingSeconds: 0,
      };
      latestSessionIdRef.current = result.session_id;
      sessionsRef.current.set(result.session_id, session);
      clearTranscripts();
      setIsMeetingActive(true);
      Analytics.trackButtonClick('start_recording', options.source ?? 'recording_controller');
      await showRecordingNotification().catch((error) => {
        console.warn('Could not show the recording notification:', error);
      });
      await persistStartedSession(session);
      await rehydrate();
    } catch (error) {
      const text = messageOf(error);
      const runtimeError = text === RUNTIME_ERROR_CODE;
      reportError(
        runtimeError ? 'setup' : 'capture',
        'Recording could not start',
        runtimeError ? RUNTIME_ERROR_MESSAGE : text,
        runtimeError ? 'transcription' : 'recording',
      );
      await rehydrate().catch(() => {});
      throw error;
    } finally {
      if (mountedRef.current) setCommand(null);
    }
  }, [
    betaFeatures.liveTranscription,
    checkModelReady,
    clearTranscripts,
    persistStartedSession,
    recording,
    recordingState,
    rehydrate,
    reportError,
    router,
    selectedDevices,
    setIsMeetingActive,
    setMeetingTitle,
  ]);

  const startRecording = useCallback((options: StartRecordingOptions = {}): Promise<void> => {
    if (startPromiseRef.current) return startPromiseRef.current;
    const promise = performStart(options).finally(() => {
      if (startPromiseRef.current === promise) startPromiseRef.current = null;
    });
    startPromiseRef.current = promise;
    return promise;
  }, [performStart]);
  startHandlerRef.current = startRecording;

  const waitForLiveTranscription = useCallback(async (deferred: boolean) => {
    if (deferred) return;
    let complete = false;
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    try {
      unlisten = await transcriptService.onTranscriptionComplete(() => { complete = true; });
      for (let elapsed = 0; elapsed < 60_000 && !complete; elapsed += 500) {
        const status = await transcriptService.getTranscriptionStatus();
        if (!status.is_processing && status.chunks_in_queue === 0) break;
        await new Promise((resolve) => setTimeout(resolve, 500));
      }
    } finally {
      disposed = true;
      if (disposed) unlisten?.();
    }
  }, []);

  const finalizeRecording = useCallback((sessionId: string, saveMeeting = true): Promise<void> => {
    const existing = finalizePromisesRef.current.get(sessionId);
    if (existing) return existing;
    const promise = (async () => {
      setCommand('finalize');
      recordingState.setStatus(RecordingStatus.PROCESSING_TRANSCRIPTS, 'Finishing transcription...');
      const session = sessionsRef.current.get(sessionId) ?? {
        sessionId,
        meetingId: authoritativeMeetingId,
        title: stoppedMetadataRef.current?.meeting_name ?? 'New Meeting',
        folderPath: stoppedMetadataRef.current?.folder_path ?? null,
        recordingSeconds: 0,
      };
      session.folderPath = stoppedMetadataRef.current?.folder_path ?? session.folderPath;
      session.title = stoppedMetadataRef.current?.meeting_name ?? session.title;

      if (!saveMeeting) {
        recordingState.setStatus(RecordingStatus.IDLE);
        return;
      }

      try {
        if (session.folderPath) await flushNotes({ sessionId: session.folderPath });
        const deferred = shouldDeferTranscription(
          localStorage.getItem(LIVE_TRANSCRIPTION_STORAGE_KEY),
          betaFeatures.liveTranscription,
        );
        await waitForLiveTranscription(deferred);
        flushBuffer();
        await new Promise((resolve) => setTimeout(resolve, 0));
        recordingState.setStatus(RecordingStatus.SAVING, 'Saving meeting...');
        const freshTranscripts = [...transcriptsRef.current];
        const saved = await storageService.saveMeeting(
          session.title,
          freshTranscripts,
          session.folderPath,
          !deferred,
          session.meetingId,
        );
        if (!saved.meeting_id) throw new Error('No meeting ID was returned while saving.');
        session.meetingId = saved.meeting_id;

        if (session.folderPath) {
          await flushNotes({ sessionId: session.folderPath });
          await invoke('attach_live_notes', {
            meetingId: saved.meeting_id,
            folderPath: session.folderPath,
          });
        }

        const preferences = await invoke<{ min_meeting_duration_seconds?: number }>(
          'get_recording_preferences',
        ).catch(() => null);
        const minimumSeconds = preferences?.min_meeting_duration_seconds ?? 10;
        const transcriptSeconds = freshTranscripts.reduce(
          (maximum, transcript) => Math.max(maximum, transcript.audio_end_time ?? 0),
          0,
        );
        const notes = await invoke<{ rawMarkdown?: string; notes?: Array<{ text: string }> } | null>(
          'get_meeting_live_notes',
          { meetingId: saved.meeting_id },
        ).catch(() => null);
        const hasNotes = Boolean(notes && (
          (notes.rawMarkdown ?? '').trim()
          || (notes.notes ?? []).some((note) => note.text.trim())
        ));
        const hasTranscript = freshTranscripts.some((transcript) => transcript.text.trim());
        if (
          minimumSeconds > 0
          && Math.max(session.recordingSeconds, transcriptSeconds) < minimumSeconds
          && !hasNotes
          && !hasTranscript
        ) {
          await invoke('api_discard_meeting', { meetingId: saved.meeting_id });
          await markMeetingAsSaved();
          localStorage.removeItem(LIVE_NOTES_FALLBACK_KEY);
          localStorage.removeItem(LIVE_NOTES_FALLBACK_FOLDER_KEY);
          sessionStorage.removeItem(ACTIVE_MEETING_KEY);
          await refetchMeetings();
          setIsMeetingActive(false);
          clearTranscripts();
          recordingState.setStatus(RecordingStatus.IDLE);
          return;
        }

        if (deferred) {
          if (!session.folderPath) throw new Error('The recording has no audio folder for deferred transcription.');
          await invoke('start_retranscription_command', {
            meetingId: saved.meeting_id,
            meetingFolderPath: session.folderPath,
            language: selectedLanguage === 'auto' || selectedLanguage === 'auto-translate' ? null : selectedLanguage,
            model: transcriptModelConfig.model || null,
            provider: transcriptModelConfig.provider || null,
          });
          markDeferredMeetingForAutoSummary(saved.meeting_id);
        } else {
          const pinned = await applyPinnedSummaryLanguageToMeeting(saved.meeting_id).catch(() => false);
          if (!pinned) {
            await detectAndCacheSummaryLanguage(
              saved.meeting_id,
              freshTranscripts.map((transcript) => transcript.text),
            ).catch(() => {});
          }
          window.dispatchEvent(new CustomEvent('meetily:meeting-ready-for-summary', {
            detail: { meetingId: saved.meeting_id },
          }));
          void invoke<{ speaker_count: number }>('run_speaker_diarization', {
            meetingId: saved.meeting_id,
            numSpeakers: null,
          }).catch((error) => console.warn('Automatic speaker identification skipped:', error));
        }

        await markMeetingAsSaved();
        localStorage.removeItem(LIVE_NOTES_FALLBACK_KEY);
        localStorage.removeItem(LIVE_NOTES_FALLBACK_FOLDER_KEY);
        sessionStorage.removeItem(ACTIVE_MEETING_KEY);
        setCurrentMeeting({ id: saved.meeting_id, title: session.title });
        await refetchMeetings();
        setIsMeetingActive(false);
        clearTranscripts();
        recordingState.setStatus(RecordingStatus.IDLE);
        window.dispatchEvent(new CustomEvent('meetily:recording-finalized', {
          detail: { meetingId: saved.meeting_id, transcribing: deferred },
        }));
        const durationSeconds = Math.max(session.recordingSeconds, transcriptSeconds);
        const wordCount = freshTranscripts.reduce(
          (count, transcript) => count + transcript.text.split(/\s+/).filter(Boolean).length,
          0,
        );
        void (async () => {
          const meetingsToday = await Analytics.getMeetingsCountToday();
          await Analytics.trackMeetingCompleted(saved.meeting_id, {
            duration_seconds: durationSeconds,
            transcript_segments: freshTranscripts.length,
            transcript_word_count: wordCount,
            words_per_minute: durationSeconds > 0 ? wordCount / (durationSeconds / 60) : 0,
            meetings_today: meetingsToday,
          });
          await Analytics.updateMeetingCount();
        })().catch((error) => console.warn('Could not record meeting completion analytics:', error));
      } catch (error) {
        recordingState.setStatus(RecordingStatus.ERROR, messageOf(error));
        reportError(
          'persistence',
          'Meeting needs attention',
          `${messageOf(error)} Your local draft and recording files were kept so you can retry.`,
          undefined,
          () => finalizeRecording(sessionId, saveMeeting),
        );
        throw error;
      } finally {
        if (mountedRef.current) setCommand(null);
      }
    })().finally(() => finalizePromisesRef.current.delete(sessionId));
    finalizePromisesRef.current.set(sessionId, promise);
    return promise;
  }, [
    authoritativeMeetingId,
    betaFeatures.liveTranscription,
    clearTranscripts,
    flushBuffer,
    markMeetingAsSaved,
    recordingState,
    refetchMeetings,
    reportError,
    selectedLanguage,
    setCurrentMeeting,
    setIsMeetingActive,
    transcriptModelConfig.model,
    transcriptModelConfig.provider,
    transcriptsRef,
    waitForLiveTranscription,
  ]);

  const performStop = useCallback(async (options: StopRecordingOptions = {}) => {
    const sessionId = recording?.session_id ?? latestSessionIdRef.current;
    if (!sessionId) return;
    const session = sessionsRef.current.get(sessionId);
    setCommand('stop');
    setFeedback(null);
    try {
      if (!options.nativeAlreadyStopped) {
        const folderPath = session?.folderPath ?? await recordingFolderPath();
        if (session) {
          session.folderPath = folderPath;
          session.recordingSeconds = recordingState.recordingDuration ?? session.recordingSeconds;
        }
        if (folderPath) await flushNotes({ sessionId: folderPath });
        const dataDir = await appDataDir();
        const savePath = `${dataDir}/recording-${new Date().toISOString().replace(/[:.]/g, '-')}.wav`;
        recordingState.setStatus(RecordingStatus.STOPPING, 'Stopping recording...');
        await recordingService.stopRecording(savePath);
      }
      await finalizeRecording(sessionId, options.saveMeeting ?? true);
      await rehydrate();
    } catch (error) {
      await rehydrate().catch(() => {});
      if (recording?.session_id === sessionId) {
        reportError('capture', 'Recording is still active', error, 'recording', () => stopHandlerRef.current(options));
      }
      throw error;
    } finally {
      if (mountedRef.current) setCommand(null);
    }
  }, [finalizeRecording, recording?.session_id, recordingState, rehydrate, reportError]);

  const stopRecording = useCallback((options: StopRecordingOptions = {}): Promise<void> => {
    if (stopPromiseRef.current) return stopPromiseRef.current;
    const promise = performStop(options).finally(() => {
      if (stopPromiseRef.current === promise) stopPromiseRef.current = null;
    });
    stopPromiseRef.current = promise;
    return promise;
  }, [performStop]);
  stopHandlerRef.current = stopRecording;

  const performPause = useCallback(async () => {
    if (!recording || recording.status !== 'recording' || command) return;
    setCommand('pause');
    setFeedback(null);
    try {
      await recordingService.pauseRecording();
      await rehydrate();
    } catch (error) {
      reportError(
        'capture',
        'Recording could not be paused',
        error,
        'recording',
        async () => { await recordingService.pauseRecording(); await rehydrate(); },
      );
      await rehydrate().catch(() => {});
      throw error;
    } finally {
      if (mountedRef.current) setCommand(null);
    }
  }, [command, recording, rehydrate, reportError]);

  const pauseRecording = useCallback((): Promise<void> => {
    if (pausePromiseRef.current) return pausePromiseRef.current;
    const promise = performPause().finally(() => {
      if (pausePromiseRef.current === promise) pausePromiseRef.current = null;
    });
    pausePromiseRef.current = promise;
    return promise;
  }, [performPause]);

  const performResume = useCallback(async () => {
    if (!recording || recording.status !== 'paused' || command) return;
    setCommand('resume');
    setFeedback(null);
    try {
      await recordingService.resumeRecording();
      await rehydrate();
    } catch (error) {
      reportError(
        'capture',
        'Recording could not be resumed',
        error,
        'recording',
        async () => { await recordingService.resumeRecording(); await rehydrate(); },
      );
      await rehydrate().catch(() => {});
      throw error;
    } finally {
      if (mountedRef.current) setCommand(null);
    }
  }, [command, recording, rehydrate, reportError]);

  const resumeRecording = useCallback((): Promise<void> => {
    if (resumePromiseRef.current) return resumePromiseRef.current;
    const promise = performResume().finally(() => {
      if (resumePromiseRef.current === promise) resumePromiseRef.current = null;
    });
    resumePromiseRef.current = promise;
    return promise;
  }, [performResume]);

  const returnToRecording = useCallback(async () => {
    const meetingId = authoritativeMeetingId;
    if (!meetingId) {
      reportError('persistence', 'Recording workspace unavailable', 'The recording is active, but its meeting row has not been created yet.');
      return;
    }
    try {
      await flushNotes();
      router.push(`/meeting-details?id=${encodeURIComponent(meetingId)}`);
    } catch (error) {
      reportError('persistence', 'Notes could not be saved before navigation', error);
      throw error;
    }
  }, [authoritativeMeetingId, reportError, router]);

  requestHandlerRef.current = (request) => {
    if (handledRequestsRef.current.has(request.request_id)) return;
    handledRequestsRef.current.add(request.request_id);
    void (async () => {
      try {
        if (request.status === 'starting') return;
        if (request.status === 'pending') {
          await meetingActivityService.claimRecordingRequest(request.request_id, CONTROLLER_CLAIMANT);
        } else if (request.claimed_by && request.claimed_by !== CONTROLLER_CLAIMANT) {
          return;
        }
        await startRecording({ requestId: request.request_id, source: request.source });
      } catch (error) {
        handledRequestsRef.current.delete(request.request_id);
        reportError('capture', 'Recording request failed', error, 'recording');
      }
    })();
  };

  useEffect(() => {
    let disposed = false;
    let unlistenRequest: UnlistenFn | undefined;
    let unlistenStopped: UnlistenFn | undefined;
    let unlistenTrayStop: UnlistenFn | undefined;
    let unlistenTranscriptionError: UnlistenFn | undefined;
    let unlistenChunkWarning: UnlistenFn | undefined;

    void (async () => {
      try {
        unlistenRequest = await meetingActivityService.onRecordingStartRequested((request) => {
          requestHandlerRef.current(request);
        });
        if (disposed) { unlistenRequest(); return; }
        const pending = await meetingActivityService.getPendingRecordingRequest();
        if (pending && !disposed) requestHandlerRef.current(pending);

        unlistenStopped = await recordingService.onRecordingStopped((payload) => {
          stoppedMetadataRef.current = payload;
          const sessionId = latestSessionIdRef.current;
          if (sessionId) {
            const session = sessionsRef.current.get(sessionId);
            if (session) {
              session.folderPath = payload.folder_path ?? session.folderPath;
              session.title = payload.meeting_name ?? session.title;
            }
          }
          if (payload.error) reportError('capture', 'Recording stopped with an error', payload.error, 'recording');
        });
        if (disposed) { unlistenStopped(); return; }

        unlistenTrayStop = await listen<boolean>('recording-stop-complete', (event) => {
          const sessionId = latestSessionIdRef.current;
          if (sessionId) void stopHandlerRef.current({ nativeAlreadyStopped: true, saveMeeting: event.payload }).catch(() => {});
        });
        if (disposed) { unlistenTrayStop(); return; }

        unlistenTranscriptionError = await transcriptService.onTranscriptionError((error) => {
          if (error.phase === 'startup' || error.actionable) {
            reportError('setup', 'Transcription needs attention', error.userMessage || error.error, 'transcription');
          } else {
            setFeedback({
              kind: 'warning',
              title: 'Live transcription issue',
              message: error.userMessage || error.error,
            });
          }
        });
        if (disposed) { unlistenTranscriptionError(); return; }

        unlistenChunkWarning = await recordingService.onChunkDropWarning((warning) => {
          setFeedback({ kind: 'warning', title: 'Some audio could not be processed', message: warning });
        });
        if (disposed) unlistenChunkWarning();
      } catch (error) {
        if (!disposed) reportError('capture', 'Recording controls could not initialize', error);
      }
    })();

    return () => {
      disposed = true;
      unlistenRequest?.();
      unlistenStopped?.();
      unlistenTrayStop?.();
      unlistenTranscriptionError?.();
      unlistenChunkWarning?.();
    };
  }, [reportError]);

  useEffect(() => {
    const startFromCompatibilityEntry = () => {
      void startHandlerRef.current({ source: 'sidebar_compat' }).catch(() => {});
    };
    window.addEventListener('start-recording-from-sidebar', startFromCompatibilityEntry);
    if (sessionStorage.getItem('autoStartRecording') === 'true') {
      sessionStorage.removeItem('autoStartRecording');
      startFromCompatibilityEntry();
    }
    return () => window.removeEventListener('start-recording-from-sidebar', startFromCompatibilityEntry);
  }, []);

  useEffect(() => {
    if (recording || command) return;
    let disposed = false;
    void (async () => {
      await indexedDBService.deleteOldMeetings(7).catch(() => {});
      await indexedDBService.deleteSavedMeetings(24).catch(() => {});
      await recovery.checkForRecoverableTranscripts();
      if (!disposed && recovery.recoverableMeetings.length > 0) setIsRecoveryOpen(true);
    })();
    return () => { disposed = true; };
    // Recovery performs one process-level startup scan; later scans are explicit.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!recording && recovery.recoverableMeetings.length > 0) setIsRecoveryOpen(true);
  }, [recording, recovery.recoverableMeetings.length]);

  const openFeedbackSettings = useCallback(async () => {
    if (!feedback?.settingsSection) return;
    try {
      await flushNotes();
      router.push(settingsHref(feedback.settingsSection));
    } catch (error) {
      reportError('persistence', 'Notes could not be saved before navigation', error);
    }
  }, [feedback?.settingsSection, reportError, router]);

  const retryFeedback = useCallback(async () => {
    const retry = retryActionRef.current;
    if (!retry) return;
    setFeedback(null);
    await retry();
  }, []);

  const value = useMemo<RecordingControllerValue>(() => ({
    activeMeetingId: authoritativeMeetingId,
    sessionId: recording?.session_id ?? latestSessionIdRef.current,
    command,
    isCommandPending: command !== null,
    feedback,
    startRecording,
    stopRecording,
    pauseRecording,
    resumeRecording,
    returnToRecording,
    dismissFeedback: () => {
      retryActionRef.current = null;
      setFeedback(null);
    },
    canRetryFeedback: retryActionRef.current !== null,
    retryFeedback,
    openFeedbackSettings,
    recovery,
    isRecoveryOpen,
    openRecovery: () => setIsRecoveryOpen(true),
    closeRecovery: () => setIsRecoveryOpen(false),
  }), [
    authoritativeMeetingId,
    command,
    feedback,
    isRecoveryOpen,
    openFeedbackSettings,
    pauseRecording,
    recording?.session_id,
    recovery,
    retryFeedback,
    resumeRecording,
    returnToRecording,
    startRecording,
    stopRecording,
  ]);

  return (
    <RecordingControllerContext.Provider value={value}>
      {children}
    </RecordingControllerContext.Provider>
  );
}
