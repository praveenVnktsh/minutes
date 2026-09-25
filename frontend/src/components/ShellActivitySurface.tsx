'use client'

import { useState } from 'react'
import { usePathname } from 'next/navigation'
import { Pause, Play, Square, X } from 'lucide-react'
import { MeetingActivityFeedback } from '@/app/_components/StatusOverlays'
import { useSidebar } from '@/components/Sidebar/SidebarProvider'
import { Button } from '@/components/ui/button'
import { StatusFeedback } from '@/components/ui/status-feedback'
import { useMeetingActivity } from '@/contexts/MeetingActivityContext'
import { useRecordingController } from '@/contexts/RecordingControllerContext'
import { useRecordingState } from '@/contexts/RecordingStateContext'

export function ShellActivitySurface() {
  const controller = useRecordingController()
  const recordingState = useRecordingState()
  const { snapshot, summaries, retrySummary, cancelSummary, dismissSummary } = useMeetingActivity()
  const { currentMeeting } = useSidebar()
  const pathname = usePathname()
  const [showAll, setShowAll] = useState(false)
  const [hiddenKeys, setHiddenKeys] = useState<Set<string>>(new Set())
  const hideKey = (key: string) => setHiddenKeys((previous) => new Set(previous).add(key))
  const workspaceMeetingId = pathname === '/meeting-details' ? currentMeeting?.id : null
  const activityCandidates = snapshot.activities.filter((activity) => (
    activity.kind !== 'recording'
    && (!workspaceMeetingId || activity.meeting_id !== workspaceMeetingId)
    && (['starting', 'queued', 'transcribing', 'paused', 'saving', 'failed'].includes(activity.status)
      || (activity.status === 'ready' && Boolean(activity.warning)))
  )).sort((left, right) => {
    const leftTerminal = left.status === 'failed' || left.status === 'ready'
    const rightTerminal = right.status === 'failed' || right.status === 'ready'
    return Number(leftTerminal) - Number(rightTerminal) || right.revision - left.revision
  })
  const summaryCandidates = summaries.filter((summary) => (
    (!workspaceMeetingId || summary.meetingId !== workspaceMeetingId)
    && ['queued', 'processing', 'failed'].includes(summary.status)
  )).sort((left, right) => Number(left.status === 'failed') - Number(right.status === 'failed') || right.revision - left.revision)
  const allWork = [
    ...activityCandidates.map((activity) => ({ type: 'activity' as const, id: activity.task_id, key: `${activity.task_id}:${activity.status}`, activity })),
    ...summaryCandidates.map((summary) => ({ type: 'summary' as const, id: summary.activityId, key: `${summary.activityId}:${summary.status}`, summary })),
  ].sort((left, right) => {
    const leftStatus = left.type === 'activity' ? left.activity.status : left.summary.status
    const rightStatus = right.type === 'activity' ? right.activity.status : right.summary.status
    return Number(leftStatus === 'failed' || leftStatus === 'ready') - Number(rightStatus === 'failed' || rightStatus === 'ready')
  }).filter((work) => !hiddenKeys.has(work.key))
  const visibleWork = showAll ? allWork : allWork.slice(0, 3)
  const commandIsFinalizing = controller.command === 'stop' || controller.command === 'finalize'
  // The live meeting workspace renders its own FloatingRecordingControls, so the shell card
  // would duplicate the pause/stop controls there.
  const isOnLiveWorkspace = Boolean(workspaceMeetingId) && workspaceMeetingId === controller.activeMeetingId
  const showRecording = recordingState.isRecording && !commandIsFinalizing && !isOnLiveWorkspace
  const showFinalizing = commandIsFinalizing || (!recordingState.isRecording && (recordingState.isProcessing || recordingState.isSaving))

  if (!showRecording && !showFinalizing && allWork.length === 0) return null
  return (
    <aside aria-label="Meeting activity" className="fixed bottom-4 right-4 z-30 w-[min(360px,calc(100vw-2rem))] space-y-2">
      {showRecording && (
        <div className="rounded-xl border border-recording/30 bg-surface-raised p-3 shadow-lg">
          <div className="flex items-center gap-2">
            <span className={`h-2.5 w-2.5 rounded-full ${recordingState.isPaused ? 'bg-paused' : 'animate-pulse bg-recording'}`} />
            <span className="text-sm font-semibold text-ink">{recordingState.isPaused ? 'Recording paused' : 'Recording in progress'}</span>
            <div className="ml-auto flex gap-2">
              {controller.activeMeetingId && <Button size="sm" variant="ghost" disabled={controller.isCommandPending} onClick={() => void controller.returnToRecording().catch(() => {})}>Return</Button>}
              <Button
                size="sm"
                variant="ghost"
                disabled={controller.isCommandPending}
                aria-label={recordingState.isPaused ? 'Resume recording' : 'Pause recording'}
                onClick={() => void (recordingState.isPaused ? controller.resumeRecording() : controller.pauseRecording()).catch(() => {})}
              >
                {recordingState.isPaused ? <Play className="h-3 w-3" /> : <Pause className="h-3 w-3" />}
                {recordingState.isPaused ? 'Resume' : 'Pause'}
              </Button>
              <Button size="sm" variant="recording" disabled={controller.isCommandPending} onClick={() => void controller.stopRecording().catch(() => {})}><Square className="h-3 w-3" /> Stop</Button>
            </div>
          </div>
          {!controller.activeMeetingId && <StatusFeedback tone="warning" className="mt-2 text-xs">Capture is active while the meeting workspace is being prepared. Stop remains available here.</StatusFeedback>}
        </div>
      )}
      {showFinalizing && <div className="rounded-xl border border-hairline bg-surface-raised p-3 shadow-lg"><StatusFeedback pending tone="info">{recordingState.isSaving ? 'Saving meeting…' : 'Finishing transcription…'}</StatusFeedback></div>}
      {visibleWork.map((work) => work.type === 'activity' ? (
        <div key={work.id} className="rounded-xl border border-hairline bg-surface-raised p-3 shadow-lg">
          <div className="mb-1 flex items-start gap-2">
            <p className="min-w-0 flex-1 truncate text-xs font-semibold text-ink">{work.activity.title}</p>
            <button type="button" aria-label={`Dismiss ${work.activity.title}`} className="shrink-0 rounded-sm text-ink-muted hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus focus-visible:ring-offset-2" onClick={() => hideKey(work.key)}>
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
          <MeetingActivityFeedback activity={work.activity} />
          {work.activity.progress_percentage !== null && (
            <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-surface-2" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={work.activity.progress_percentage}>
              <div className="h-full bg-info" style={{ width: `${Math.max(0, Math.min(100, work.activity.progress_percentage))}%` }} />
            </div>
          )}
        </div>
      ) : (
        <div key={work.id} className="rounded-xl border border-hairline bg-surface-raised p-3 shadow-lg">
          <div className="mb-1 flex items-start gap-2">
            <p className="min-w-0 flex-1 truncate text-xs font-semibold text-ink">{work.summary.response?.meetingName ?? 'Meeting summary'}</p>
            <button
              type="button"
              aria-label={`Dismiss ${work.summary.response?.meetingName ?? 'Meeting summary'}`}
              className="shrink-0 rounded-sm text-ink-muted hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus focus-visible:ring-offset-2"
              onClick={() => {
                hideKey(work.key)
                if (work.summary.status === 'failed') dismissSummary(work.summary.meetingId, work.summary.processId ?? undefined)
              }}
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
          <StatusFeedback
            pending={work.summary.status === 'queued' || work.summary.status === 'processing'}
            tone={work.summary.status === 'failed' ? 'error' : 'info'}
            actionLabel={work.summary.status === 'failed' ? 'Retry' : work.summary.processId ? 'Cancel' : undefined}
            onAction={work.summary.status === 'failed'
              ? () => void retrySummary(work.summary.meetingId).catch(() => {})
              : work.summary.processId
                ? () => void cancelSummary(work.summary.meetingId, work.summary.processId!).catch(() => {})
                : undefined}
          >
            {work.summary.error ?? (work.summary.status === 'queued' ? 'Summary queued…' : 'Generating summary…')}
          </StatusFeedback>
        </div>
      ))}
      {allWork.length > 3 && (
        <button type="button" className="w-full rounded-lg bg-surface-raised px-3 py-2 text-xs font-semibold text-ink-muted shadow" onClick={() => setShowAll((value) => !value)}>
          {showAll ? 'Show less' : `Show ${allWork.length - 3} more activities`}
        </button>
      )}
    </aside>
  )
}
