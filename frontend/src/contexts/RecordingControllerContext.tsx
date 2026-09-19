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
import { appDataDir, join } from '@tauri-apps/api/path';
import { readTextFile } from '@tauri-apps/plugin-fs';
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
import { meetingActivityService } from '@/services/meetingActivityService';
import { recordingService } from '@/services/recordingService';
import { storageService } from '@/services/storageService';
import { transcriptService } from '@/services/transcriptService';
import { indexedDBService } from '@/services/indexedDBService';
import type { RecordingStartRequest } from '@/types/meetingActivity';
import { useTranscriptRecovery, type UseTranscriptRecoveryReturn } from '@/hooks/useTranscriptRecovery';
import {
  flushNotes,
  meetingNotesTarget,
  notePersistenceService,
} from '@/services/notePersistenceService';
import { originalNotesMarkdown } from '@/lib/meetingExport';
import type { LiveNotesDocument } from '@/lib/liveNotes';
import type { Transcript } from '@/types';

const ACTIVE_MEETING_KEY = 'active_recording_meeting_id';
const ACTIVE_SESSIONS_KEY = 'active_recording_sessions';
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
  recordingSeconds: number | null;
  recoveryMeetingId?: string | null;
  finalization?: PersistedFinalizationRecord;
}

interface PersistedFinalizationRecord {
  completed: boolean;
  deferred: boolean | null;
  savedMeetingId: string | null;
  notesAttached: boolean;
  postProcessingStarted: boolean;
  markedSaved: boolean;
  catalogRefreshed: boolean;
  discarded: boolean;
}

interface FinalizationRecord extends PersistedFinalizationRecord {
  promise: Promise<void> | null;
  transcripts: Transcript[] | null;
  attachedNotesRead: boolean;
  attachedNotes: LiveNotesDocument | null;
}

interface LifecycleOperation {
  kind: Exclude<RecordingCommand, null>;
  sessionId: string | null;
  promise: Promise<void>;
}

class LifecycleBusyError extends Error {}

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

function readPersistedSessions(): Map<string, SessionRecord> {
  try {
    const value = sessionStorage.getItem(ACTIVE_SESSIONS_KEY);
    if (!value) return new Map();
    const records = JSON.parse(value) as SessionRecord[];
    return new Map(records.map((record) => [record.sessionId, record]));
  } catch {
    return new Map();
  }
}

function persistSessions(sessions: Map<string, SessionRecord>): void {
  sessionStorage.setItem(ACTIVE_SESSIONS_KEY, JSON.stringify([...sessions.values()]));
}

function persistedFinalization(state: FinalizationRecord): PersistedFinalizationRecord {
  return {
    completed: state.completed,
    deferred: state.deferred,
    savedMeetingId: state.savedMeetingId,
    notesAttached: state.notesAttached,
    postProcessingStarted: state.postProcessingStarted,
    markedSaved: state.markedSaved,
    catalogRefreshed: state.catalogRefreshed,
    discarded: state.discarded,
  };
}

function checkpointFinalization(
  session: SessionRecord,
  state: FinalizationRecord,
  sessions: Map<string, SessionRecord>,
): void {
  session.finalization = persistedFinalization(state);
  sessions.set(session.sessionId, session);
  persistSessions(sessions);
}

function restoreFinalizations(sessions: Map<string, SessionRecord>): Map<string, FinalizationRecord> {
  return new Map([...sessions.values()]
    .filter((session) => session.finalization)
    .map((session) => [session.sessionId, {
      ...session.finalization!,
      promise: null,
      transcripts: null,
      attachedNotesRead: false,
      attachedNotes: null,
    }]));
}

function isNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value);
}

