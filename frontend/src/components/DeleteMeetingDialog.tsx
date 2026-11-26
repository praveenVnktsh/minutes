'use client'

import { useEffect, useState } from 'react'
import { Loader2, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { StatusFeedback } from '@/components/ui/status-feedback'
import { useSidebar } from '@/components/Sidebar/SidebarProvider'
import { useMeetingActivity } from '@/contexts/MeetingActivityContext'
import { folderNotRemovedMessage, meetingDeleteBlockedReason } from '@/lib/meetingDeletion'
import { errorMessage } from '@/lib/meetingCatalog'

export interface DeleteMeetingTarget {
  id: string
  title?: string | null
}

/**
 * Asks before permanently deleting a meeting, then deletes it. While the
 * meeting is still recording, transcribing or writing its AI notes the dialog
 * says so and Delete stays disabled; the Rust core refuses those deletes too.
 */
export function DeleteMeetingDialog({
  meeting,
  onOpenChange,
  onDeleted,
}: {
  /** The meeting to delete; null keeps the dialog closed. */
  meeting: DeleteMeetingTarget | null
  onOpenChange: (open: boolean) => void
  /** Runs after the meeting is gone (e.g. to leave its page). */
  onDeleted?: (meetingId: string) => void | Promise<void>
}) {
  const { deleteMeeting } = useSidebar()
  const activity = useMeetingActivity()
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const meetingId = meeting?.id ?? null

  useEffect(() => {
    setPending(false)
    setError(null)
  }, [meetingId])

  const title = meeting?.title?.trim() || 'Untitled meeting'
  const blockedReason = meeting
    ? meetingDeleteBlockedReason(meeting.id, {
      recording: activity.recording,
      activities: activity.getMeetingActivities(meeting.id),
      summaryStatus: activity.getSummaryActivity(meeting.id)?.status ?? null,
    })
    : null

  const close = (open: boolean) => {
    if (pending) return
    onOpenChange(open)
  }

  const confirm = async () => {
    if (!meeting || pending || blockedReason) return
    setPending(true)
    setError(null)
    try {
      const result = await deleteMeeting(meeting.id)
      const folderWarning = folderNotRemovedMessage(result)
      if (folderWarning) toast.error(folderWarning, { duration: 15000 })
      else toast.success(`Deleted “${title}”`)
      setPending(false)
      onOpenChange(false)
      await onDeleted?.(meeting.id)
    } catch (deleteError) {
      setPending(false)
      setError(errorMessage(deleteError))
    }
  }

  return (
    <Dialog open={meeting !== null} onOpenChange={close}>
      <DialogContent className="sm:max-w-[440px]">
        <DialogHeader>
          <DialogTitle>Delete “{title}”?</DialogTitle>
          <DialogDescription>
            The meeting&apos;s notes, transcript and recording are permanently deleted. This can&apos;t be undone.
          </DialogDescription>
        </DialogHeader>
        {blockedReason && <StatusFeedback tone="warning">{blockedReason}</StatusFeedback>}
        {error && <StatusFeedback tone="error">{error}</StatusFeedback>}
        <DialogFooter>
          <Button variant="outline" disabled={pending} onClick={() => close(false)}>Cancel</Button>
          <Button
            variant="destructive"
            disabled={pending || Boolean(blockedReason)}
            aria-label={`Delete ${title} permanently`}
            onClick={() => void confirm()}
          >
            {pending ? <Loader2 className="animate-spin" /> : <Trash2 />}
            {pending ? 'Deleting…' : 'Delete'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
