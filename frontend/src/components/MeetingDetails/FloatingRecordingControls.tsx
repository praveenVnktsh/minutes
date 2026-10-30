'use client';

import { RecordingControls } from '@/components/RecordingControls';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useRecordingController } from '@/contexts/RecordingControllerContext';
import { SIDEBAR_COLLAPSED_WIDTH, SIDEBAR_WIDTH, useShell } from '@/contexts/ShellContext';

/** Floating recording bar shown over the workspace while a meeting is recording. */
export function FloatingRecordingControls({ onStopInitiated }: { onStopInitiated?: () => void }) {
  const recordingState = useRecordingState();
  const controller = useRecordingController();
  const { collapsed } = useShell();

  return (
    <div
      className="pointer-events-none fixed bottom-8 right-0 z-30 transition-[left] duration-200"
      style={{ left: collapsed ? SIDEBAR_COLLAPSED_WIDTH : SIDEBAR_WIDTH }}
    >
      <div className="flex justify-center">
        <div className="pointer-events-auto">
          <RecordingControls
            isRecording={recordingState.isRecording}
            onRecordingStop={() => controller.stopRecording()}
            onRecordingStart={() => controller.startRecording()}
            onStopInitiated={onStopInitiated}
            isRecordingDisabled={false}
            isParentProcessing={recordingState.isProcessing}
          />
        </div>
      </div>
    </div>
  );
}
