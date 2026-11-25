'use client';

import { useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { useRecordingState } from '@/contexts/RecordingStateContext';

/**
 * The floating "meeting detected" prompt is shown from Rust (so it works while
 * this window is in the background). This component only starts monitoring and
 * hides the prompt when it is no longer relevant.
 */
export function MeetingDetectedPrompt({ enabled = true }: { enabled?: boolean }) {
  const { isRecording } = useRecordingState();

  const hidePrompt = useCallback(async () => {
    const promptWindow = await WebviewWindow.getByLabel('meeting-prompt');
    await promptWindow?.hide();
  }, []);

  useEffect(() => {
    if (isRecording) void hidePrompt();
  }, [hidePrompt, isRecording]);

  useEffect(() => {
    const handlePreferenceChange = (event: Event) => {
      const preferenceEnabled = (event as CustomEvent<boolean>).detail;
      if (!preferenceEnabled) void hidePrompt();
    };

    window.addEventListener('automaticRecordPromptChanged', handlePreferenceChange);
    return () => window.removeEventListener('automaticRecordPromptChanged', handlePreferenceChange);
  }, [hidePrompt]);

  useEffect(() => {
    if (!enabled) return;

    let disposed = false;
    const unlisteners: UnlistenFn[] = [];

    const registerListener = async <T,>(event: string, handler: (event: { payload: T }) => void) => {
      const unlisten = await listen<T>(event, handler);
      if (disposed) {
        unlisten();
        return false;
      }
      unlisteners.push(unlisten);
      return true;
    };

    const setup = async () => {
      if (!await registerListener<null>('system-audio-stopped', () => {
        void hidePrompt();
      })) return;

      if (disposed) return;
      await invoke('start_system_audio_monitoring');
    };

    setup().catch((error) => {
      console.error('[MeetingDetection] Failed to start monitoring:', error);
    });

    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
      void hidePrompt();
    };
  }, [enabled, hidePrompt]);

  return null;
}
