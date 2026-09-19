import { useCallback } from 'react';
import { useRecordingController } from '@/contexts/RecordingControllerContext';

interface UseRecordingStartReturn {
  handleRecordingStart: () => Promise<void>;
  isAutoStarting: boolean;
}

/** Compatibility facade for route code while the global controller owns start. */
export function useRecordingStart(
  _isRecording: boolean,
  setIsRecording: (value: boolean) => void,
  _showModal?: (name: 'modelSelector', message?: string) => void,
): UseRecordingStartReturn {
  const controller = useRecordingController();
  const handleRecordingStart = useCallback(async () => {
    await controller.startRecording();
    if (controller.sessionId) setIsRecording(true);
  }, [controller, setIsRecording]);

  return {
    handleRecordingStart,
    isAutoStarting: controller.command === 'start',
  };
}
