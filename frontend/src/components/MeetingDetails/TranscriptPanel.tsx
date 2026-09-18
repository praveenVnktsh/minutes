"use client";

import { Transcript, TranscriptSegmentData } from '@/types';
import { VirtualizedTranscriptView } from '@/components/VirtualizedTranscriptView';
import { TranscriptButtonGroup } from './TranscriptButtonGroup';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { ChevronDown, ChevronUp, Loader2, Pause, Play, Search, X } from 'lucide-react';
import { useAudioPlayer } from '@/hooks/useAudioPlayer';
import { useTranscriptionProgress } from '@/hooks/useTranscriptionProgress';
import { SpeakerCorrectionDialog, SpeakerIdentity } from './SpeakerCorrectionDialog';

function formatClock(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '00:00';
  const total = Math.floor(seconds);
  return `${Math.floor(total / 60).toString().padStart(2, '0')}:${(total % 60).toString().padStart(2, '0')}`;
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
  // Local speaker edits applied in place so renaming/reassigning does not
  // refetch (and reset) the transcript scroll position.
  const [speakerNames, setSpeakerNames] = useState<Record<string, string>>({});
  const [segmentSpeakerIds, setSegmentSpeakerIds] = useState<Record<string, string>>({});

  // Live stage-by-stage progress for the meeting currently being transcribed.
  const transcriptionProgress = useTranscriptionProgress(meetingId);

  useEffect(() => {
    setSpeakerNames(
      Object.fromEntries(speakerOptions.map((speaker) => [speaker.speaker_id, speaker.display_name])),
    );
  }, [speakerOptions]);

  // Resolve the meeting's audio so the transcript can jump to a line.
  useEffect(() => {
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

  const handleSeek = useCallback(async (seconds: number) => {
    await player.seek(seconds);
    await player.play();
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

  const activeSegmentId = useMemo(() => {
    if (!audioPath || (!player.isPlaying && player.currentTime <= 0)) return undefined;
    let current: string | undefined;
    for (const segment of convertedSegments) {
      if (segment.timestamp <= player.currentTime + 0.1) current = segment.id;
      else break;
    }
    return current;
  }, [audioPath, convertedSegments, player.currentTime, player.isPlaying]);

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
          <div className="mt-3 flex items-center gap-3">
            <button
              type="button"
              onClick={() => (player.isPlaying ? player.pause() : player.play())}
              className="flex h-8 w-8 items-center justify-center rounded-full bg-[var(--surface-2)] text-[var(--ink-muted)] hover:text-ink"
              title={player.isPlaying ? 'Pause' : 'Play recording'}
              aria-label={player.isPlaying ? 'Pause' : 'Play recording'}
            >
              {player.isPlaying ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
            </button>
            <span className="text-xs tabular-nums text-[var(--ink-subtle)]">
              {formatClock(player.currentTime)} / {formatClock(player.duration)}
            </span>
            <span className="hidden text-[11px] text-[var(--ink-subtle)] @[28rem]:inline">Click a timestamp to play from there</span>
          </div>
        )}
        {locked && convertedSegments.length > 0 && (
          <p className="mt-2 text-[11px] text-[var(--ink-subtle)]">Transcript is locked while the summary is being generated.</p>
        )}
      </div>

      {/* Transcript content - use virtualized view for better performance */}
      {isTranscribing && convertedSegments.length === 0 ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 px-8 pb-16 text-center">
          {transcriptionProgress ? (
            <>
              <Loader2 className="h-6 w-6 animate-spin text-[var(--ink-subtle)]" />
              <p className="text-2xl font-semibold tabular-nums text-[var(--ink-muted)]">
                {transcriptionProgress.percent}%
              </p>
              <div className="w-full max-w-xs">
                <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--surface-2)]">
                  <div
                    className="h-full rounded-full bg-[var(--ink-muted)] transition-all duration-300 ease-out"
                    style={{ width: `${transcriptionProgress.percent}%` }}
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
            </>
          ) : (
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
            onSpeakerChange={meetingId && !locked ? handleSpeakerReassignment : undefined}
            onRenameSpeaker={meetingId && !locked ? handleRenameSpeaker : undefined}
            onSeek={audioPath && !locked ? handleSeek : undefined}
            activeSegmentId={activeSegmentId}
          />
        </div>
      )}

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
