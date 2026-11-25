"use client";

import { useState, useCallback, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Copy, FolderOpen, Link2, Loader2, Mic, MoreHorizontal, RefreshCw, Search, UserRoundCog, Users } from 'lucide-react';
import { toast } from 'sonner';
import { copyMeetingLink } from '@/lib/clipboard';
import { RetranscribeDialog } from './RetranscribeDialog';
import { useContinueRecording } from './useContinueRecording';


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
  /** Whether the transcript search box is open, and how to toggle it. */
  searchOpen?: boolean;
  onToggleSearch?: () => void;
  /** Shown at the start of the row (the model provenance info icon). */
  leading?: ReactNode;
}

const pill = 'h-8 rounded-full bg-[var(--surface-2)] px-3 text-[var(--ink-muted)] hover:bg-[var(--surface-2)]';

/**
 * The transcript header: Search and Speakers as buttons (plus Re-transcribe
 * while there is no transcript yet, when it is the thing to do), and
 * everything else in the ⋯ menu.
 */
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
  searchOpen = false,
  onToggleSearch,
  leading,
}: TranscriptButtonGroupProps) {
  const continueRecording = useContinueRecording(meetingId, locked);
  const [showRetranscribeDialog, setShowRetranscribeDialog] = useState(false);
  const [isIdentifyingSpeakers, setIsIdentifyingSpeakers] = useState(false);
  const canRetranscribe = Boolean(meetingId && meetingFolderPath) && !locked;
  // Re-transcribing is the primary action only while there is nothing to read.
  const retranscribeIsPrimary = canRetranscribe && (transcriptCount === 0 || isEnhancing);
  const canManageSpeakers = Boolean(meetingId) && transcriptCount > 0 && Boolean(onOpenSpeakerManager) && !locked;
  const handleCopyMeetingLink = useCallback(async () => {
    if (!meetingId) return;
    try {
      await copyMeetingLink(meetingId);
      toast.success('Meeting link copied');
    } catch (error) {
      toast.error('Could not copy meeting link', {
        description: error instanceof Error ? error.message : String(error),
      });
    }
  }, [meetingId]);
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
      {leading && <div className="mr-auto flex items-center">{leading}</div>}
      <div className="flex items-center gap-1.5">
        {onToggleSearch && (
          <Button
            size="sm"
            variant="ghost"
            className={`${pill} ${searchOpen ? 'text-ink' : ''}`}
            onClick={onToggleSearch}
            disabled={transcriptCount === 0}
            aria-pressed={searchOpen}
            title="Search transcript"
          >
            <Search className="@[22rem]:mr-2" size={18} />
            <span className="hidden @[22rem]:inline">Search</span>
          </Button>
        )}

        {canManageSpeakers && (
          <Button
            size="sm"
            variant="ghost"
            className={pill}
            onClick={() => onOpenSpeakerManager?.()}
            title="Manage speaker names"
          >
            <UserRoundCog className="@[22rem]:mr-2" size={18} />
            <span className="hidden @[22rem]:inline">Speakers</span>
          </Button>
        )}

        {retranscribeIsPrimary && (
          <Button
            size="sm"
            variant="ghost"
            className={pill}
            onClick={() => setShowRetranscribeDialog(true)}
            disabled={isEnhancing}
            title={isEnhancing ? 'Re-transcribing…' : 'Re-transcribe'}
          >
            {isEnhancing ? <Loader2 className="h-4 w-4 animate-spin @[22rem]:mr-2" /> : <RefreshCw className="@[22rem]:mr-2" size={18} />}
            <span className="hidden @[22rem]:inline">{isEnhancing ? 'Re-transcribing…' : 'Re-transcribe'}</span>
          </Button>
        )}

        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button size="sm" variant="ghost" className="h-8 rounded-full bg-[var(--surface-2)] px-2.5 text-[var(--ink-muted)] hover:bg-[var(--surface-2)]" title="More transcript actions" aria-label="More transcript actions">
              <MoreHorizontal size={18} />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-56">
            <DropdownMenuItem disabled={transcriptCount === 0} onClick={() => onCopyTranscript()}>
              <Copy className="mr-2 h-4 w-4" /> Copy transcript
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => void onOpenMeetingFolder()}>
              <FolderOpen className="mr-2 h-4 w-4" /> Open recording folder
            </DropdownMenuItem>
            {meetingId && (
              <DropdownMenuItem onClick={() => void handleCopyMeetingLink()}>
                <Link2 className="mr-2 h-4 w-4" /> Copy meeting link
              </DropdownMenuItem>
            )}
            {(continueRecording.available || canRetranscribe) && (
              <DropdownMenuSeparator />
            )}
            {continueRecording.available && (
              <DropdownMenuItem
                disabled={continueRecording.pending || isEnhancing}
                onClick={() => void continueRecording.continueRecording()}
              >
                <Mic className="mr-2 h-4 w-4" /> Continue recording
              </DropdownMenuItem>
            )}
            {canRetranscribe && !retranscribeIsPrimary && (
              <DropdownMenuItem disabled={isEnhancing} onClick={() => setShowRetranscribeDialog(true)}>
                <RefreshCw className="mr-2 h-4 w-4" /> Re-transcribe
              </DropdownMenuItem>
            )}
            {canRetranscribe && transcriptCount > 0 && (
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
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {meetingId && meetingFolderPath && (
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
