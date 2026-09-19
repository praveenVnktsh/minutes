import { useCallback } from 'react';
import { RecordingStatus, useRecordingState } from '@/contexts/RecordingStateContext';
import { useRecordingController } from '@/contexts/RecordingControllerContext';

type SummaryStatus = 'idle' | 'processing' | 'summarizing' | 'regenerating' | 'completed' | 'error';

interface UseRecordingStopReturn {
  handleRecordingStop: (callApi: boolean) => Promise<void>;
  isStopping: boolean;
  isProcessingTranscript: boolean;
  isSavingTranscript: boolean;
  summaryStatus: SummaryStatus;
  setIsStopping: (value: boolean) => void;
}

/** Compatibility facade for existing Home callers. */
export function useRecordingStop(
  setIsRecording: (value: boolean) => void,
  setIsRecordingDisabled: (value: boolean) => void,
): UseRecordingStopReturn {
  const controller = useRecordingController();
  const state = useRecordingState();
  const handleRecordingStop = useCallback(async (callApi: boolean) => {
    setIsRecordingDisabled(true);
    try {
      await controller.stopRecording({ nativeAlreadyStopped: !callApi });
      setIsRecording(false);
    } finally {
      setIsRecordingDisabled(false);
    }
  }, [controller, setIsRecording, setIsRecordingDisabled]);

  return {
    handleRecordingStop,
    isStopping: controller.command === 'stop',
    isProcessingTranscript: controller.command === 'finalize'
      || state.status === RecordingStatus.PROCESSING_TRANSCRIPTS,
    isSavingTranscript: state.status === RecordingStatus.SAVING,
    summaryStatus: state.status === RecordingStatus.PROCESSING_TRANSCRIPTS ? 'processing' : 'idle',
    setIsStopping: (value) => {
      if (value) state.setStatus(RecordingStatus.STOPPING);
    },
  };
}
