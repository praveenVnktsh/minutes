"use client";

import { Transcript, TranscriptSegmentData } from '@/types';
import { VirtualizedTranscriptView } from '@/components/VirtualizedTranscriptView';
import { TranscriptButtonGroup } from './TranscriptButtonGroup';
import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { ChevronDown, ChevronUp, Loader2, Search, X } from 'lucide-react';
import { useAudioPlayer } from '@/hooks/useAudioPlayer';
import { useTranscriptionProgress } from '@/hooks/useTranscriptionProgress';
import { useDiarizationStatus } from '@/hooks/useDiarizationStatus';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import { SpeakerCorrectionDialog, SpeakerIdentity } from './SpeakerCorrectionDialog';
import { AudioScrubber } from './AudioScrubber';
import { toneText } from '@/lib/theme-classes';
import { FINAL_PASS_COMPLETE_EVENT, type FinalPassComplete } from '@/lib/liveTranscriptionModels';
import { SpeakerStatusNotice } from './SpeakerStatusNotice';
import { ModelProvenanceNote } from './ModelProvenanceNote';
import { resolveMatchTarget, searchMeetingTranscript, transcriptMatchIds, type TranscriptMatch } from '@/lib/transcriptSearch';

/**
 * The one status line under the transcript header: a playback error first,
 * then the lock while AI notes are written, then the speaker status.
 */
export function TranscriptStatusLine({
  playerError,
  locked,
  speakerNotice,
}: {
  playerError?: string | null;
  locked: boolean;
  speakerNotice: ReactNode;
}) {
  if (playerError) return <p role="alert" className={`mt-1 text-[11px] ${toneText.error}`}>{playerError}</p>;
  if (locked) return <p className="mt-2 text-[11px] text-[var(--ink-subtle)]">Transcript is locked while the AI notes are written.</p>;
  return <>{speakerNotice}</>;
}

export function findSegmentIdAtTime(
  segments: Array<{ id: string; timestamp: number; endTime?: number }>,
  time: number,
): string | undefined {
  if (segments.length === 0 || !Number.isFinite(time)) return undefined;
  let current: string | undefined;
  for (let i = 0; i < segments.length; i += 1) {
    const segment = segments[i];
    const next = segments[i + 1];
    const end = segment.endTime ?? next?.timestamp ?? Number.POSITIVE_INFINITY;
    if (time + 0.05 < segment.timestamp) break;
    if (time < end || !next) current = segment.id;
  }
  return current;
}

// Decides which line is lit up. A remembered seek target is the last resort so
// that a click still highlights its line while the file is decoding and
// playback has not moved off 0 yet - otherwise the click looks like it missed.
export function resolveActiveSegmentId(
  segments: Array<{ id: string; timestamp: number; endTime?: number }>,
  state: {
    hasAudio: boolean;
    scrubTime: number | null;
    seekTarget: number | null;
    isPlaying: boolean;
    currentTime: number;
  },
): string | undefined {
  if (!state.hasAudio) return undefined;
  const time = state.scrubTime
    ?? (state.isPlaying || state.currentTime > 0 ? state.currentTime : state.seekTarget);
  if (time === null) return undefined;
  return findSegmentIdAtTime(segments, time);
}

interface TranscriptPanelProps {
  transcripts: Transcript[];
  onCopyTranscript: () => void;
  onOpenMeetingFolder: () => Promise<void>;
  isRecording: boolean;
  isTranscribing?: boolean;
  locked?: boolean;
  disableAutoScroll?: boolean;

  // Optional pagination props (when using virtualization)
  usePagination?: boolean;
  segments?: TranscriptSegmentData[];
  hasMore?: boolean;
  isLoadingMore?: boolean;
  totalCount?: number;
  loadedCount?: number;
  onLoadMore?: () => void;
  /** Loads every remaining row (used to reach a search match not loaded yet). */
  onLoadAll?: () => Promise<void>;

  // Retranscription props
  meetingId?: string;
  meetingFolderPath?: string | null;
  onRefetchTranscripts?: () => Promise<void>;
}

