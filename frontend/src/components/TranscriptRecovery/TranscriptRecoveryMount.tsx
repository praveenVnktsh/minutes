'use client';

import { useRouter } from 'next/navigation';
import { toast } from 'sonner';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import { useRecordingController } from '@/contexts/RecordingControllerContext';
import { TranscriptRecovery } from './TranscriptRecovery';

/** Route-independent recovery surface for the application shell. */
export function TranscriptRecoveryMount() {
  const router = useRouter();
  const { refetchMeetings } = useSidebar();
  const controller = useRecordingController();
  const { recovery } = controller;

  const recover = async (meetingId: string) => {
    const result = await recovery.recoverMeeting(meetingId);
    await refetchMeetings();
    if (result.meetingId) {
      toast.success('Meeting recovered', {
        description: result.transcriptCount > 0 ? 'Transcript recovery completed.' : 'Audio recovery completed.',
      });
      router.push(`/meeting-details?id=${encodeURIComponent(result.meetingId)}`);
    }
    return result;
  };

  return (
    <TranscriptRecovery
      isOpen={controller.isRecoveryOpen}
      onClose={controller.closeRecovery}
      recoverableMeetings={recovery.recoverableMeetings}
      onRecover={recover}
      onDelete={recovery.deleteRecoverableMeeting}
      onLoadPreview={recovery.loadMeetingTranscripts}
    />
  );
}
