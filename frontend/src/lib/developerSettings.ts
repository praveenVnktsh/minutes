'use client';

import { readBackendFlag, useBackendFlag, writeBackendFlag } from '@/lib/backendFlag';

/** Settings → Developer → Replays: shows the Replays page in the sidebar and
 * lets the replay commands run. Kept by the app (store.json), which enforces
 * it: the replay commands refuse while it is off. Off by default. */
const GET = 'get_replays_enabled';
const SET = 'set_replays_enabled';
const CHANGED_EVENT = 'meetily:replays-enabled-changed';

export function readReplaysEnabled(): Promise<boolean> {
  return readBackendFlag(GET);
}

export function setReplaysEnabled(enabled: boolean): Promise<void> {
  return writeBackendFlag(SET, CHANGED_EVENT, enabled);
}

export function useReplaysEnabled(): boolean {
  return useBackendFlag(GET, SET, CHANGED_EVENT)[0];
}