export function TranscriptPanel({
  transcripts,
  onCopyTranscript,
  onOpenMeetingFolder,
  isRecording,
  isTranscribing = false,
  locked = false,
  disableAutoScroll = false,
  usePagination = false,
  segments,
  hasMore,
  isLoadingMore,
  totalCount,
  loadedCount,
  onLoadMore,
  onLoadAll,
  meetingId,
  meetingFolderPath,
  onRefetchTranscripts,
}: TranscriptPanelProps) {
  const [showSpeakerDialog, setShowSpeakerDialog] = useState(false);
  const [speakerOptions, setSpeakerOptions] = useState<SpeakerIdentity[]>([]);
  const [searchQuery, setSearchQuery] = useState('');
  const [searchOpen, setSearchOpen] = useState(false);
  const [audioPath, setAudioPath] = useState<string | null>(null);
  const [scrubTime, setScrubTime] = useState<number | null>(null);
  // The position last asked for, remembered so the clicked line lights up
  // before the audio has decoded far enough to report it back.
  const [seekTarget, setSeekTarget] = useState<number | null>(null);
  // Local speaker edits applied in place so renaming/reassigning does not
  // refetch (and reset) the transcript scroll position.
  const [speakerNames, setSpeakerNames] = useState<Record<string, string>>({});
  const [segmentSpeakerIds, setSegmentSpeakerIds] = useState<Record<string, string>>({});

  // Live stage-by-stage progress for the meeting currently being transcribed,
  // whether that is its first pass or a re-run over an existing transcript.
  const transcriptionProgress = useTranscriptionProgress(meetingId);
  const { cancelTranscription } = useMeetingActivity();

  // A running pass does not lock the transcript: speaker corrections are kept
  // by time, so they carry over to the rows the pass saves.
  const editsLocked = locked;
  // Once a pass reaches its speaker stage the new transcript is saved, so it
  // stays readable with a status line instead of sitting under the overlay.
  const isDiarizingPass = transcriptionProgress?.isDiarization ?? false;

  const { status: diarizationStatus } = useDiarizationStatus(meetingId);
  const [isStartingDiarization, setIsStartingDiarization] = useState(false);
  const handleRetryDiarization = useCallback(async () => {
    if (!meetingId || isStartingDiarization) return;
    setIsStartingDiarization(true);
    try {
      // Status and transcript refreshes arrive as events while this runs.
      await invoke('run_speaker_diarization', { meetingId, numSpeakers: null });
    } catch (error) {
      toast.error(`Speaker identification failed: ${String(error)}`);
    } finally {
      setIsStartingDiarization(false);
    }
  }, [isStartingDiarization, meetingId]);

  useEffect(() => {
    setSpeakerNames(
      Object.fromEntries(speakerOptions.map((speaker) => [speaker.speaker_id, speaker.display_name])),
    );
  }, [speakerOptions]);

  // Resolve the meeting's audio so the transcript can jump to a line.
  useEffect(() => {
    setSeekTarget(null);
    if (!meetingId) {
      setAudioPath(null);
      return;
    }
    let cancelled = false;
    invoke<string | null>('api_get_meeting_audio_path', { meetingId })
      .then((path) => {
        if (!cancelled) setAudioPath(path ?? null);
      })
      .catch(() => {
        if (!cancelled) setAudioPath(null);
      });
    return () => {
      cancelled = true;
    };
  }, [meetingId]);

  const player = useAudioPlayer(audioPath);

  // Playing the recording changes nothing on disk, so unlike the speaker edits
  // this stays available while a summary or re-transcription pass is running.
  const handleSeek = useCallback(async (seconds: number) => {
    setScrubTime(null);
    setSeekTarget(seconds);
    await player.seek(seconds);
    // seek() restarts the source when it is already running, so playing again
    // here would restart it a second time from the same position.
    if (!player.isPlaying) await player.play();
  }, [player]);

  const handleScrubPreview = useCallback((time: number) => {
    setScrubTime(time);
  }, []);

  const handleScrubCommit = useCallback(async (time: number) => {
    setScrubTime(null);
    setSeekTarget(time);
    await player.seek(time);
  }, [player]);

  const handleTogglePlayback = useCallback(() => {
    if (player.isPlaying) player.pause();
    else void player.play();
  }, [player]);

  const refreshSpeakers = useCallback(async () => {
    if (!meetingId) return;
    try {
      setSpeakerOptions(await invoke<SpeakerIdentity[]>('get_speaker_identities', { meetingId }));
    } catch (error) {
      console.warn('Could not load speaker identities:', error);
    }
  }, [meetingId]);

  useEffect(() => {
    if (meetingId) void refreshSpeakers();
  }, [meetingId, refreshSpeakers, segments]);

  const handleSpeakerReassignment = useCallback(async (transcriptId: string, speakerId: string) => {
    if (!meetingId) return;
    // Apply optimistically; no transcript refetch so scroll position is kept.
    setSegmentSpeakerIds((current) => ({ ...current, [transcriptId]: speakerId }));
    try {
      await invoke('reassign_transcript_speaker', { meetingId, transcriptId, speakerId });
      await refreshSpeakers();
      toast.success('Transcript segment reassigned');
    } catch (error) {
      setSegmentSpeakerIds((current) => {
        const next = { ...current };
        delete next[transcriptId];
        return next;
      });
      toast.error(`Could not reassign speaker: ${String(error)}`);
    }
  }, [meetingId, refreshSpeakers]);

  const handleRenameSpeaker = useCallback(async (speakerId: string, displayName: string) => {
    if (!meetingId) return;
    // Apply optimistically to every segment with this speaker id.
    const previous = speakerNames[speakerId];
    setSpeakerNames((current) => ({ ...current, [speakerId]: displayName }));
    try {
      await invoke('rename_speaker', { meetingId, speakerId, displayName });
      await refreshSpeakers();
      toast.success(`Renamed to ${displayName}`);
    } catch (error) {
      setSpeakerNames((current) => {
        const next = { ...current };
        if (previous === undefined) delete next[speakerId];
        else next[speakerId] = previous;
        return next;
      });
      toast.error(`Could not rename speaker: ${String(error)}`);
    }
  }, [meetingId, refreshSpeakers, speakerNames]);

  // Merging remaps speaker identities, so reload transcripts from the database.
  const handleSpeakersChanged = useCallback(async () => {
    await onRefetchTranscripts?.();
    await refreshSpeakers();
  }, [onRefetchTranscripts, refreshSpeakers]);

  // Convert transcripts to segments, then apply any in-place speaker edits.
  const convertedSegments = useMemo(() => {
    const base = (usePagination && segments)
      ? segments
      : transcripts.map(t => ({
          id: t.id,
          timestamp: t.audio_start_time ?? 0,
          endTime: t.audio_end_time,
          text: t.text,
          confidence: t.confidence,
          speaker: t.speaker,
          speakerId: t.speaker_id,
        }));

    return base.map((segment) => {
      const speakerId = segmentSpeakerIds[segment.id] ?? segment.speakerId ?? segment.speaker;
      const displayName = (speakerId ? speakerNames[speakerId] : undefined)
        ?? segment.speaker
        ?? speakerId;
      if (speakerId === segment.speakerId && displayName === segment.speaker) return segment;
      return { ...segment, speakerId, speaker: displayName };
    });
  }, [transcripts, usePagination, segments, speakerNames, segmentSpeakerIds]);

  const playbackTime = scrubTime ?? player.currentTime;

  const activeSegmentId = useMemo(() => resolveActiveSegmentId(convertedSegments, {
    hasAudio: Boolean(audioPath),
    scrubTime,
    seekTarget,
    isPlaying: player.isPlaying,
    currentTime: player.currentTime,
  }), [audioPath, convertedSegments, player.currentTime, player.isPlaying, scrubTime, seekTarget]);

  const normalizedQuery = searchQuery.trim().toLowerCase();

  // The whole transcript is searched in the database, so a match in a row
  // that is not loaded yet still counts and can be jumped to.
  const [remoteMatches, setRemoteMatches] = useState<TranscriptMatch[] | null>(null);
  const searchedRowCount = usePagination ? (totalCount ?? convertedSegments.length) : convertedSegments.length;
  useEffect(() => {
    setRemoteMatches(null);
    if (!meetingId || !normalizedQuery) return undefined;
    let cancelled = false;
    const timer = setTimeout(() => {
      searchMeetingTranscript(meetingId, normalizedQuery)
        .then((matches) => {
          if (!cancelled) setRemoteMatches(matches);
        })
        .catch((error) => {
          // The rows on screen are still searched.
          if (!cancelled) console.warn('Could not search the whole transcript:', error);
        });
    }, 200);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [meetingId, normalizedQuery, searchedRowCount]);

  const matchIdList = useMemo(
    () => transcriptMatchIds(convertedSegments, normalizedQuery, remoteMatches),
    [convertedSegments, normalizedQuery, remoteMatches],
  );
  const matchIds = useMemo(() => new Set(matchIdList), [matchIdList]);

  const [activeMatch, setActiveMatch] = useState(0);
  useEffect(() => {
    setActiveMatch(0);
  }, [normalizedQuery]);

  const activeMatchIndex = matchIdList.length
    ? Math.min(activeMatch, matchIdList.length - 1)
    : -1;
  const activeMatchId = activeMatchIndex >= 0 ? matchIdList[activeMatchIndex] : undefined;

  // A match beyond the loaded rows: load the rest, then jump once it is there.
  const [pendingJumpId, setPendingJumpId] = useState<string | null>(null);
  const jumpTo = useCallback((id: string, index: number) => {
    window.dispatchEvent(new CustomEvent('meetily:jump-to-transcript', { detail: { id, index } }));
  }, []);
  useEffect(() => {
    if (!pendingJumpId) return;
    const target = resolveMatchTarget(convertedSegments, pendingJumpId, false);
    if (target.kind !== 'jump') return;
    setPendingJumpId(null);
    // Let the view render the new rows before scrolling to one.
    requestAnimationFrame(() => jumpTo(pendingJumpId, target.index));
  }, [convertedSegments, jumpTo, pendingJumpId]);

  const goToMatch = useCallback((delta: number) => {
    if (matchIdList.length === 0) return;
    const next = (activeMatch + delta + matchIdList.length) % matchIdList.length;
    setActiveMatch(next);
    const id = matchIdList[next];
    const canLoadMore = Boolean(hasMore && (onLoadAll || onLoadMore));
    const target = resolveMatchTarget(convertedSegments, id, canLoadMore);
    if (target.kind === 'jump') {
      jumpTo(id, target.index);
    } else if (target.kind === 'load') {
      setPendingJumpId(id);
      if (onLoadAll) void onLoadAll();
      else onLoadMore?.();
    }
  }, [activeMatch, convertedSegments, hasMore, jumpTo, matchIdList, onLoadAll, onLoadMore]);

  // Keep paging toward a pending match when only page-by-page loading exists.
  useEffect(() => {
    if (!pendingJumpId || onLoadAll || !hasMore || isLoadingMore) return;
    if (resolveMatchTarget(convertedSegments, pendingJumpId, true).kind === 'load') onLoadMore?.();
  }, [convertedSegments, hasMore, isLoadingMore, onLoadAll, onLoadMore, pendingJumpId]);

  // Say when the background pass has swapped in its transcript, or kept the
  // current one.
  useEffect(() => {
    if (!meetingId) return undefined;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<FinalPassComplete>(FINAL_PASS_COMPLETE_EVENT, ({ payload }) => {
      if (payload.meeting_id !== meetingId) return;
      if (payload.outcome === 'replaced') {
        toast.success('Transcript improved', {
          description: payload.model ? `Re-transcribed with ${payload.model}.` : undefined,
        });
      } else {
        toast.info('Kept the live transcript', { description: payload.warning ?? undefined });
      }
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    }).catch((error) => console.warn('Could not listen for final transcript passes:', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [meetingId]);

  // No optimistic state: the activity snapshot removes the surface once the
  // cancellation actually lands.
  const handleCancelTranscription = useCallback(async () => {
    if (!transcriptionProgress) return;
    try {
      await cancelTranscription(transcriptionProgress.taskId);
    } catch (error) {
      toast.error(`Could not cancel transcription: ${String(error)}`);
    }
  }, [cancelTranscription, transcriptionProgress]);

  // The full progress surface, shown only in place of an empty transcript (a
  // first transcription). A pass over an existing one runs in the background
  // and the meeting header's banner is its only progress surface.
  const progressSurface = transcriptionProgress ? (
    <div
      role="status"
      aria-live="polite"
      className="flex flex-col items-center justify-center gap-3 px-8 text-center"
    >
      <Loader2 className="h-6 w-6 animate-spin text-[var(--ink-subtle)]" />
      {/* A pass that has not reported a percentage yet gets a pulsing bar
          rather than a "0%" that reads as a stalled measurement. */}
      {!transcriptionProgress.indeterminate && (
        <p className="text-2xl font-semibold tabular-nums text-[var(--ink-muted)]">
          {transcriptionProgress.percent}%
        </p>
      )}
      <div className="w-full max-w-xs">
        <div
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={transcriptionProgress.indeterminate ? undefined : transcriptionProgress.percent}
          className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--surface-2)]"
        >
          <div
            className={`h-full rounded-full bg-[var(--ink-muted)] ${
              transcriptionProgress.indeterminate
                ? 'w-full animate-pulse opacity-40'
                : 'transition-all duration-300 ease-out'
            }`}
            style={transcriptionProgress.indeterminate ? undefined : { width: `${transcriptionProgress.percent}%` }}
          />
        </div>
      </div>
      <div>
        <p className="text-sm font-medium text-[var(--ink-muted)]">
          {transcriptionProgress.stageLabel}…
        </p>
        <p className="mt-1 text-xs text-[var(--ink-subtle)]">
          {transcriptionProgress.message
            ?? 'This can take a moment. Your notes are safe and stay editable meanwhile.'}
        </p>
      </div>
      {transcriptionProgress.cancellable && (
        <button
          type="button"
          onClick={() => void handleCancelTranscription()}
          className="h-8 rounded-full bg-[var(--surface-2)] px-3 text-xs font-medium text-[var(--ink-muted)] hover:text-ink"
        >
          Cancel
        </button>
      )}
    </div>
  ) : null;

  return (
    <div className="flex h-full min-w-0 w-full bg-[var(--surface-0)] flex-col relative @container">
      {/* Title area */}
      <div className="mx-auto w-full max-w-[900px] px-8 pb-2 pt-4">
        <TranscriptButtonGroup
          transcriptCount={usePagination ? (totalCount ?? convertedSegments.length) : (transcripts?.length || 0)}
          onCopyTranscript={onCopyTranscript}
          onOpenMeetingFolder={onOpenMeetingFolder}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onRefetchTranscripts={onRefetchTranscripts}
          onOpenSpeakerManager={() => setShowSpeakerDialog(true)}
          locked={locked}
          isEnhancing={transcriptionProgress?.kind === 'retranscription'}
          searchOpen={searchOpen}
          onToggleSearch={() => {
            if (searchOpen) setSearchQuery('');
            setSearchOpen(!searchOpen);
          }}
          leading={meetingId ? <ModelProvenanceNote meetingId={meetingId} /> : undefined}
        />
        {searchOpen && <div className="mt-3 flex items-center gap-2">
          <div className="relative min-w-0 flex-1">
            <Search className="absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-[var(--ink-subtle)]" />
            <input
              value={searchQuery}
              onChange={(event) => setSearchQuery(event.target.value)}
              autoFocus
              aria-label="Search transcript"
              onKeyDown={(event) => {
                if (event.key === 'Escape') {
                  event.preventDefault();
                  setSearchQuery('');
                  setSearchOpen(false);
                  return;
                }
                if (event.key === 'Enter') {
                  event.preventDefault();
                  goToMatch(event.shiftKey ? -1 : 1);
                }
              }}
              placeholder="Search transcript…"
              className="h-9 w-full rounded-xl border border-hairline bg-[var(--surface-1)] pl-9 pr-9 text-sm text-ink outline-none placeholder:text-[var(--ink-subtle)] focus:border-[var(--ink-subtle)]"
            />
            {searchQuery && (
              <button
                type="button"
                onClick={() => setSearchQuery('')}
                className="absolute right-2.5 top-1/2 -translate-y-1/2 rounded p-1 text-[var(--ink-subtle)] hover:bg-[var(--surface-2)]"
                aria-label="Clear transcript search"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            )}
          </div>
          {normalizedQuery && (
            <div className="flex shrink-0 items-center gap-1">
              <span className="text-[11px] tabular-nums text-[var(--ink-subtle)]">
                {matchIdList.length === 0 ? 'No matches' : `${activeMatchIndex + 1}/${matchIdList.length}`}
              </span>
              <button
                type="button"
                onClick={() => goToMatch(-1)}
                disabled={matchIdList.length === 0}
                title="Previous match"
                aria-label="Previous match"
                className="rounded p-1 text-[var(--ink-subtle)] hover:bg-[var(--surface-2)] hover:text-ink disabled:opacity-40"
              >
                <ChevronUp className="h-3.5 w-3.5" />
              </button>
              <button
                type="button"
                onClick={() => goToMatch(1)}
                disabled={matchIdList.length === 0}
                title="Next match"
                aria-label="Next match"
                className="rounded p-1 text-[var(--ink-subtle)] hover:bg-[var(--surface-2)] hover:text-ink disabled:opacity-40"
              >
                <ChevronDown className="h-3.5 w-3.5" />
              </button>
            </div>
          )}
        </div>}
        {audioPath && (
          <AudioScrubber
            currentTime={playbackTime}
            duration={player.duration}
            isPlaying={player.isPlaying}
            onTogglePlayback={handleTogglePlayback}
            onScrubPreview={handleScrubPreview}
            onScrubCommit={handleScrubCommit}
          />
        )}
        <TranscriptStatusLine
          playerError={player.error}
          locked={locked && convertedSegments.length > 0}
          speakerNotice={meetingId && meetingFolderPath && convertedSegments.length > 0 && !isRecording ? (
            <SpeakerStatusNotice
              status={diarizationStatus}
              progress={isDiarizingPass && transcriptionProgress ? {
                percent: transcriptionProgress.percent,
                indeterminate: transcriptionProgress.indeterminate,
                label: transcriptionProgress.stageLabel,
              } : null}
              isStarting={isStartingDiarization}
              onRetry={() => void handleRetryDiarization()}
            />
          ) : null}
        />
      </div>

      {/* Transcript content - use virtualized view for better performance */}
      <div className="relative flex min-h-0 flex-1 flex-col">
        {(transcriptionProgress || isTranscribing) && convertedSegments.length === 0 ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 px-8 pb-16 text-center">
            {progressSurface ?? (
              <>
                <Loader2 className="h-6 w-6 animate-spin text-[var(--ink-subtle)]" />
                <div>
                  <p className="text-sm font-medium text-[var(--ink-muted)]">Transcribing meeting audio…</p>
                  <p className="mt-1 text-xs text-[var(--ink-subtle)]">This can take a moment. Your notes are safe and stay editable meanwhile.</p>
                </div>
              </>
            )}
          </div>
        ) : (
          <div className="mx-auto flex w-full max-w-[900px] flex-1 flex-col overflow-hidden pb-4">
            {/* A note, not a replacement: the rows (and their load-more) stay. */}
            {normalizedQuery && matchIdList.length === 0 && (
              <p role="status" className="px-8 pb-2 text-center text-sm text-[var(--ink-subtle)]">
                No transcript matches “{searchQuery.trim()}”.
              </p>
            )}
            <div className="min-h-0 flex-1 overflow-hidden">
              <VirtualizedTranscriptView
                segments={convertedSegments}
                matchIds={matchIds}
                activeMatchId={activeMatchId}
                highlightQuery={normalizedQuery}
                isRecording={isRecording}
                isPaused={false}
                isProcessing={false}
                isStopping={false}
                enableStreaming={false}
                disableAutoScroll={disableAutoScroll}
                hasMore={hasMore}
                isLoadingMore={isLoadingMore}
                totalCount={totalCount}
                loadedCount={loadedCount}
                onLoadMore={onLoadMore}
                speakerOptions={speakerOptions}
                onSpeakerChange={meetingId && !editsLocked ? handleSpeakerReassignment : undefined}
                onRenameSpeaker={meetingId && !editsLocked ? handleRenameSpeaker : undefined}
                onSeek={audioPath ? handleSeek : undefined}
                activeSegmentId={activeSegmentId}
              />
            </div>
          </div>
        )}

      </div>

      {meetingId && (
        <SpeakerCorrectionDialog
          open={showSpeakerDialog}
          onOpenChange={setShowSpeakerDialog}
          meetingId={meetingId}
          speakers={speakerOptions}
          onChanged={handleSpeakersChanged}
          onRenamed={handleRenameSpeaker}
        />
      )}
    </div>
  );
}
