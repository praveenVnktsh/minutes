'use client';

import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';

/** Settings → Developer → Replays: shows the Replays page in the sidebar and
 * lets the replay commands run. Kept by the app (store.json), which enforces
 * it: the replay commands refuse while it is off. Off by default. */
const CHANGED_EVENT = 'meetily:developer-settings-changed';

export async function readReplaysEnabled(): Promise<boolean> {
  try {
    return await invoke<boolean>('get_replays_enabled');
  } catch {
    return false;
  }
}

export async function setReplaysEnabled(enabled: boolean): Promise<void> {
  await invoke('set_replays_enabled', { enabled });
  window.dispatchEvent(new CustomEvent(CHANGED_EVENT, { detail: { replays: enabled } }));
}

export function useReplaysEnabled(): boolean {
  const [enabled, setEnabled] = useState(false);
  useEffect(() => {
    let live = true;
    void readReplaysEnabled().then((value) => {
      if (live) setEnabled(value);
    });
    const update = (event: Event) => {
      const detail = (event as CustomEvent<{ replays?: boolean }>).detail;
      if (typeof detail?.replays === 'boolean') setEnabled(detail.replays);
    };
    window.addEventListener(CHANGED_EVENT, update);
    return () => {
      live = false;
      window.removeEventListener(CHANGED_EVENT, update);
    };
  }, []);
  return enabled;
}
