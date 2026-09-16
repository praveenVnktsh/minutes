"use client";

import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Copy, FolderOpen, Link2, MoreHorizontal, RefreshCw, UserRoundCog, Users } from 'lucide-react';
import { toast } from 'sonner';
import Analytics from '@/lib/analytics';
import { RetranscribeDialog } from './RetranscribeDialog';
import { useConfig } from '@/contexts/ConfigContext';


interface TranscriptButtonGroupProps {
  transcriptCount: number;
  onCopyTranscript: () => void;
  onOpenMeetingFolder: () => Promise<void>;
  meetingId?: string;
  meetingFolderPath?: string | null;
  onRefetchTranscripts?: () => Promise<void>;
  onOpenSpeakerManager?: () => void;
  locked?: boolean;
}


export function TranscriptButtonGroup({
  transcriptCount,
  onCopyTranscript,
  onOpenMeetingFolder,
  meetingId,
  meetingFolderPath,
  onRefetchTranscripts,
  onOpenSpeakerManager,
  locked = false,
}: TranscriptButtonGroupProps) {
  const { betaFeatures } = useConfig();
  const [showRetranscribeDialog, setShowRetranscribeDialog] = useState(false);
  const [isIdentifyingSpeakers, setIsIdentifyingSpeakers] = useState(false);
  const hasMoreActions = !locked && Boolean(
    meetingId && (
      (betaFeatures.importAndRetranscribe && meetingFolderPath)
      || (transcriptCount > 0 && (meetingFolderPath || onOpenSpeakerManager))
    )
  );

  const handleRetranscribeComplete = useCallback(async () => {
    // Refetch transcripts to show the updated data
    if (onRefetchTranscripts) {
      await onRefetchTranscripts();
    }
  }, [onRefetchTranscripts]);

  const handleIdentifySpeakers = useCallback(async (numSpeakers: number | null) => {
    if (!meetingId || isIdentifyingSpeakers) return;
    setIsIdentifyingSpeakers(true);
    const toastId = toast.loading('Identifying speakers locally…');
    try {
      const result = await invoke<{ speaker_count: number }>('run_speaker_diarization', {
        meetingId,
        numSpeakers,
      });
      await onRefetchTranscripts?.();
      toast.success(
        `Identified ${result.speaker_count} speaker${result.speaker_count === 1 ? '' : 's'}`,
        { id: toastId },
      );
    } catch (error) {
      toast.error(`Speaker identification failed: ${String(error)}`, { id: toastId });
    } finally {
      setIsIdentifyingSpeakers(false);
    }
  }, [isIdentifyingSpeakers, meetingId, onRefetchTranscripts]);

  return (
    <div className="flex w-full items-center justify-end gap-2">
      <div className="flex items-center gap-1.5">
        <Button
          variant="ghost"
          size="sm"
          className="h-8 rounded-full bg-[var(--surface-2)] px-3 text-[var(--ink-muted)] hover:bg-[var(--surface-2)]"
          onClick={() => {
            Analytics.trackButtonClick('copy_transcript', 'meeting_details');
            onCopyTranscript();
          }}
          disabled={transcriptCount === 0}
          title={transcriptCount === 0 ? 'No transcript available' : 'Copy Transcript'}
        >
          <Copy />
          <span className="hidden @[22rem]:inline">Copy</span>
        </Button>

        <Button
          size="sm"
          variant="ghost"
          className="h-8 rounded-full bg-[var(--surface-2)] px-3 text-[var(--ink-muted)] hover:bg-[var(--surface-2)]"
          onClick={() => {
            Analytics.trackButtonClick('open_recording_folder', 'meeting_details');
            onOpenMeetingFolder();
          }}
          title="Open Recording Folder"
        >
          <FolderOpen className="@[22rem]:mr-2" size={18} />
          <span className="hidden @[22rem]:inline">Recording</span>
        </Button>

        {meetingId && transcriptCount > 0 && onOpenSpeakerManager && !locked && (
          <Button
            size="sm"
            variant="ghost"
            className="h-8 rounded-full bg-[var(--surface-2)] px-3 text-[var(--ink-muted)] hover:bg-[var(--surface-2)]"
            onClick={() => {
              Analytics.trackButtonClick('manage_speakers', 'meeting_details');
              onOpenSpeakerManager();
            }}
            title="Manage speaker names"
          >
            <UserRoundCog className="@[22rem]:mr-2" size={18} />
            <span className="hidden @[22rem]:inline">Speakers</span>
          </Button>
        )}

        {hasMoreActions && <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button size="sm" variant="ghost" className="h-8 rounded-full bg-[var(--surface-2)] px-2.5 text-[var(--ink-muted)] hover:bg-[var(--surface-2)]" title="More transcript actions" aria-label="More transcript actions">
              <MoreHorizontal size={18} />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-52">
            {betaFeatures.importAndRetranscribe && meetingId && meetingFolderPath && (
              <DropdownMenuItem onClick={() => {
                Analytics.trackButtonClick('enhance_transcript', 'meeting_details');
                setShowRetranscribeDialog(true);
              }}>
                <RefreshCw className="mr-2 h-4 w-4" /> Enhance transcript
              </DropdownMenuItem>
            )}
            {meetingId && meetingFolderPath && transcriptCount > 0 && (
              <DropdownMenuSub>
                <DropdownMenuSubTrigger disabled={isIdentifyingSpeakers}>
                  <Users className={`mr-2 h-4 w-4 ${isIdentifyingSpeakers ? 'animate-pulse' : ''}`} />
                  {isIdentifyingSpeakers ? 'Identifying speakers…' : 'Identify speakers'}
                </DropdownMenuSubTrigger>
                <DropdownMenuSubContent className="w-44">
                  <DropdownMenuItem onClick={() => void handleIdentifySpeakers(null)}>
                    Auto-detect
                  </DropdownMenuItem>
                  {[2, 3, 4, 5, 6].map((count) => (
                    <DropdownMenuItem key={count} onClick={() => void handleIdentifySpeakers(count)}>
                      {count} speakers
                    </DropdownMenuItem>
                  ))}
                </DropdownMenuSubContent>
              </DropdownMenuSub>
            )}
            {meetingId && transcriptCount > 0 && onOpenSpeakerManager && (
              <DropdownMenuItem onClick={onOpenSpeakerManager}>
                <UserRoundCog className="mr-2 h-4 w-4" /> Manage speaker names
              </DropdownMenuItem>
            )}
            {meetingId && (
              <DropdownMenuItem
                onClick={() => {
                  void navigator.clipboard.writeText(`minutes://meeting/${meetingId}`);
                  toast.success('Meeting link copied');
                }}
              >
                <Link2 className="mr-2 h-4 w-4" /> Copy meeting link
              </DropdownMenuItem>
            )}
          </DropdownMenuContent>
        </DropdownMenu>}
      </div>

      {betaFeatures.importAndRetranscribe && meetingId && meetingFolderPath && (
        <RetranscribeDialog
          open={showRetranscribeDialog}
          onOpenChange={setShowRetranscribeDialog}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onComplete={handleRetranscribeComplete}
        />
      )}
    </div>
  );
}
