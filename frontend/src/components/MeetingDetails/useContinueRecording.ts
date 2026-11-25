import { useCallback } from 'react';
import { useOptionalRecordingController } from '@/contexts/RecordingControllerContext';
import { useRecordingState } from '@/contexts/RecordingStateContext';

/**
 * "Continue recording": record more into a finished meeting. Offered only
 * while no recording is running anywhere in the app (this meeting or any
 * other), since a second one would collide with it, and not while `locked`
 * (the AI notes are being written).
 */
export function useContinueRecording(meetingId: string | undefined, locked: boolean) {
  const recordingController = useOptionalRecordingController();
  const { isRecording, isStopping, isProcessing, isSaving, isStartingRecording } = useRecordingState();
  const isRecordingIdle = !isRecording && !isStopping && !isProcessing && !isSaving && !isStartingRecording;
  const available = Boolean(meetingId) && !locked && Boolean(recordingController) && isRecordingIdle;

  const continueRecording = useCallback(async () => {
    if (!meetingId || !recordingController) return;
    try {
      await recordingController.resumeMeeting(meetingId);
    } catch (error) {
      // The controller already surfaces user-facing feedback for a failed resume
      // (its feedback banner), so we only log here to avoid double-reporting.
      console.warn('Continue recording failed:', error);
    }
  }, [meetingId, recordingController]);

  return {
    available,
    pending: recordingController?.isCommandPending ?? false,
    continueRecording,
  };
}
