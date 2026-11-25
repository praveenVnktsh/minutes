import { useDebugModeFlag } from '@/lib/debugMode';

/** React binding for the debug-mode preference (backend is the source of truth). */
export function useDebugMode(): boolean {
  return useDebugModeFlag();
}
