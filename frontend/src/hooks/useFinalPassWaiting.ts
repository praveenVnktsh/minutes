'use client';

import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  FINAL_PASS_COMPLETE_EVENT,
  FINAL_PASS_WAITING_EVENT,
  finalPassApi,
  type FinalPassWaitingEvent,
} from '@/lib/liveTranscriptionModels';

/**
 * Whether this meeting's transcript is waiting for the Mac to be plugged in
 * before it is improved, and a way to improve it now.
 */
export function useFinalPassWaiting(meetingId: string) {
  const [waiting, setWaiting] = useState(false);
  const [starting, setStarting] = useState(false);

  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const refresh = () => {
      finalPassApi
        .isWaiting(meetingId)
        .then((value) => {
          if (!disposed) setWaiting(value);
        })
        .catch(() => {
          if (!disposed) setWaiting(false);
        });
    };
    refresh();
    void listen<FinalPassWaitingEvent>(FINAL_PASS_WAITING_EVENT, ({ payload }) => {
      if (payload.meeting_id === meetingId) setWaiting(payload.waiting);
    })
      .then((fn) => (disposed ? fn() : unlisteners.push(fn)))
      .catch(() => {});
    void listen<{ meeting_id: string }>(FINAL_PASS_COMPLETE_EVENT, ({ payload }) => {
      if (payload.meeting_id === meetingId) refresh();
    })
      .then((fn) => (disposed ? fn() : unlisteners.push(fn)))
      .catch(() => {});
    return () => {
      disposed = true;
      unlisteners.forEach((fn) => fn());
    };
  }, [meetingId]);

  const improveNow = useCallback(async () => {
    setStarting(true);
    try {
      await finalPassApi.runNow(meetingId);
      setWaiting(false);
    } catch (error) {
      console.warn('Could not improve the transcript now:', error);
    } finally {
      setStarting(false);
    }
  }, [meetingId]);

  return { waiting, starting, improveNow };
}
