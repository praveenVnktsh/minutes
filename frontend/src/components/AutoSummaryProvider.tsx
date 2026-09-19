'use client';

import { useEffect, useRef } from 'react';
import { toast } from 'sonner';
import { useConfig } from '@/contexts/ConfigContext';
import {
  claimAutoSummaryJob,
  generateAutomaticSummary,
} from '@/lib/autoSummary';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import type { ModelConfig } from '@/services/configService';

export function AutoSummaryProvider() {
  const {
    isAutoSummary,
    isModelConfigLoading,
    isModelConfigSaving,
    modelConfigSaveError,
    modelConfig,
  } = useConfig();
  const { snapshot: { activities } } = useMeetingActivity();
  const activeMeetingIds = useRef(new Set<string>());
  const pendingByMeeting = useRef(new Map<string, { taskId: string; config: ModelConfig }>());

  useEffect(() => {
    const startSummary = async (taskId: string, meetingId: string, config: ModelConfig) => {
      if (!isAutoSummary || isModelConfigLoading || isModelConfigSaving || modelConfigSaveError) return;
      if (activeMeetingIds.current.has(meetingId)) {
        pendingByMeeting.current.set(meetingId, { taskId, config });
        return;
      }
      if (!claimAutoSummaryJob(taskId, config)) return;
      activeMeetingIds.current.add(meetingId);
      // Compatibility signal for the current workspace. c11 removes its
      // route-owned auto-generation check and this event consumer.
      if (typeof window !== 'undefined') {
        window.dispatchEvent(new CustomEvent('meetily:auto-summary-requested', {
          detail: { meetingId },
        }));
      }

      try {
        await generateAutomaticSummary(meetingId, config);
      } catch (error) {
        console.error('[AutoSummary] Failed to start summary:', error);
        toast.error('Automatic summary could not start', {
          description: error instanceof Error ? error.message : String(error),
        });
      } finally {
        activeMeetingIds.current.delete(meetingId);
        const pending = pendingByMeeting.current.get(meetingId);
        if (pending) {
          pendingByMeeting.current.delete(meetingId);
          void startSummary(pending.taskId, meetingId, pending.config);
        }
      }
    };

    if (isAutoSummary && !isModelConfigLoading && !isModelConfigSaving && !modelConfigSaveError) {
      const latestReadyByMeeting = new Map<string, (typeof activities)[number]>();
      for (const activity of activities) {
        if (activity.status !== 'ready' || !activity.meeting_id) continue;
        const current = latestReadyByMeeting.get(activity.meeting_id);
        if (!current || activity.revision > current.revision) {
          latestReadyByMeeting.set(activity.meeting_id, activity);
        }
      }
      for (const activity of latestReadyByMeeting.values()) {
        void startSummary(activity.task_id, activity.meeting_id!, modelConfig);
      }
    }
  }, [activities, isAutoSummary, isModelConfigLoading, isModelConfigSaving, modelConfig, modelConfigSaveError]);

  return null;
}
