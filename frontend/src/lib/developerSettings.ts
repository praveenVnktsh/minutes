'use client';

import { useEffect, useState } from 'react';

/** Settings → Developer → Replays: shows the Replays page in the sidebar. A
 * per-device preference, off by default. */
export const REPLAYS_ENABLED_KEY = 'developer.replays';
const CHANGED_EVENT = 'meetily:developer-settings-changed';

export function readReplaysEnabled(): boolean {
  if (typeof window === 'undefined') return false;
  try {
    return localStorage.getItem(REPLAYS_ENABLED_KEY) === 'true';
  } catch {
    return false;
  }
}

export function setReplaysEnabled(enabled: boolean): void {
  try {
    localStorage.setItem(REPLAYS_ENABLED_KEY, String(enabled));
  } catch {
    // Storage unavailable: the change lasts until the window reloads.
  }
  window.dispatchEvent(new CustomEvent(CHANGED_EVENT, { detail: { replays: enabled } }));
}

export function useReplaysEnabled(): boolean {
  const [enabled, setEnabled] = useState(false);
  useEffect(() => {
    setEnabled(readReplaysEnabled());
    const update = (event: Event) => {
      const detail = (event as CustomEvent<{ replays?: boolean }>).detail;
      setEnabled(typeof detail?.replays === 'boolean' ? detail.replays : readReplaysEnabled());
    };
    const storage = (event: StorageEvent) => {
      if (event.key === REPLAYS_ENABLED_KEY) setEnabled(event.newValue === 'true');
    };
    window.addEventListener(CHANGED_EVENT, update);
    window.addEventListener('storage', storage);
    return () => {
      window.removeEventListener(CHANGED_EVENT, update);
      window.removeEventListener('storage', storage);
    };
  }, []);
  return enabled;
}
