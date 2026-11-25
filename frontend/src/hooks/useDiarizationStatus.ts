'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

/**
 * Whether a meeting's current transcript has speaker labels, as reported by
 * `get_diarization_status`. Mirrors the Rust `DiarizationStatus` enum.
 */
export type DiarizationStatus =
  | { state: 'done' }
  | { state: 'running' }
  /** Owed but not running: interrupted by a quit, or not started yet. */
  | { state: 'pending'; attempts: number }
  | { state: 'failed'; error: string }
  /** No run was ever recorded, e.g. an imported meeting. */
  | { state: 'missing' };

/**
 * Tracks a meeting's diarization status and refreshes it whenever the Rust
 * core starts or finishes a run for that meeting.
 */
export function useDiarizationStatus(meetingId?: string | null) {
  const [status, setStatus] = useState<DiarizationStatus | null>(null);

  const refresh = useCallback(async () => {
    if (!meetingId) {
      setStatus(null);
      return;
    }
    try {
      setStatus(await invoke<DiarizationStatus>('get_diarization_status', { meetingId }));
    } catch (error) {
      console.warn('Could not load speaker identification status:', error);
      setStatus(null);
    }
  }, [meetingId]);

  useEffect(() => {
    setStatus(null);
    void refresh();
    if (!meetingId) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<{ meeting_id: string }>('diarization-status-changed', ({ payload }) => {
      if (payload.meeting_id === meetingId) void refresh();
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    }).catch((error) => console.warn('Could not listen for speaker identification status:', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [meetingId, refresh]);

  return { status, refresh };
}
