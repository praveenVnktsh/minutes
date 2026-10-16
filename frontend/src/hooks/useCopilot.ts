'use client';

import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  COPILOT_DELTA_EVENT,
  COPILOT_STATE_EVENT,
  acceptCopilotSnapshot,
  applyDelta,
  copilotApi,
  type CopilotDelta,
  type CopilotSnapshot,
} from '@/lib/copilot';

/**
 * The recording's Copilot: hydrated from the Rust session (which outlives
 * navigation) and kept current by its state and token events.
 */
export function useCopilot(): CopilotSnapshot | null {
  const [snapshot, setSnapshot] = useState<CopilotSnapshot | null>(null);
  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    void copilotApi.state()
      .then((current) => {
        if (!disposed && current) setSnapshot((previous) => acceptCopilotSnapshot(previous, current));
      })
      .catch((error) => console.warn('Could not load Copilot:', error));
    const subscribe = <T,>(event: string, handler: (payload: T) => void) => {
      void listen<T>(event, ({ payload }) => handler(payload))
        .then((fn) => {
          if (disposed) fn();
          else unlisteners.push(fn);
        })
        .catch((error) => console.warn(`Could not listen for ${event}:`, error));
    };
    subscribe<CopilotSnapshot>(COPILOT_STATE_EVENT, (payload) => {
      setSnapshot((previous) => acceptCopilotSnapshot(previous, payload));
    });
    subscribe<CopilotDelta>(COPILOT_DELTA_EVENT, (payload) => {
      setSnapshot((previous) => applyDelta(previous, payload));
    });
    return () => {
      disposed = true;
      unlisteners.forEach((fn) => fn());
    };
  }, []);
  return snapshot;
}
