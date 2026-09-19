'use client';

import { useEffect, useRef } from 'react';
import { toast } from 'sonner';
import { useConfig } from '@/contexts/ConfigContext';
import {
  claimAutoSummaryJob,
  generateAutomaticSummary,
  releaseAutoSummaryJob,
} from '@/lib/autoSummary';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';

export function AutoSummaryProvider() {
  const { isAutoSummary, isModelConfigLoading, modelConfig } = useConfig();
  const { snapshot: { activities } } = useMeetingActivity();
  const activeMeetingIds = useRef(new Set<string>());
  const pendingByMeeting = useRef(new Map<string, string>());

  useEffect(() => {
    const startSummary = async (taskId: string, meetingId: string) => {
      if (!isAutoSummary || isModelConfigLoading) return;
      if (activeMeetingIds.current.has(meetingId)) {
        pendingByMeeting.current.set(meetingId, taskId);
        return;
      }
      if (!claimAutoSummaryJob(taskId, modelConfig)) return;
      activeMeetingIds.current.add(meetingId);
      // Compatibility signal for the current workspace. c11 removes its
      // route-owned auto-generation check and this event consumer.
      if (typeof window !== 'undefined') {
        window.dispatchEvent(new CustomEvent('meetily:auto-summary-requested', {
          detail: { meetingId },
        }));
      }

      try {
        await generateAutomaticSummary(meetingId, modelConfig);
      } catch (error) {
        releaseAutoSummaryJob(taskId, modelConfig);
        console.error('[AutoSummary] Failed to start summary:', error);
        toast.error('Automatic summary could not start', {
          description: error instanceof Error ? error.message : String(error),
        });
      } finally {
        activeMeetingIds.current.delete(meetingId);
        const pendingTaskId = pendingByMeeting.current.get(meetingId);
        if (pendingTaskId) {
          pendingByMeeting.current.delete(meetingId);
          void startSummary(pendingTaskId, meetingId);
        }
      }
    };

    if (isAutoSummary && !isModelConfigLoading) {
      const latestReadyByMeeting = new Map<string, (typeof activities)[number]>();
      for (const activity of activities) {
        if (activity.status !== 'ready' || !activity.meeting_id) continue;
        const current = latestReadyByMeeting.get(activity.meeting_id);
        if (!current || activity.revision > current.revision) {
          latestReadyByMeeting.set(activity.meeting_id, activity);
        }
      }
      for (const activity of latestReadyByMeeting.values()) {
        void startSummary(activity.task_id, activity.meeting_id!);
      }
    }
  }, [activities, isAutoSummary, isModelConfigLoading, modelConfig]);

  return null;
}
