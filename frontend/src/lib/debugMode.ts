import { cachedBackendFlag, readBackendFlag, useBackendFlag, writeBackendFlag } from '@/lib/backendFlag';

/**
 * Debug mode records meetings into an isolated "debug" bucket (hidden from the
 * normal list, stored under a recordings/debug folder) and exposes diagnostics.
 * The backend is the source of truth; localStorage mirrors it for first paint.
 */
const GET = 'get_debug_mode';
const SET = 'set_debug_mode';
const CHANGED_EVENT = 'debug-mode-changed';
const OPTIONS = { cacheKey: 'debugMode' };

export function cachedDebugMode(): boolean {
  return cachedBackendFlag(OPTIONS);
}

/** Read the authoritative value from the backend and mirror it locally. */
export function loadDebugMode(): Promise<boolean> {
  return readBackendFlag(GET, OPTIONS);
}

export function setDebugMode(enabled: boolean): Promise<void> {
  return writeBackendFlag(SET, CHANGED_EVENT, enabled, OPTIONS);
}

/** React binding for the debug-mode preference. */
export function useDebugModeFlag(): boolean {
  return useBackendFlag(GET, SET, CHANGED_EVENT, OPTIONS)[0];
}
