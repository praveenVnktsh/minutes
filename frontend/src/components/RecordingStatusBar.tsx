'use client';

import { motion } from 'framer-motion';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useRecordingController } from '@/contexts/RecordingControllerContext';
import { useEffect, useState } from 'react';

interface RecordingStatusBarProps {
  isPaused?: boolean;
}

export const RecordingStatusBar: React.FC<RecordingStatusBarProps> = ({ isPaused = false }) => {
  // Get recording duration from backend-synced context (in seconds)
  // Backend polls every 500ms, providing smooth updates
  const { activeDuration, isPaused: authoritativePaused } = useRecordingState();
  const { returnToRecording } = useRecordingController();
  const paused = authoritativePaused || isPaused;

  // Display state synced from backend
  const [displaySeconds, setDisplaySeconds] = useState(0);

  // Sync with backend duration when it changes (handles refresh/navigation)
  useEffect(() => {
    if (activeDuration !== null) {
      // Round to nearest second to avoid decimal issues
      setDisplaySeconds(Math.floor(activeDuration));
    }
  }, [activeDuration]);

  const formatDuration = (seconds: number): string => {
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
  };

  return (
    <motion.div
      initial={{ opacity: 0, y: -10 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: -10 }}
      transition={{ duration: 0.2 }}
      className="flex items-center gap-2 px-3 py-2 bg-surface-2 rounded-lg mb-2"
    >
      <div className={`w-2 h-2 rounded-full ${paused ? 'bg-paused' : 'bg-recording animate-pulse'}`} />
      <span className={`text-sm ${paused ? 'text-paused' : 'text-ink'}`}>
        {paused ? 'Paused' : 'Recording'} - {formatDuration(displaySeconds)}
      </span>
      <button
        type="button"
        className="ml-auto rounded px-2 py-1 text-xs font-semibold text-[var(--ink-muted)] hover:bg-[var(--surface-raised)]"
        onClick={() => void returnToRecording()}
      >
        Return to recording
      </button>
    </motion.div>
  );
};
