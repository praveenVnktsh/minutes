import { useState, useEffect, useCallback, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { TranscriptModelProps } from '@/components/TranscriptSettings';

export type ModalType =
  | 'modelSettings'
  | 'deviceSettings'
  | 'languageSettings'
  | 'modelSelector'
  | 'errorAlert'
  | 'chunkDropWarning';

interface ModalState {
  modelSettings: boolean;
  deviceSettings: boolean;
  languageSettings: boolean;
  modelSelector: boolean;
  errorAlert: boolean;
  chunkDropWarning: boolean;
}

interface ModalMessages {
  errorAlert: string;
  chunkDropWarning: string;
  modelSelector: string;
}

interface UseModalStateReturn {
  modals: ModalState;
  messages: ModalMessages;
  showModal: (name: ModalType, message?: string) => void;
  hideModal: (name: ModalType) => void;
  hideAllModals: () => void;
}

/**
 * Custom hook for managing all modal state and event listeners.
 * Consolidates 9 useState calls and 3 event listeners from page.tsx.
 *
 * Features:
 * - Unified modal state management
 * - Event listeners for chunk drops, transcription errors, model downloads
 * - Auto-close on model download completion
 */
export function useModalState(transcriptModelConfig?: TranscriptModelProps): UseModalStateReturn {
  const closeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Modal visibility state
  const [modals, setModals] = useState<ModalState>({
    modelSettings: false,
    deviceSettings: false,
    languageSettings: false,
    modelSelector: false,
    errorAlert: false,
    chunkDropWarning: false,
  });

  // Modal messages
  const [messages, setMessages] = useState<ModalMessages>({
    errorAlert: '',
    chunkDropWarning: '',
    modelSelector: '',
  });

  // Show modal with optional message
  const showModal = useCallback((name: ModalType, message?: string) => {
    setModals(prev => ({ ...prev, [name]: true }));

    // Set message if provided
    if (message && (name === 'errorAlert' || name === 'chunkDropWarning' || name === 'modelSelector')) {
      setMessages(prev => ({ ...prev, [name]: message }));
    }
  }, []);

  // Hide modal and clear its message
  const hideModal = useCallback((name: ModalType) => {
    setModals(prev => ({ ...prev, [name]: false }));

    // Clear message when closing
    if (name === 'errorAlert' || name === 'chunkDropWarning' || name === 'modelSelector') {
      setMessages(prev => ({ ...prev, [name]: '' }));
    }
  }, []);

  // Hide all modals
  const hideAllModals = useCallback(() => {
    setModals({
      modelSettings: false,
      deviceSettings: false,
      languageSettings: false,
      modelSelector: false,
      errorAlert: false,
      chunkDropWarning: false,
    });
    setMessages({
      errorAlert: '',
      chunkDropWarning: '',
      modelSelector: '',
    });
  }, []);

  // Listen for model download completion to auto-close modal
  useEffect(() => {
    let disposed = false;
    let cleanup: (() => void) | undefined;
    const setupDownloadListeners = async () => {
      const unlisteners: (() => void)[] = [];

      try {
        // Listen for Whisper model download complete
        const unlistenWhisper = await listen<{ modelName: string }>('model-download-complete', (event) => {
          const { modelName } = event.payload;
          console.log('[useModalState] Whisper model download complete:', modelName);

          // Auto-close modal if the downloaded model matches the selected one
          if (transcriptModelConfig?.provider === 'localWhisper' && transcriptModelConfig?.model === modelName) {
            toast.success('Model ready! Closing window...', { duration: 1500 });
            if (closeTimerRef.current) clearTimeout(closeTimerRef.current);
            closeTimerRef.current = setTimeout(() => hideModal('modelSelector'), 1500);
          }
        });
        if (disposed) {
          unlistenWhisper();
          return;
        }
        unlisteners.push(unlistenWhisper);
        cleanup = () => {
          unlisteners.forEach(unsub => unsub());
        };
      } catch (error) {
        if (!disposed) console.error('Failed to listen for model download completion:', error);
      }
    };

    void setupDownloadListeners();
    return () => {
      disposed = true;
      cleanup?.();
      if (closeTimerRef.current) {
        clearTimeout(closeTimerRef.current);
        closeTimerRef.current = null;
      }
    };
  }, [transcriptModelConfig, hideModal]);

  return {
    modals,
    messages,
    showModal,
    hideModal,
    hideAllModals,
  };
}
