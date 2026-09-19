'use client';

import React, { createContext, useContext, useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { recordingService } from '@/services/recordingService';
import { toast } from 'sonner';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import type { MeetingActivityStatus } from '@/types/meetingActivity';

/**
 * Recording state synchronized with backend
 * This context provides a single source of truth for recording state
 * that automatically syncs with the Rust backend, solving:
 * 1. Page refresh desync (backend recording but UI shows stopped)
 * 2. Pause state visibility across components
 * 3. Comprehensive state for future features (reconnection, etc.)
 */

// Recording lifecycle status enum
export enum RecordingStatus {
  IDLE = 'idle',                          // Not recording
  STARTING = 'starting',                  // Initiating recording
  RECORDING = 'recording',                // Active recording
  STOPPING = 'stopping',                  // Stop initiated, waiting for backend
  PROCESSING_TRANSCRIPTS = 'processing',  // Transcription completion wait
  SAVING = 'saving',                      // Saving to database
  COMPLETED = 'completed',                // Successfully saved
  ERROR = 'error'                         // Error occurred
}

interface RecordingState {
  isRecording: boolean;           // Is a recording session active
  isPaused: boolean;              // Is the recording paused
  isActive: boolean;              // Is actively recording (recording && !paused)
  recordingDuration: number | null;  // Total duration including pauses
  activeDuration: number | null;     // Active recording time (excluding pauses)

  // NEW: Lifecycle status
  status: RecordingStatus;
  statusMessage?: string;  // Optional message for current status
}

interface RecordingStateContextType extends RecordingState {
  sessionId: string | null;
  activeMeetingId: string | null;
  activityStatus: MeetingActivityStatus | null;
  activityError: string | null;
  // NEW: Setters for status management
  setStatus: (status: RecordingStatus, message?: string) => void;

  // Computed helpers (derived from status)
  isStopping: boolean;
  isProcessing: boolean;
  isSaving: boolean;
  isStartingRecording: boolean;
}

const RecordingStateContext = createContext<RecordingStateContextType | null>(null);

export const useRecordingState = () => {
  const context = useContext(RecordingStateContext);
  if (!context) {
    throw new Error('useRecordingState must be used within a RecordingStateProvider');
  }
  return context;
};

export function RecordingStateProvider({ children }: { children: React.ReactNode }) {
  const { recording, hydrationStatus, snapshot } = useMeetingActivity();
  const [state, setState] = useState<RecordingState>({
    isRecording: false,
    isPaused: false,
    isActive: false,
    recordingDuration: null,
    activeDuration: null,
    status: RecordingStatus.IDLE,  // NEW: Initialize with IDLE status
    statusMessage: undefined,       // NEW: No message initially
  });

  const previousSessionRef = useRef<string | null>(null);

  useEffect(() => {
    if (!recording) {
      if (hydrationStatus === 'ready') {
        const previousSession = previousSessionRef.current;
        const terminal = previousSession
          ? snapshot.activities.find((activity) => activity.task_id === previousSession)
          : null;
        setState((previous) => {
          const finalizing = [
            RecordingStatus.STOPPING,
            RecordingStatus.PROCESSING_TRANSCRIPTS,
            RecordingStatus.SAVING,
          ].includes(previous.status);
          const status = terminal?.status === 'failed'
            ? RecordingStatus.ERROR
            : finalizing
              ? previous.status
              : terminal?.status === 'ready'
                ? RecordingStatus.COMPLETED
                : RecordingStatus.IDLE;
          return {
            ...previous,
            isRecording: false,
            isPaused: false,
            isActive: false,
            recordingDuration: null,
            activeDuration: null,
            status,
            statusMessage: terminal?.error ?? (finalizing ? previous.statusMessage : undefined),
          };
        });
        previousSessionRef.current = null;
      }
      return;
    }
    previousSessionRef.current = recording.session_id;
    const isRecording = recording.status === 'starting'
      || recording.status === 'recording'
      || recording.status === 'paused';
    setState((previous) => {
      const status = recording.status === 'starting'
        ? RecordingStatus.STARTING
        : recording.status === 'recording' || recording.status === 'paused'
          ? RecordingStatus.RECORDING
          : recording.status === 'saving'
            ? RecordingStatus.SAVING
            : recording.status === 'failed'
              ? RecordingStatus.ERROR
              : previous.status;
      return {
        ...previous,
        isRecording,
        isPaused: recording.status === 'paused',
        isActive: recording.status === 'recording',
        status,
        statusMessage: recording.error
          ?? (recording.status === 'starting' ? 'Starting recording...' : undefined),
      };
    });
  }, [hydrationStatus, recording, snapshot.activities]);

  // NEW: Status setter with logging
  const setStatus = useCallback((status: RecordingStatus, message?: string) => {
    setState(prev => {
      console.log(`[RecordingState] Status: ${prev.status} → ${status}`, message || '');
      return { ...prev, status, statusMessage: message };
    });
  }, []);

  // Duration is not part of the activity snapshot. Poll only those counters and
  // discard delayed reads unless they still belong to the authoritative session.
  useEffect(() => {
    const sessionId = recording?.session_id;
    if (!sessionId || (recording.status !== 'recording' && recording.status !== 'paused')) return;
    let disposed = false;
    const readDurations = async () => {
      try {
        const backend = await recordingService.getRecordingState();
        if (disposed || backend.session_id !== sessionId || previousSessionRef.current !== sessionId) return;
        setState((previous) => ({
          ...previous,
          recordingDuration: backend.recording_duration,
          activeDuration: backend.active_duration,
        }));
      } catch (error) {
        if (!disposed) console.error('[RecordingStateContext] Failed to read recording duration:', error);
      }
    };
    void readDurations();
    const timer = setInterval(() => void readDurations(), 500);
    return () => {
      disposed = true;
      clearInterval(timer);
    };
  }, [recording?.session_id, recording?.status]);

  /**
   * Global mic hot-swap UI feedback.
   *
   * The Rust backend emits `mic-device-switched` whenever the recording mic
   * changes without the user picking it — on a successful mid-recording
   * disconnect fallback, and when a selected mic isn't available at start and
   * the backend falls back to the default — and `mic-swap-failed` when
   * mid-recording recovery fails. Nothing else in the app listens for these,
   * so without this effect the switch is silent (or a dead-mic recording).
   * Mounting the listener here surfaces a toast regardless of which page the
   * user is on. Pure UI feedback — no state mutation, no persisted-preference
   * writes.
   */
  // Ref tracks latest isRecording for the mount-once listener below — the
  // effect has [] deps so it can't read state directly (it would capture the
  // initial value). Kept current by this small sync effect.
  const isRecordingRef = useRef(false);
  useEffect(() => {
    isRecordingRef.current = state.isRecording;
  }, [state.isRecording]);

  useEffect(() => {
    // `cancelled` guard prevents leaking a listener when StrictMode/HMR runs
    // cleanup before the async listen(...) registration resolves.
    let cancelled = false;
    let unlistenSwitched: (() => void) | undefined;
    let unlistenFailed: (() => void) | undefined;
    let unlistenMicUnavailable: (() => void) | undefined;
    let unlistenExhausted: (() => void) | undefined;

    const setup = async () => {
      try {
        const fnSwitched = await recordingService.onMicDeviceSwitched(({ device_name }) => {
          console.log('[RecordingStateContext] mic-device-switched →', device_name);
          // Fires for both cases: a mic that disconnects mid-recording, and a
          // selected mic that wasn't available at start (backend fell back to
          // the default). Copy is worded to be accurate for both.
          toast.info(
            `Microphone switched to ${device_name} for this meeting.`,
            { duration: 6000 }
          );
        });
        if (cancelled) { fnSwitched(); return; }
        unlistenSwitched = fnSwitched;

        const fnFailed = await recordingService.onMicSwapFailed(({ error, device_name }) => {
          console.error('[RecordingStateContext] mic-swap-failed →', device_name, error);
          // Only alarm the user if recording is still active. A Stop clicked
          // during a hot-swap race fails with "Recording manager not
          // available" — expected, not an error worth a toast.
          if (isRecordingRef.current) {
            toast.error(
              `Microphone fallback failed for ${device_name}: ${error}`,
              { duration: 8000 }
            );
          }
        });
        if (cancelled) { fnFailed(); return; }
        unlistenFailed = fnFailed;

        const fnUnavailable = await recordingService.onMicUnavailable(() => {
          console.log('[RecordingStateContext] mic-unavailable');
          // Fires at recording start, before isRecording flips true, so this
          // is intentionally NOT gated by isRecordingRef.
          toast.error(
            'No microphone available — recording system audio only.',
            { duration: 8000 }
          );
        });
        if (cancelled) { fnUnavailable(); return; }
        unlistenMicUnavailable = fnUnavailable;

        const fnExhausted = await recordingService.onMicRecoveryExhausted(({ device_name }) => {
          console.error('[RecordingStateContext] mic-recovery-exhausted →', device_name);
          // Fires mid-recording only — gate on the recording ref like
          // mic-swap-failed so a stale event after Stop doesn't alarm the user.
          if (isRecordingRef.current) {
            toast.error(
              `Microphone '${device_name}' could not be recovered — recording continues without a microphone. Stop and restart to fix.`,
              { duration: 10000 }
            );
          }
        });
        if (cancelled) { fnExhausted(); return; }
        unlistenExhausted = fnExhausted;
      } catch (e) {
        console.error('[RecordingStateContext] Failed to set up hot-swap listeners:', e);
      }
    };

    setup();

    return () => {
      cancelled = true;
      unlistenSwitched?.();
      unlistenFailed?.();
      unlistenMicUnavailable?.();
      unlistenExhausted?.();
    };
  }, []);

  // NEW: Computed helpers from status
  const contextValue = useMemo(() => ({
    ...state,
    sessionId: recording?.session_id ?? null,
    activeMeetingId: recording?.meeting_id ?? null,
    activityStatus: recording?.status ?? null,
    activityError: recording?.error ?? null,
    setStatus,
    isStopping: state.status === RecordingStatus.STOPPING,
    isProcessing: state.status === RecordingStatus.PROCESSING_TRANSCRIPTS,
    isSaving: state.status === RecordingStatus.SAVING,
    isStartingRecording: state.status === RecordingStatus.STARTING,
  }), [recording, state, setStatus]);

  return (
    <RecordingStateContext.Provider value={contextValue}>
      {children}
    </RecordingStateContext.Provider>
  );
}
