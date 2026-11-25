'use client';

import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';

/**
 * A boolean the Rust core keeps (debug mode, Replays): a get and a set command,
 * and a window event (detail: the new value) so every view follows a change.
 */
export interface BackendFlagOptions {
  /** Mirror the value in localStorage under this key: it is the first value
   * shown before the backend answers, and the answer when it cannot. */
  cacheKey?: string;
}

function readCache(cacheKey: string | undefined): boolean {
  if (!cacheKey || typeof window === 'undefined') return false;
  try {
    return localStorage.getItem(cacheKey) === 'true';
  } catch {
    return false;
  }
}

function writeCache(cacheKey: string | undefined, enabled: boolean): void {
  if (!cacheKey) return;
  try {
    localStorage.setItem(cacheKey, String(enabled));
  } catch {
    // The backend still has the value.
  }
}

/** The cached value (false without a cache). */
export function cachedBackendFlag(options: BackendFlagOptions = {}): boolean {
  return readCache(options.cacheKey);
}

/** The backend's value, mirrored to the cache; the cache (or false) if it cannot be read. */
export async function readBackendFlag(getCmd: string, options: BackendFlagOptions = {}): Promise<boolean> {
  try {
    const enabled = await invoke<boolean>(getCmd);
    writeCache(options.cacheKey, enabled);
    return enabled;
  } catch {
    return readCache(options.cacheKey);
  }
}

/**
 * Sets the flag. With a cache the change is shown at once and then saved;
 * without one it is announced only once the backend has accepted it.
 */
export async function writeBackendFlag(
  setCmd: string,
  eventName: string,
  enabled: boolean,
  options: BackendFlagOptions = {},
): Promise<void> {
  const announce = () => window.dispatchEvent(new CustomEvent(eventName, { detail: enabled }));
  if (options.cacheKey) {
    writeCache(options.cacheKey, enabled);
    announce();
    await invoke(setCmd, { enabled });
    return;
  }
  await invoke(setCmd, { enabled });
  announce();
}

/** The flag's current value, and a setter, kept in step across views. */
export function useBackendFlag(
  getCmd: string,
  setCmd: string,
  eventName: string,
  options: BackendFlagOptions = {},
): [boolean, (enabled: boolean) => Promise<void>] {
  const { cacheKey } = options;
  const [enabled, setEnabled] = useState(() => readCache(cacheKey));

  useEffect(() => {
    let live = true;
    void readBackendFlag(getCmd, { cacheKey }).then((value) => {
      if (live) setEnabled(value);
    });
    const update = (event: Event) => {
      const detail = (event as CustomEvent<unknown>).detail;
      setEnabled(typeof detail === 'boolean' ? detail : readCache(cacheKey));
    };
    window.addEventListener(eventName, update);
    return () => {
      live = false;
      window.removeEventListener(eventName, update);
    };
  }, [cacheKey, eventName, getCmd]);

  return [enabled, (next: boolean) => writeBackendFlag(setCmd, eventName, next, { cacheKey })];
}
