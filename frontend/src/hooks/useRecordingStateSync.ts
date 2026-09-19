import { useState, useEffect } from 'react';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';

interface UseRecordingStateSyncReturn {
  isBackendRecording: boolean;
  isRecordingDisabled: boolean;
  setIsRecordingDisabled: (value: boolean) => void;
}

/**
 * Custom hook for synchronizing frontend recording state with backend.
 * Compatibility hook backed by the native activity snapshot. No route-local
 * polling is needed, and the active meeting identity comes from native state.
 */
export function useRecordingStateSync(
  isRecording: boolean,
  setIsRecording: (value: boolean) => void,
  setIsMeetingActive: (value: boolean) => void
): UseRecordingStateSyncReturn {
  const [isRecordingDisabled, setIsRecordingDisabled] = useState(false);
  const { recording } = useMeetingActivity();
  const isBackendRecording = recording?.status === 'starting'
    || recording?.status === 'recording'
    || recording?.status === 'paused';

  useEffect(() => {
    if (isBackendRecording !== isRecording) setIsRecording(isBackendRecording);
    if (isBackendRecording) setIsMeetingActive(true);
  }, [isBackendRecording, isRecording, setIsMeetingActive, setIsRecording]);

  return {
    isBackendRecording,
    isRecordingDisabled,
    setIsRecordingDisabled,
  };
}
