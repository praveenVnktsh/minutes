"use client";

import { Transcript, TranscriptSegmentData } from '@/types';
import { VirtualizedTranscriptView } from '@/components/VirtualizedTranscriptView';
import { TranscriptButtonGroup } from './TranscriptButtonGroup';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { ChevronDown, ChevronUp, Loader2, Search, X } from 'lucide-react';
import { useAudioPlayer } from '@/hooks/useAudioPlayer';
import { useTranscriptionProgress } from '@/hooks/useTranscriptionProgress';
import { useDiarizationStatus } from '@/hooks/useDiarizationStatus';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import { SpeakerCorrectionDialog, SpeakerIdentity } from './SpeakerCorrectionDialog';
import { AudioScrubber } from './AudioScrubber';
import { toneText } from '@/lib/theme-classes';
import { SpeakerStatusNotice } from './SpeakerStatusNotice';
import { ModelProvenanceNote } from './ModelProvenanceNote';

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
  meetingId,
  meetingFolderPath,
  onRefetchTranscripts,
}: TranscriptPanelProps) {
  const [showSpeakerDialog, setShowSpeakerDialog] = useState(false);
  const [speakerOptions, setSpeakerOptions] = useState<SpeakerIdentity[]>([]);
  const [searchQuery, setSearchQuery] = useState('');
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

  // Segments are about to be replaced by the running pass, so edits made now
  // would be thrown away.
  const editsLocked = locked || transcriptionProgress !== null;
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

  const matchIndices = useMemo(() => {
    if (!normalizedQuery) return [] as number[];
    const indices: number[] = [];
    convertedSegments.forEach((segment, index) => {
      if (segment.text.toLowerCase().includes(normalizedQuery)) indices.push(index);
    });
    return indices;
  }, [convertedSegments, normalizedQuery]);

  const matchIds = useMemo(
    () => new Set(matchIndices.map((index) => convertedSegments[index].id)),
    [matchIndices, convertedSegments],
  );

  const [activeMatch, setActiveMatch] = useState(0);
  useEffect(() => {
    setActiveMatch(0);
  }, [normalizedQuery]);

  const activeMatchIndex = matchIndices.length
    ? Math.min(activeMatch, matchIndices.length - 1)
    : -1;
  const activeMatchId = activeMatchIndex >= 0
    ? convertedSegments[matchIndices[activeMatchIndex]]?.id
    : undefined;

  const goToMatch = useCallback((delta: number) => {
    if (matchIndices.length === 0) return;
    const next = (activeMatch + delta + matchIndices.length) % matchIndices.length;
    setActiveMatch(next);
    const index = matchIndices[next];
    const id = convertedSegments[index]?.id;
    if (id) {
      window.dispatchEvent(new CustomEvent('meetily:jump-to-transcript', { detail: { id, index } }));
    }
  }, [activeMatch, matchIndices, convertedSegments]);

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

  // One copy of the progress markup, shown either in place of an empty
  // transcript or laid over an existing one.
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
        />
        <div className="mt-3 flex items-center gap-2">
          <div className="relative min-w-0 flex-1">
            <Search className="absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-[var(--ink-subtle)]" />
            <input
              value={searchQuery}
              onChange={(event) => setSearchQuery(event.target.value)}
              onKeyDown={(event) => {
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
                {matchIndices.length === 0 ? 'No matches' : `${activeMatchIndex + 1}/${matchIndices.length}`}
              </span>
              <button
                type="button"
                onClick={() => goToMatch(-1)}
                disabled={matchIndices.length === 0}
                title="Previous match"
                aria-label="Previous match"
                className="rounded p-1 text-[var(--ink-subtle)] hover:bg-[var(--surface-2)] hover:text-ink disabled:opacity-40"
              >
                <ChevronUp className="h-3.5 w-3.5" />
              </button>
              <button
                type="button"
                onClick={() => goToMatch(1)}
                disabled={matchIndices.length === 0}
                title="Next match"
                aria-label="Next match"
                className="rounded p-1 text-[var(--ink-subtle)] hover:bg-[var(--surface-2)] hover:text-ink disabled:opacity-40"
              >
                <ChevronDown className="h-3.5 w-3.5" />
              </button>
            </div>
          )}
        </div>
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
        {player.error && (
          <p className={`mt-1 text-[11px] ${toneText.error}`}>{player.error}</p>
        )}
        {meetingId && meetingFolderPath && convertedSegments.length > 0 && !isRecording
          && (!transcriptionProgress || isDiarizingPass) && (
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
        )}
        {meetingId && <ModelProvenanceNote meetingId={meetingId} />}
        {locked && convertedSegments.length > 0 && (
          <p className="mt-2 text-[11px] text-[var(--ink-subtle)]">Transcript is locked while the summary is being generated.</p>
        )}
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
        ) : normalizedQuery && matchIndices.length === 0 ? (
          <div className="flex flex-1 items-center justify-center px-8 pb-16 text-center text-sm text-[var(--ink-subtle)]">
            No transcript matches “{searchQuery.trim()}”.
          </div>
        ) : (
          <div className="mx-auto w-full max-w-[900px] flex-1 overflow-hidden pb-4">
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
              showConfidence={true}
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
        )}

        {/* A pass over an existing transcript keeps it visible but inert. */}
        {progressSurface && convertedSegments.length > 0 && !isDiarizingPass && (
          <div className="absolute inset-0 z-20 flex flex-col items-center justify-center pb-16">
            {/* Separate scrim layer so the progress text itself stays opaque. */}
            <div aria-hidden="true" className="absolute inset-0 bg-[var(--surface-0)] opacity-[0.85]" />
            <div className="relative">{progressSurface}</div>
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
