'use client';

import { createContext, useContext, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

/** What a live listener receives: the event's payload, as Tauri delivers it. */
export interface LiveEvent<T> {
  payload: T;
}

/**
 * Where the recording screen's live components get their events and reads
 * from. In the app this is Tauri itself; the replay viewer provides one fed
 * from a recorded run, so the same components play the run back.
 */
export interface LiveSource {
  readonly kind: 'tauri' | 'replay';
  listen<T>(event: string, handler: (event: LiveEvent<T>) => void): Promise<() => void>;
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
}

/** The app's source: Tauri's event bus and commands, called exactly as the
 * components called them before the source existed. */
export const tauriLiveSource: LiveSource = {
  kind: 'tauri',
  listen: (event, handler) => listen(event, handler),
  invoke: (command, args) => (args === undefined ? invoke(command) : invoke(command, args)),
};

const LiveSourceContext = createContext<LiveSource>(tauriLiveSource);

export function LiveSourceProvider({ source, children }: { source: LiveSource; children: ReactNode }) {
  return <LiveSourceContext.Provider value={source}>{children}</LiveSourceContext.Provider>;
}

/** The live source for this subtree: Tauri unless a replay provides one. */
export function useLiveSource(): LiveSource {
  return useContext(LiveSourceContext);
}
