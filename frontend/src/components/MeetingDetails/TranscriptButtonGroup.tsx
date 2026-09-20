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
import { Copy, FolderOpen, Link2, Loader2, MoreHorizontal, RefreshCw, UserRoundCog, Users } from 'lucide-react';
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
  isEnhancing?: boolean;
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
  isEnhancing = false,
}: TranscriptButtonGroupProps) {
  const { betaFeatures } = useConfig();
  const [showRetranscribeDialog, setShowRetranscribeDialog] = useState(false);
  const [isIdentifyingSpeakers, setIsIdentifyingSpeakers] = useState(false);
  // "Copy meeting link" only needs meetingId, so it alone keeps the menu non-empty.
  const hasMoreActions = !locked && Boolean(meetingId);

  const handleIdentifySpeakers = useCallback(async (numSpeakers: number | null) => {
    if (!meetingId || isIdentifyingSpeakers) return;
    setIsIdentifyingSpeakers(true);
    // sonner never auto-dismisses a loading toast and renders no close button for one, so if
    // `run_speaker_diarization` hangs (it has no timeout) the toast would otherwise be stuck
    // on screen for the rest of the session. sonner still renders the `cancel` button for a
    // loading toast and dismisses it on click, so we use that as the escape hatch. This only
    // hides the toast — the diarization run keeps going in the Rust core — so the label says
    // "Dismiss", not "Cancel".
    let toastDismissedByUser = false;
    const toastId = toast.loading('Identifying speakers locally…', {
      cancel: {
        label: 'Dismiss',
        onClick: () => {
          toastDismissedByUser = true;
        },
      },
    });
    try {
      const result = await invoke<{ speaker_count: number }>('run_speaker_diarization', {
        meetingId,
        numSpeakers,
      });
      await onRefetchTranscripts?.();
      const message = `Identified ${result.speaker_count} speaker${result.speaker_count === 1 ? '' : 's'}`;
      // If the user already dismissed the loading toast, reusing its id would merge the
      // stale "cancel" affordance into this toast and could replay it well after the user
      // moved on. Show a fresh toast instead so a late result is still surfaced, but as its
      // own new toast rather than a resurrection of the one the user dismissed.
      toast.success(message, toastDismissedByUser ? undefined : { id: toastId });
    } catch (error) {
      const message = `Speaker identification failed: ${String(error)}`;
      toast.error(message, toastDismissedByUser ? undefined : { id: toastId });
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

        {betaFeatures.importAndRetranscribe && meetingId && meetingFolderPath && !locked && (
          <Button
            size="sm"
            variant="ghost"
            className="h-8 w-8 rounded-full bg-[var(--surface-2)] p-0 text-[var(--ink-muted)] hover:bg-[var(--surface-2)]"
            onClick={() => {
              Analytics.trackButtonClick('enhance_transcript', 'meeting_details');
              setShowRetranscribeDialog(true);
            }}
            disabled={isEnhancing}
            title={isEnhancing ? 'Enhancing transcript…' : 'Enhance transcript'}
            aria-label={isEnhancing ? 'Enhancing transcript…' : 'Enhance transcript'}
          >
            {isEnhancing ? <Loader2 className="h-4 w-4 animate-spin" /> : <RefreshCw size={18} />}
          </Button>
        )}

        {hasMoreActions && <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button size="sm" variant="ghost" className="h-8 rounded-full bg-[var(--surface-2)] px-2.5 text-[var(--ink-muted)] hover:bg-[var(--surface-2)]" title="More transcript actions" aria-label="More transcript actions">
              <MoreHorizontal size={18} />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-52">
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
        />
      )}
    </div>
  );
}