async function readPersistedTranscripts(folderPath: string): Promise<Transcript[]> {
  const transcriptPath = await join(folderPath, 'transcripts.json');
  const parsed = JSON.parse(await readTextFile(transcriptPath)) as {
    version?: unknown;
    total_segments?: unknown;
    segments?: unknown;
  };
  if (parsed.version !== '1.0' || !Number.isInteger(parsed.total_segments) || !Array.isArray(parsed.segments)) {
    throw new Error('The saved transcript file has an invalid structure.');
  }
  if (parsed.total_segments !== parsed.segments.length) {
    throw new Error('The saved transcript file is incomplete.');
  }
  const sequences = new Set<number>();
  return parsed.segments.map((value, index) => {
    if (!value || typeof value !== 'object') throw new Error(`Transcript segment ${index} is invalid.`);
    const segment = value as Record<string, unknown>;
    if (
      typeof segment.id !== 'string'
      || typeof segment.text !== 'string'
      || typeof segment.display_time !== 'string'
      || !isNumber(segment.sequence_id)
      || !Number.isInteger(segment.sequence_id)
      || !isNumber(segment.audio_start_time)
      || !isNumber(segment.audio_end_time)
      || !isNumber(segment.duration)
      || !isNumber(segment.confidence)
      || !(segment.speaker === null || segment.speaker === undefined || typeof segment.speaker === 'string')
      || sequences.has(segment.sequence_id)
    ) {
      throw new Error(`Transcript segment ${index} is invalid.`);
    }
    sequences.add(segment.sequence_id);
    return {
      id: segment.id,
      text: segment.text,
      timestamp: segment.display_time,
      speaker: typeof segment.speaker === 'string' ? segment.speaker : undefined,
      sequence_id: segment.sequence_id,
      chunk_start_time: segment.audio_start_time,
      is_partial: false,
      confidence: segment.confidence,
      audio_start_time: segment.audio_start_time,
      audio_end_time: segment.audio_end_time,
      duration: segment.duration,
    };
  }).sort((left, right) => (left.sequence_id ?? 0) - (right.sequence_id ?? 0));
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
  const { recording, activeMeetingId: authoritativeMeetingId, snapshot, rehydrate } = useMeetingActivity();
  const recordingState = useRecordingState();
  const {
    clearTranscripts,
    setMeetingTitle,
  } = useTranscripts();
  const { setCurrentMeeting, setIsMeetingActive, refetchMeetings } = useSidebar();
  const recovery = useTranscriptRecovery();

  const [command, setCommand] = useState<RecordingCommand>(null);
  const [feedback, setFeedback] = useState<RecordingFeedbackState | null>(null);
  const [isRecoveryOpen, setIsRecoveryOpen] = useState(false);
  const lifecycleOperationRef = useRef<LifecycleOperation | null>(null);
  const sessionsRef = useRef(readPersistedSessions());
  const finalizationsRef = useRef(restoreFinalizations(sessionsRef.current));
  const latestSessionIdRef = useRef<string | null>([...sessionsRef.current.keys()].at(-1) ?? null);
  const durationSessionIdRef = useRef<string | null>(null);
  const handledRequestsRef = useRef(new Set<string>());
  const requestHandlerRef = useRef<(request: RecordingStartRequest) => void>(() => {});
  const startHandlerRef = useRef<(options?: StartRecordingOptions) => Promise<void>>(async () => {});
  const stopHandlerRef = useRef<(options?: StopRecordingOptions) => Promise<void>>(async () => {});
  const finalizeHandlerRef = useRef<(sessionId: string, saveMeeting?: boolean) => Promise<void>>(async () => {});
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
      recordingSeconds: current?.recordingSeconds ?? null,
      recoveryMeetingId: current?.recoveryMeetingId
        ?? sessionStorage.getItem('indexeddb_current_meeting_id'),
      finalization: current?.finalization,
    });
    persistSessions(sessionsRef.current);
  }, [recording]);

  useEffect(() => {
    if (!recording) return;
    const sessionId = recording.session_id;
    let disposed = false;
    void (async () => {
      const nativeState = await recordingService.getRecordingState().catch(() => null);
      if (disposed || nativeState?.session_id !== sessionId) return;
      const current = sessionsRef.current.get(sessionId) ?? {
        sessionId,
        meetingId: recording.meeting_id,
        title: 'New Meeting',
        folderPath: null,
        recordingSeconds: null,
        recoveryMeetingId: sessionStorage.getItem('indexeddb_current_meeting_id'),
      };
      const [folderPath, title] = await Promise.all([
        current.folderPath ? Promise.resolve(current.folderPath) : recordingFolderPath(),
        current.title !== 'New Meeting'
          ? Promise.resolve(current.title)
          : recordingService.getRecordingMeetingName().catch(() => null),
      ]);
      if (disposed) return;
      current.meetingId = recording.meeting_id ?? current.meetingId;
      current.folderPath = folderPath ?? current.folderPath;
      current.title = title ?? current.title;
      current.recordingSeconds = nativeState.recording_duration ?? current.recordingSeconds;
      sessionsRef.current.set(sessionId, current);
      persistSessions(sessionsRef.current);
      if (current.meetingId && !recording.meeting_id) {
        await meetingActivityService.bindActiveRecordingMeeting(sessionId, current.meetingId).catch(() => {});
        if (!disposed) await rehydrate().catch(() => {});
      }
    })();
    return () => { disposed = true; };
  }, [recording, rehydrate]);

  useEffect(() => {
    const sessionId = recording?.session_id ?? durationSessionIdRef.current;
    if (!sessionId || recordingState.recordingDuration == null) return;
    if (recording && durationSessionIdRef.current !== recording.session_id) {
      durationSessionIdRef.current = recording.session_id;
      return;
    }
    const session = sessionsRef.current.get(sessionId);
    if (!session) return;
    session.recordingSeconds = Math.max(session.recordingSeconds ?? 0, recordingState.recordingDuration);
    persistSessions(sessionsRef.current);
  }, [recording, recordingState.recordingDuration]);

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
      session.recoveryMeetingId ??= sessionStorage.getItem('indexeddb_current_meeting_id');
      sessionsRef.current.set(session.sessionId, session);
      persistSessions(sessionsRef.current);
      const created = await storageService.createMeeting(
        session.title,
        session.folderPath,
        cachedDebugMode(),
      );
      if (!created.meeting_id) throw new Error('The meeting row was not created.');
      session.meetingId = created.meeting_id;
      sessionsRef.current.set(session.sessionId, session);
      sessionStorage.setItem(ACTIVE_MEETING_KEY, created.meeting_id);
      persistSessions(sessionsRef.current);
      await meetingActivityService.bindActiveRecordingMeeting(session.sessionId, created.meeting_id);
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

    let nativeStartInvoked = false;
    let requestRejected = false;
    const rejectRequest = async (error: string) => {
      if (!options.requestId || nativeStartInvoked || requestRejected) return;
      requestRejected = true;
      await meetingActivityService.acknowledgeRecordingRequest(options.requestId, false, error).catch(() => {});
    };

    try {
      await invoke('set_live_transcription_enabled', { enabled: liveTranscription });
      if (liveTranscription) {
        const readiness = await checkModelReady();
        if (readiness !== 'ready') {
          const error = readiness === 'downloading'
            ? 'The transcription model is still downloading. Wait for it to finish, then try again.'
            : 'Download a transcription model before starting live transcription.';
          reportError('setup', 'Transcription setup required', error, 'transcription');
          await rejectRequest(error);
          return;
        }
      }

      const title = meetingTitleNow();
      setMeetingTitle(title);
      recordingState.setStatus(RecordingStatus.STARTING, 'Initializing recording...');
      nativeStartInvoked = true;
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
        recordingSeconds: null,
        recoveryMeetingId: sessionStorage.getItem('indexeddb_current_meeting_id'),
      };
      latestSessionIdRef.current = result.session_id;
      sessionsRef.current.set(result.session_id, session);
      persistSessions(sessionsRef.current);
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
      await rejectRequest(text);
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

  const runLifecycle = useCallback((
    kind: Exclude<RecordingCommand, null>,
    sessionId: string | null,
    operation: () => Promise<void>,
  ): Promise<void> => {
    const active = lifecycleOperationRef.current;
    if (active) {
      if (active.kind === kind && active.sessionId === sessionId) return active.promise;
      return Promise.reject(new LifecycleBusyError(`Cannot ${kind} while ${active.kind} is in progress.`));
    }
    const promise = operation().finally(() => {
      if (lifecycleOperationRef.current?.promise === promise) lifecycleOperationRef.current = null;
    });
    lifecycleOperationRef.current = { kind, sessionId, promise };
    return promise;
  }, []);

  const startRecording = useCallback((options: StartRecordingOptions = {}): Promise<void> => {
    if ([...finalizationsRef.current.values()].some((state) => !state.completed)) {
      return Promise.reject(new LifecycleBusyError('Finish saving the previous meeting before starting another.'));
    }
    return runLifecycle('start', null, () => performStart(options));
  }, [performStart, runLifecycle]);
  startHandlerRef.current = startRecording;

  const finalizeRecording = useCallback((sessionId: string, saveMeeting = true): Promise<void> => {
    const session = sessionsRef.current.get(sessionId);
    if (!session) {
      return Promise.reject(new Error('The stopped recording could not be matched to its saved session.'));
    }
    let state = finalizationsRef.current.get(sessionId);
    if (state?.completed) return Promise.resolve();
    if (state?.promise) return state.promise;
    if (!state) {
      state = {
        ...session.finalization,
        promise: null,
        completed: session.finalization?.completed ?? false,
        deferred: session.finalization?.deferred ?? null,
        transcripts: null,
        savedMeetingId: session.finalization?.savedMeetingId ?? null,
        attachedNotesRead: false,
        attachedNotes: null,
        notesAttached: session.finalization?.notesAttached ?? false,
        postProcessingStarted: session.finalization?.postProcessingStarted ?? false,
        markedSaved: session.finalization?.markedSaved ?? false,
        catalogRefreshed: session.finalization?.catalogRefreshed ?? false,
        discarded: session.finalization?.discarded ?? false,
      };
      finalizationsRef.current.set(sessionId, state);
    }
    const finalization = state;
    const promise = (async () => {
      setCommand('finalize');
      recordingState.setStatus(RecordingStatus.PROCESSING_TRANSCRIPTS, 'Finishing transcription...');

      if (!saveMeeting) {
        finalization.completed = true;
        sessionsRef.current.delete(sessionId);
        persistSessions(sessionsRef.current);
        recordingState.setStatus(RecordingStatus.IDLE);
        if (mountedRef.current) setCommand(null);
        return;
      }

      try {
        if (session.folderPath) await flushNotes({ sessionId: session.folderPath });
        if (session.meetingId) await flushNotes({ meetingId: session.meetingId });
        if (finalization.deferred === null) {
          finalization.deferred = shouldDeferTranscription(
            localStorage.getItem(LIVE_TRANSCRIPTION_STORAGE_KEY),
            betaFeatures.liveTranscription,
          );
          checkpointFinalization(session, finalization, sessionsRef.current);
        }
        const deferred = finalization.deferred;
        if (!finalization.transcripts) {
          if (!deferred && !session.folderPath) {
            throw new Error('The recording has no folder for its completed transcript file.');
          }
          finalization.transcripts = deferred ? [] : await readPersistedTranscripts(session.folderPath!);
        }
        recordingState.setStatus(RecordingStatus.SAVING, 'Saving meeting...');
        const freshTranscripts = finalization.transcripts;
        if (!finalization.savedMeetingId) {
          const saved = await storageService.saveMeeting(
            session.title,
            freshTranscripts,
            session.folderPath,
            !deferred,
            session.meetingId,
          );
          if (!saved.meeting_id) throw new Error('No meeting ID was returned while saving.');
          finalization.savedMeetingId = saved.meeting_id;
          session.meetingId = saved.meeting_id;
          checkpointFinalization(session, finalization, sessionsRef.current);
        }
        const savedMeetingId = finalization.savedMeetingId;

        const notesTarget = meetingNotesTarget(savedMeetingId);
        if (!finalization.notesAttached && session.folderPath) {
          await flushNotes({ sessionId: session.folderPath });
          await flushNotes({ meetingId: savedMeetingId });
          const existingNotes = await notePersistenceService.loadNotes(notesTarget);
          if (existingNotes.loadState === 'error') throw existingNotes.loadError;
          if (!finalization.attachedNotesRead) {
            finalization.attachedNotes = await invoke<LiveNotesDocument | null>('attach_live_notes', {
              meetingId: savedMeetingId,
              folderPath: session.folderPath,
            });
            finalization.attachedNotesRead = true;
          }
          if (
            existingNotes.document
            && finalization.attachedNotes
            && (
              !Number.isFinite(Date.parse(existingNotes.document.updatedAt))
              || !Number.isFinite(Date.parse(finalization.attachedNotes.updatedAt))
              || Date.parse(existingNotes.document.updatedAt) > Date.parse(finalization.attachedNotes.updatedAt)
            )
          ) {
            notePersistenceService.saveNotes(notesTarget, existingNotes.document);
            await flushNotes({ meetingId: savedMeetingId });
          }
          finalization.notesAttached = true;
          checkpointFinalization(session, finalization, sessionsRef.current);
        }

        const notes = await notePersistenceService.loadNotes(notesTarget);
        if (notes.loadState === 'error') throw notes.loadError;

        const preferences = await invoke<{ min_meeting_duration_seconds?: number }>(
          'get_recording_preferences',
        ).catch(() => null);
        const minimumSeconds = preferences?.min_meeting_duration_seconds ?? 10;
        const transcriptSeconds = freshTranscripts.reduce(
          (maximum, transcript) => Math.max(maximum, transcript.audio_end_time ?? 0),
          0,
        );
        const hasNotes = Boolean(originalNotesMarkdown(notes.document).trim());
        const hasTranscript = freshTranscripts.some((transcript) => transcript.text.trim());
        if (
          minimumSeconds > 0
          && session.recordingSeconds !== null
          && Math.max(session.recordingSeconds, transcriptSeconds) < minimumSeconds
          && !hasNotes
          && !hasTranscript
        ) {
          if (!finalization.discarded) {
            await invoke('api_discard_meeting', { meetingId: savedMeetingId });
            finalization.discarded = true;
            checkpointFinalization(session, finalization, sessionsRef.current);
          }
          const ownsCurrentSession = latestSessionIdRef.current === sessionId;
          if (ownsCurrentSession && !finalization.markedSaved) {
            const recoveryMeetingId = session.recoveryMeetingId
              ?? sessionStorage.getItem('indexeddb_current_meeting_id');
            if (!recoveryMeetingId) throw new Error('The recovery draft identity is unavailable.');
            session.recoveryMeetingId = recoveryMeetingId;
            checkpointFinalization(session, finalization, sessionsRef.current);
            await indexedDBService.markMeetingSaved(recoveryMeetingId);
            finalization.markedSaved = true;
            checkpointFinalization(session, finalization, sessionsRef.current);
          }
          if (ownsCurrentSession) {
            localStorage.removeItem(LIVE_NOTES_FALLBACK_KEY);
            localStorage.removeItem(LIVE_NOTES_FALLBACK_FOLDER_KEY);
            sessionStorage.removeItem(ACTIVE_MEETING_KEY);
            sessionStorage.removeItem('indexeddb_current_meeting_id');
          }
          if (!finalization.catalogRefreshed) {
            await refetchMeetings();
            finalization.catalogRefreshed = true;
            checkpointFinalization(session, finalization, sessionsRef.current);
          }
          if (ownsCurrentSession) {
            setIsMeetingActive(false);
            clearTranscripts();
            recordingState.setStatus(RecordingStatus.IDLE);
          }
          finalization.completed = true;
          sessionsRef.current.delete(sessionId);
          persistSessions(sessionsRef.current);
          return;
        }

        if (!finalization.postProcessingStarted && deferred) {
          if (!session.folderPath) throw new Error('The recording has no audio folder for deferred transcription.');
          await invoke('start_retranscription_command', {
            meetingId: savedMeetingId,
            meetingFolderPath: session.folderPath,
            language: selectedLanguage === 'auto' || selectedLanguage === 'auto-translate' ? null : selectedLanguage,
            model: transcriptModelConfig.model || null,
            provider: transcriptModelConfig.provider || null,
          });
          markDeferredMeetingForAutoSummary(savedMeetingId);
          finalization.postProcessingStarted = true;
          checkpointFinalization(session, finalization, sessionsRef.current);
        } else if (!finalization.postProcessingStarted) {
          const pinned = await applyPinnedSummaryLanguageToMeeting(savedMeetingId).catch(() => false);
          if (!pinned) {
            await detectAndCacheSummaryLanguage(
              savedMeetingId,
              freshTranscripts.map((transcript) => transcript.text),
            ).catch(() => {});
          }
          window.dispatchEvent(new CustomEvent('meetily:meeting-ready-for-summary', {
            detail: { meetingId: savedMeetingId },
          }));
          void invoke<{ speaker_count: number }>('run_speaker_diarization', {
            meetingId: savedMeetingId,
            numSpeakers: null,
          }).catch((error) => console.warn('Automatic speaker identification skipped:', error));
          finalization.postProcessingStarted = true;
          checkpointFinalization(session, finalization, sessionsRef.current);
        }

        const ownsCurrentSession = latestSessionIdRef.current === sessionId;
        if (ownsCurrentSession && !finalization.markedSaved) {
          const recoveryMeetingId = session.recoveryMeetingId
            ?? sessionStorage.getItem('indexeddb_current_meeting_id');
          if (!recoveryMeetingId) throw new Error('The recovery draft identity is unavailable.');
          session.recoveryMeetingId = recoveryMeetingId;
          checkpointFinalization(session, finalization, sessionsRef.current);
          await indexedDBService.markMeetingSaved(recoveryMeetingId);
          finalization.markedSaved = true;
          checkpointFinalization(session, finalization, sessionsRef.current);
        }
        if (ownsCurrentSession) {
          localStorage.removeItem(LIVE_NOTES_FALLBACK_KEY);
          localStorage.removeItem(LIVE_NOTES_FALLBACK_FOLDER_KEY);
          sessionStorage.removeItem(ACTIVE_MEETING_KEY);
          sessionStorage.removeItem('indexeddb_current_meeting_id');
          setCurrentMeeting({ id: savedMeetingId, title: session.title });
        }
        if (!finalization.catalogRefreshed) {
          await refetchMeetings();
          finalization.catalogRefreshed = true;
          checkpointFinalization(session, finalization, sessionsRef.current);
        }
        if (ownsCurrentSession) {
          setIsMeetingActive(false);
          clearTranscripts();
          recordingState.setStatus(RecordingStatus.IDLE);
        }
        window.dispatchEvent(new CustomEvent('meetily:recording-finalized', {
          detail: { meetingId: savedMeetingId, transcribing: deferred },
        }));
        finalization.completed = true;
        sessionsRef.current.delete(sessionId);
        persistSessions(sessionsRef.current);
        const durationSeconds = Math.max(session.recordingSeconds ?? transcriptSeconds, transcriptSeconds);
        const wordCount = freshTranscripts.reduce(
          (count, transcript) => count + transcript.text.split(/\s+/).filter(Boolean).length,
          0,
        );
        void (async () => {
          const meetingsToday = await Analytics.getMeetingsCountToday();
          await Analytics.trackMeetingCompleted(savedMeetingId, {
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
          () => runLifecycle('finalize', sessionId, () => finalizeRecording(sessionId, saveMeeting)),
        );
        throw error;
      } finally {
        if (mountedRef.current) setCommand(null);
      }
    })().finally(() => {
      if (finalization.promise === promise) finalization.promise = null;
    });
    finalization.promise = promise;
    return promise;
  }, [
    betaFeatures.liveTranscription,
    clearTranscripts,
    recordingState,
    refetchMeetings,
    reportError,
    selectedLanguage,
    setCurrentMeeting,
    setIsMeetingActive,
    transcriptModelConfig.model,
    transcriptModelConfig.provider,
    runLifecycle,
  ]);
  finalizeHandlerRef.current = finalizeRecording;

  const performStop = useCallback(async (options: StopRecordingOptions = {}) => {
    const sessionId = recording?.session_id ?? latestSessionIdRef.current;
    if (!sessionId) return;
    const session = sessionsRef.current.get(sessionId);
    setCommand('stop');
    setFeedback(null);
    let nativeStopCompleted = options.nativeAlreadyStopped ?? false;
    try {
      if (!options.nativeAlreadyStopped) {
        const folderPath = session?.folderPath ?? await recordingFolderPath();
        if (session) {
          session.folderPath = folderPath;
          const nativeState = await recordingService.getRecordingState();
          if (nativeState.session_id === sessionId && nativeState.recording_duration != null) {
            session.recordingSeconds = nativeState.recording_duration;
          }
          persistSessions(sessionsRef.current);
        }
        if (folderPath) await flushNotes({ sessionId: folderPath });
        if (session?.meetingId) await flushNotes({ meetingId: session.meetingId });
        const dataDir = await appDataDir();
        const savePath = `${dataDir}/recording-${new Date().toISOString().replace(/[:.]/g, '-')}.wav`;
        recordingState.setStatus(RecordingStatus.STOPPING, 'Stopping recording...');
        await recordingService.stopRecording(savePath);
        nativeStopCompleted = true;
      }
      await finalizeRecording(sessionId, options.saveMeeting ?? true);
      await rehydrate().catch(() => {});
    } catch (error) {
      if (!nativeStopCompleted) {
        const nativeState = await recordingService.getRecordingState().catch(() => null);
        await rehydrate().catch(() => {});
        if (!nativeState) {
          reportError(
            'capture',
            'Recording state could not be confirmed',
            error,
            'recording',
            () => stopHandlerRef.current(options),
          );
        } else if (nativeState.is_recording && nativeState.session_id === sessionId) {
          reportError('capture', 'Recording is still active', error, 'recording', () => stopHandlerRef.current(options));
        } else {
          try {
            await finalizeRecording(sessionId, options.saveMeeting ?? true);
            reportError('capture', 'Recording stopped with an error', error, 'recording');
          } catch {
            // Finalization installs its own persistence-only retry.
          }
        }
      } else if (!finalizationsRef.current.get(sessionId)?.completed) {
        // Finalization installs its own persistence-only retry.
      }
      throw error;
    } finally {
      if (mountedRef.current) setCommand(null);
    }
  }, [finalizeRecording, recording?.session_id, recordingState, rehydrate, reportError]);

  const stopRecording = useCallback((options: StopRecordingOptions = {}): Promise<void> => {
    const sessionId = recording?.session_id ?? latestSessionIdRef.current;
    return runLifecycle('stop', sessionId, () => performStop(options));
  }, [performStop, recording?.session_id, runLifecycle]);
  stopHandlerRef.current = stopRecording;

  useEffect(() => {
    for (const activity of snapshot.activities) {
      if (
        activity.kind !== 'recording'
        || (activity.status !== 'ready' && activity.status !== 'failed')
        || !sessionsRef.current.has(activity.task_id)
        || (recording !== null && recording.session_id !== activity.task_id)
      ) continue;
      const finalization = finalizationsRef.current.get(activity.task_id);
      if (finalization?.completed || finalization?.promise) continue;
      void runLifecycle(
        'finalize',
        activity.task_id,
        () => finalizeRecording(activity.task_id, true),
      ).catch(() => {});
    }
  }, [finalizeRecording, recording, runLifecycle, snapshot.activities]);

  const performPause = useCallback(async () => {
    if (!recording || recording.status !== 'recording') return;
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
        () => runLifecycle('pause', recording.session_id, performPause),
      );
      await rehydrate().catch(() => {});
      throw error;
    } finally {
      if (mountedRef.current) setCommand(null);
    }
  }, [recording, rehydrate, reportError, runLifecycle]);

  const pauseRecording = useCallback((): Promise<void> => (
    runLifecycle('pause', recording?.session_id ?? null, performPause)
  ), [performPause, recording?.session_id, runLifecycle]);

  const performResume = useCallback(async () => {
    if (!recording || recording.status !== 'paused') return;
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
        () => runLifecycle('resume', recording.session_id, performResume),
      );
      await rehydrate().catch(() => {});
      throw error;
    } finally {
      if (mountedRef.current) setCommand(null);
    }
  }, [recording, rehydrate, reportError, runLifecycle]);

  const resumeRecording = useCallback((): Promise<void> => (
    runLifecycle('resume', recording?.session_id ?? null, performResume)
  ), [performResume, recording?.session_id, runLifecycle]);

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
        if (error instanceof LifecycleBusyError) {
          await meetingActivityService.acknowledgeRecordingRequest(
            request.request_id,
            false,
            error.message,
          ).catch(() => {});
        }
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

    const retain = async (registration: Promise<UnlistenFn>, assign: (unlisten: UnlistenFn) => void) => {
      try {
        const unlisten = await registration;
        if (disposed) unlisten();
        else assign(unlisten);
      } catch (error) {
        if (!disposed) reportError('capture', 'Recording controls could not initialize', error);
      }
    };

    void (async () => {
      try {
        const unlisten = await meetingActivityService.onRecordingStartRequested(
          (request) => requestHandlerRef.current(request),
        );
        if (disposed) {
          unlisten();
          return;
        }
        unlistenRequest = unlisten;
        const pending = await meetingActivityService.getPendingRecordingRequest();
        if (pending && !disposed) requestHandlerRef.current(pending);
      } catch (error) {
        if (!disposed) reportError('capture', 'Recording controls could not initialize', error);
      }
    })();
    void retain(
      recordingService.onRecordingStopped(() => {
        // Legacy metadata is unscoped. Session identity is reconciled from activity state.
        void rehydrate().catch(() => {});
      }),
      (unlisten) => { unlistenStopped = unlisten; },
    );
    void retain(
      listen<boolean>('recording-stop-complete', () => {
        void (async () => {
          const latestSessionId = latestSessionIdRef.current;
          const latest = await meetingActivityService.getSnapshot().catch(() => null);
          const terminal = latest?.activities.find((activity) => (
            activity.kind === 'recording'
            && activity.task_id === latestSessionId
            && (activity.status === 'ready' || activity.status === 'failed')
          ));
          if (terminal) {
            await runLifecycle(
              'finalize',
              terminal.task_id,
              () => finalizeHandlerRef.current(terminal.task_id, true),
            ).catch(() => {});
          }
          await rehydrate().catch(() => {});
        })();
      }),
      (unlisten) => { unlistenTrayStop = unlisten; },
    );
    void retain(
      transcriptService.onTranscriptionError((error) => {
        if (error.phase === 'startup' || error.actionable) {
          reportError('setup', 'Transcription needs attention', error.userMessage || error.error, 'transcription');
        } else {
          setFeedback({
            kind: 'warning',
            title: 'Live transcription issue',
            message: error.userMessage || error.error,
          });
        }
      }),
      (unlisten) => { unlistenTranscriptionError = unlisten; },
    );
    void retain(
      recordingService.onChunkDropWarning((warning) => {
        setFeedback({ kind: 'warning', title: 'Some audio could not be processed', message: warning });
      }),
      (unlisten) => { unlistenChunkWarning = unlisten; },
    );

    return () => {
      disposed = true;
      unlistenRequest?.();
      unlistenStopped?.();
      unlistenTrayStop?.();
      unlistenTranscriptionError?.();
      unlistenChunkWarning?.();
    };
  }, [rehydrate, reportError, runLifecycle]);

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
