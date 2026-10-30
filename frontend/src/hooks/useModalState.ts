import { useState, useCallback } from 'react';

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
 * Custom hook for managing all modal state: which modals are open and the
 * message each one shows.
 */
export function useModalState(): UseModalStateReturn {
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

  return {
    modals,
    messages,
    showModal,
    hideModal,
    hideAllModals,
  };
}
