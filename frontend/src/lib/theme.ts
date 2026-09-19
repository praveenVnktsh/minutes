import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getAllWindows, type Theme as NativeTheme } from '@tauri-apps/api/window';

export type AppTheme = 'light' | 'dark';

export const THEME_STORAGE_KEY = 'meetily:theme';
export const THEME_CHANGED_EVENT = 'meetily-theme-changed';

export function readTheme(): AppTheme {
  if (typeof window === 'undefined') return 'dark';
  try {
    return localStorage.getItem(THEME_STORAGE_KEY) === 'light' ? 'light' : 'dark';
  } catch {
    return 'dark';
  }
}

export function applyTheme(theme: AppTheme): void {
  document.documentElement.classList.toggle('dark', theme === 'dark');
  document.documentElement.style.colorScheme = theme;
}

export async function persistAndBroadcastTheme(theme: AppTheme): Promise<void> {
  try {
    localStorage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // Theme still applies for this process when storage is unavailable.
  }
  applyTheme(theme);

  const nativeTheme: NativeTheme = theme;
  await Promise.allSettled([
    emit(THEME_CHANGED_EVENT, theme),
    getAllWindows().then((windows) => Promise.all(windows.map((window) => window.setTheme(nativeTheme)))),
  ]);
}

export async function listenForThemeChanges(callback: (theme: AppTheme) => void): Promise<UnlistenFn> {
  return listen<AppTheme>(THEME_CHANGED_EVENT, ({ payload }) => {
    if (payload === 'light' || payload === 'dark') callback(payload);
  });
}
