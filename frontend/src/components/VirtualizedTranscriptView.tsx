'use client';

import { useCallback, useRef, useReducer, startTransition, useEffect, useState, memo, type ReactNode } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ChevronDown } from "lucide-react";
import { useAutoScroll } from "@/hooks/useAutoScroll";
import { useTranscriptStreaming } from "@/hooks/useTranscriptStreaming";
import { ConfidenceIndicator } from "./ConfidenceIndicator";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";
import { RecordingStatusBar } from "./RecordingStatusBar";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import { Button } from "./ui/button";
import { motion, AnimatePresence } from "framer-motion";
import { TranscriptSegmentData } from "@/types";

export interface VirtualizedTranscriptViewProps {
    /** Transcript segments to display */
    segments: TranscriptSegmentData[];
    /** Whether recording is in progress */
    isRecording?: boolean;
    /** Whether recording is paused */
    isPaused?: boolean;
    /** Whether processing/finalizing transcription */
    isProcessing?: boolean;
    /** Whether stopping */
    isStopping?: boolean;
    /** Enable streaming effect for latest segment */
    enableStreaming?: boolean;
    /** Show confidence indicators */
    showConfidence?: boolean;
    /** Completely disable auto-scroll behavior (for meeting details page) */
    disableAutoScroll?: boolean;

    // Pagination props (infinite scroll)
    hasMore?: boolean;
    isLoadingMore?: boolean;
    totalCount?: number;
    loadedCount?: number;
    onLoadMore?: () => void;
    speakerOptions?: Array<{ speaker_id: string; display_name: string }>;
    onSpeakerChange?: (transcriptId: string, speakerId: string) => void;
    onRenameSpeaker?: (speakerId: string, displayName: string) => void;
    /** Play the recording from a segment's timestamp. */
    onSeek?: (seconds: number) => void;
    /** Segment currently playing, highlighted in the list. */
    activeSegmentId?: string;
    /** Ids of segments matching the transcript search. */
    matchIds?: Set<string>;
    /** The search match currently in focus. */
    activeMatchId?: string;
    /** Query to highlight inside segment text. */
    highlightQuery?: string;
}

// Threshold for enabling virtualization (below this, use simple rendering)
const VIRTUALIZATION_THRESHOLD = 10;

// Stable per-speaker chip colors that read well in both themes.
const SPEAKER_CHIP_COLORS = [
    'bg-blue-100 text-blue-700 dark:bg-blue-500/15 dark:text-blue-300',
    'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300',
    'bg-violet-100 text-violet-700 dark:bg-violet-500/15 dark:text-violet-300',
    'bg-amber-100 text-amber-800 dark:bg-amber-500/15 dark:text-amber-300',
    'bg-rose-100 text-rose-700 dark:bg-rose-500/15 dark:text-rose-300',
    'bg-cyan-100 text-cyan-700 dark:bg-cyan-500/15 dark:text-cyan-300',
    'bg-orange-100 text-orange-700 dark:bg-orange-500/15 dark:text-orange-300',
    'bg-pink-100 text-pink-700 dark:bg-pink-500/15 dark:text-pink-300',
];

function speakerChipClass(key: string): string {
    let hash = 0;
    for (let i = 0; i < key.length; i += 1) {
        hash = (hash * 31 + key.charCodeAt(i)) >>> 0;
    }
    return SPEAKER_CHIP_COLORS[hash % SPEAKER_CHIP_COLORS.length];
}

// Helper function to format seconds as recording-relative time [MM:SS]
function formatRecordingTime(seconds: number | undefined): string {
    if (seconds === undefined) return '[--:--]';

    const totalSeconds = Math.floor(seconds);
    const minutes = Math.floor(totalSeconds / 60);
    const secs = totalSeconds % 60;

    return `[${minutes.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}]`;
}

// Helper function to remove filler words and repetitions
function cleanStopWords(text: string): string {
    const stopWords = ['uh', 'um', 'er', 'ah', 'hmm', 'hm', 'eh', 'oh'];

    let cleanedText = text;
    stopWords.forEach(word => {
        const pattern = new RegExp(`\\b${word}\\b[,\\s]*`, 'gi');
        cleanedText = cleanedText.replace(pattern, ' ');
    });

    return cleanedText.replace(/\s+/g, ' ').trim();
}

// Click-to-edit speaker label. Renaming applies to every segment for that
// speaker; the list lets you re-assign just this line.
const EditableSpeakerLabel = memo(function EditableSpeakerLabel({
    transcriptId,
    speaker,
    speakerId,
    speakerOptions,
    onSpeakerChange,
    onRenameSpeaker,
}: {
    transcriptId: string;
    speaker?: string;
    speakerId?: string;
    speakerOptions?: Array<{ speaker_id: string; display_name: string }>;
    onSpeakerChange?: (transcriptId: string, speakerId: string) => void;
    onRenameSpeaker?: (speakerId: string, displayName: string) => void;
}) {
    const label = speaker === 'mic' ? 'You' : speaker === 'system' ? 'Others' : (speaker ?? '');
    const currentId = speakerId || speaker || '';
    const [open, setOpen] = useState(false);
    const [draft, setDraft] = useState(label);

    useEffect(() => {
        if (open) setDraft(label);
    }, [open, label]);

    if (!onRenameSpeaker && !onSpeakerChange) {
        return (
            <span className={`mb-1 inline-flex rounded-full px-2 py-0.5 text-[11px] font-medium ${speakerChipClass(currentId)}`}>
                {label}
            </span>
        );
    }

    const commitRename = () => {
        const next = draft.trim();
        if (!next || next === label) {
            setOpen(false);
            return;
        }
        onRenameSpeaker?.(currentId, next);
        setOpen(false);
    };

    return (
        <Popover open={open} onOpenChange={setOpen}>
            <PopoverTrigger asChild>
                <button
                    type="button"
                    title="Edit speaker name"
                    className={`mb-1 inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium ${speakerChipClass(currentId)}`}
                >
                    {label}
                    <ChevronDown className="h-3 w-3 opacity-70" />
                </button>
            </PopoverTrigger>
            <PopoverContent align="start" className="w-64 space-y-3 p-3">
                <div>
                    <label className="mb-1 block text-[11px] font-medium text-ink-muted">Speaker name</label>
                    <div className="flex items-center gap-2">
                        <input
                            autoFocus
                            value={draft}
                            onChange={(event) => setDraft(event.target.value)}
                            onKeyDown={(event) => {
                                if (event.key === 'Enter') {
                                    event.preventDefault();
                                    commitRename();
                                }
                            }}
                            className="h-8 min-w-0 flex-1 rounded-md border border-hairline bg-surface-1 px-2 text-sm text-ink outline-none focus:border-ink-subtle"
                        />
                        <Button size="sm" onClick={commitRename} disabled={!draft.trim() || draft.trim() === label}>
                            Rename
                        </Button>
                    </div>
                    <p className="mt-1 text-[10px] text-ink-subtle">Renames every line for this speaker.</p>
                </div>
                {onSpeakerChange && speakerOptions && speakerOptions.length > 0 && (
                    <div className="border-t border-hairline pt-2">
                        <div className="mb-1 text-[11px] font-medium text-ink-muted">Assign this line to</div>
                        <div className="flex flex-wrap gap-1">
                            {speakerOptions
                                .filter((option) => option.speaker_id !== currentId)
                                .map((option) => (
                                    <button
                                        key={option.speaker_id}
                                        type="button"
                                        onClick={() => {
                                            onSpeakerChange(transcriptId, option.speaker_id);
                                            setOpen(false);
                                        }}
                                        className="rounded-full bg-surface-2 px-2 py-0.5 text-[11px] text-ink-muted hover:text-ink"
                                    >
                                        {option.display_name}
                                    </button>
                                ))}
                        </div>
                    </div>
                )}
            </PopoverContent>
        </Popover>
    );
});

// Memoized transcript segment component
const TranscriptSegment = memo(function TranscriptSegment({
    id,
    timestamp,
    text,
    confidence,
    speaker,
    speakerId,
    speakerOptions,
    onSpeakerChange,
    onRenameSpeaker,
    onSeek,
    isActive,
    isMatch,
    isActiveMatch,
    highlight,
    isStreaming,
    showConfidence,
}: {
    id: string;
    timestamp: number;
    text: string;
    confidence?: number;
    speaker?: string;
    speakerId?: string;
    speakerOptions?: Array<{ speaker_id: string; display_name: string }>;
    onSpeakerChange?: (transcriptId: string, speakerId: string) => void;
    onRenameSpeaker?: (speakerId: string, displayName: string) => void;
    onSeek?: (seconds: number) => void;
    isActive?: boolean;
    isMatch?: boolean;
    isActiveMatch?: boolean;
    highlight?: string;
    isStreaming: boolean;
    showConfidence: boolean;
}) {
    const displayText = cleanStopWords(text) || (text.trim() === '' ? '[Silence]' : text);

    const highlightClass = isActiveMatch
        ? 'bg-amber-400/25 ring-1 ring-amber-400/60'
        : isMatch
            ? 'bg-amber-400/10'
            : isActive
                ? 'bg-blue-500/15 shadow-[inset_3px_0_0_0_rgb(59,130,246)] ring-1 ring-inset ring-blue-500/35'
                : '';

    const renderText = (value: string) => {
        if (!highlight) return value;
        const lower = value.toLowerCase();
        const needle = highlight.toLowerCase();
        const nodes: ReactNode[] = [];
        let cursor = 0;
        while (cursor <= value.length) {
            const found = lower.indexOf(needle, cursor);
            if (found === -1) {
                nodes.push(value.slice(cursor));
                break;
            }
            if (found > cursor) nodes.push(value.slice(cursor, found));
            nodes.push(
                <mark key={`${found}-${cursor}`} className="rounded bg-amber-300/60 px-0.5 text-inherit">
                    {value.slice(found, found + needle.length)}
                </mark>,
            );
            cursor = found + Math.max(needle.length, 1);
        }
        return nodes;
    };

    return (
        <div
            id={`segment-${id}`}
            data-playing={isActive ? 'true' : undefined}
            aria-current={isActive ? 'true' : undefined}
            className={`mb-3 rounded-lg px-2 -mx-2 transition-colors ${highlightClass}`}
        >
            <div className="flex items-start gap-2">
                <Tooltip>
                    <TooltipTrigger asChild>
                        {onSeek ? (
                            <button
                                type="button"
                                onClick={() => onSeek(timestamp)}
                                title="Play from here"
                                className="mt-1 min-w-[50px] flex-shrink-0 text-left text-xs text-ink-subtle hover:text-ink"
                            >
                                {formatRecordingTime(timestamp)}
                            </button>
                        ) : (
                            <span className="mt-1 min-w-[50px] flex-shrink-0 text-xs text-ink-subtle">
                                {formatRecordingTime(timestamp)}
                            </span>
                        )}
                    </TooltipTrigger>
                    <TooltipContent>
                        {confidence !== undefined && showConfidence && (
                            <ConfidenceIndicator confidence={confidence} showIndicator={showConfidence} />
                        )}
                    </TooltipContent>
                </Tooltip>
                <div className="flex-1">
                    {speaker && (
                        <EditableSpeakerLabel
                            transcriptId={id}
                            speaker={speaker}
                            speakerId={speakerId}
                            speakerOptions={speakerOptions}
                            onSpeakerChange={onSpeakerChange}
                            onRenameSpeaker={onRenameSpeaker}
                        />
                    )}
                    {isStreaming ? (
                        <div className="bg-surface-2 border border-hairline rounded-lg px-3 py-2">
                            <p className="text-base text-ink leading-relaxed">{renderText(displayText)}</p>
                        </div>
                    ) : (
                        <p className="text-base text-ink leading-relaxed">{renderText(displayText)}</p>
                    )}
                </div>
            </div>
        </div>
    );
});

export const VirtualizedTranscriptView: React.FC<VirtualizedTranscriptViewProps> = ({
    segments,
    isRecording = false,
    isPaused = false,
    isProcessing = false,
    isStopping = false,
    enableStreaming = false,
    showConfidence = true,
    disableAutoScroll = false,
    hasMore = false,
    isLoadingMore = false,
    totalCount = 0,
    loadedCount = 0,
    onLoadMore,
    speakerOptions,
    onSpeakerChange,
    onRenameSpeaker,
    onSeek,
    activeSegmentId,
    matchIds,
    activeMatchId,
    highlightQuery,
}) => {
    // Create scroll ref first - shared between virtualizer and auto-scroll hook
    const scrollRef = useRef<HTMLDivElement>(null);
    // Ref for infinite scroll trigger element
    const loadMoreTriggerRef = useRef<HTMLDivElement>(null);

    // Force re-render without flushSync (avoids React warning)
    const [, rerender] = useReducer((x: number) => x + 1, 0);

    // Setup virtualizer for efficient rendering of large lists
    const virtualizer = useVirtualizer({
        count: segments.length,
        getScrollElement: () => scrollRef.current,
        estimateSize: () => 60, // Estimated height per segment
        overscan: 10, // Render extra items above/below viewport
        onChange: () => {
            startTransition(() => {
                rerender();
            });
        },
    });

    // Custom hook for auto-scrolling (supports both virtualized and non-virtualized)
    useAutoScroll({
        scrollRef,
        segments,
        isRecording,
        isPaused,
        activeSegmentId,
        virtualizer,
        virtualizationThreshold: VIRTUALIZATION_THRESHOLD,
        disableAutoScroll,
    });

    // Streaming text effect hook (typewriter animation for new transcripts)
    const { streamingSegmentId, getDisplayText } = useTranscriptStreaming(
        segments,
        isRecording,
        enableStreaming
    );

    // Infinite scroll: IntersectionObserver to trigger loading more
    useEffect(() => {
        if (!onLoadMore || !hasMore || isLoadingMore || isRecording || segments.length === 0) {
            return;
        }

        const triggerElement = loadMoreTriggerRef.current;
        if (!triggerElement) return;

        const observer = new IntersectionObserver(
            (entries) => {
                if (entries[0].isIntersecting && hasMore && !isLoadingMore) {
                    onLoadMore();
                }
            },
            {
                root: null,
                rootMargin: '100px',
                threshold: 0,
            }
        );

        observer.observe(triggerElement);

        return () => observer.disconnect();
    }, [hasMore, isLoadingMore, onLoadMore, isRecording, segments.length]);

    // Scroll-based fallback for fast scrolling
    useEffect(() => {
        if (!onLoadMore || !hasMore || isLoadingMore || isRecording) return;

        const scrollElement = scrollRef.current;
        if (!scrollElement) return;

        let ticking = false;

        const handleScroll = () => {
            if (ticking || isLoadingMore || !hasMore) return;

            ticking = true;
            requestAnimationFrame(() => {
                const { scrollTop, scrollHeight, clientHeight } = scrollElement;
                const scrollBottom = scrollHeight - scrollTop - clientHeight;

                // Trigger load when within 200px of bottom
                if (scrollBottom < 200 && hasMore && !isLoadingMore) {
                    onLoadMore();
                }
                ticking = false;
            });
        };

        scrollElement.addEventListener('scroll', handleScroll, { passive: true });
        return () => scrollElement.removeEventListener('scroll', handleScroll);
    }, [onLoadMore, hasMore, isLoadingMore, isRecording]);

    // Use simple rendering for small lists, virtualization for large lists
    const useVirtualization = segments.length >= VIRTUALIZATION_THRESHOLD;

    useEffect(() => {
        const handleJump = (event: Event) => {
            const { id, index } = (event as CustomEvent<{ id: string; index: number }>).detail;
            if (index < 0 || index >= segments.length) return;
            if (useVirtualization) {
                virtualizer.scrollToIndex(index, { align: 'center', behavior: 'smooth' });
            } else {
                document.getElementById(`segment-${id}`)?.scrollIntoView({ behavior: 'smooth', block: 'center' });
            }
            window.setTimeout(() => {
                document.getElementById(`segment-${id}`)?.animate(
                    [{ backgroundColor: 'rgba(251, 191, 36, .22)' }, { backgroundColor: 'transparent' }],
                    { duration: 1600 },
                );
            }, 250);
        };
        window.addEventListener('meetily:jump-to-transcript', handleJump);
        return () => window.removeEventListener('meetily:jump-to-transcript', handleJump);
    }, [segments.length, useVirtualization, virtualizer]);

    return (
        <div ref={scrollRef} className="flex flex-col h-full overflow-y-auto px-4 py-2">
            {/* Recording Status Bar - Sticky at top, always visible when recording */}
            <AnimatePresence>
                {isRecording && (
                    <div className="sticky top-0 z-10 bg-surface-raised pb-2">
                        <RecordingStatusBar isPaused={isPaused} />
                    </div>
                )}
            </AnimatePresence>

            {/* Content - add padding when recording to prevent overlap */}
            <div className={isRecording ? 'pt-2' : ''}>
            {segments.length === 0 ? (
                // Empty state
                <motion.div
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    className="text-center text-ink-muted mt-8"
                >
                    {isRecording ? (
                        <>
                            <div className="flex items-center justify-center mb-3">
                                <div className={`w-3 h-3 rounded-full ${isPaused ? 'bg-orange-500' : 'bg-blue-500 animate-pulse'}`}></div>
                            </div>
                            <p className="text-sm text-ink-muted">
                                {isPaused ? 'Recording paused' : 'Listening for speech...'}
                            </p>
                            <p className="text-xs mt-1 text-ink-subtle">
                                {isPaused ? 'Click resume to continue recording' : 'Speak to see live transcription'}
                            </p>
                        </>
                    ) : (
                        <>
                            <p className="text-lg font-semibold">Welcome to Minutes!</p>
                            <p className="text-xs mt-1">Start recording to see live transcription</p>
                        </>
                    )}
                </motion.div>
            ) : useVirtualization ? (
                // Virtualized rendering for large lists
                <>
                    <div
                        style={{
                            height: virtualizer.getTotalSize(),
                            width: "100%",
                            position: "relative",
                        }}
                    >
                        {virtualizer.getVirtualItems().map((virtualRow) => {
                            const segment = segments[virtualRow.index];
                            const isStreaming = streamingSegmentId === segment.id;

                            return (
                                <div
                                    key={segment.id}
                                    data-index={virtualRow.index}
                                    ref={virtualizer.measureElement}
                                    style={{
                                        position: "absolute",
                                        top: 0,
                                        left: 0,
                                        width: "100%",
                                        transform: `translateY(${virtualRow.start}px)`,
                                    }}
                                >
                                    <TranscriptSegment
                                        id={segment.id}
                                        timestamp={segment.timestamp}
                                        text={getDisplayText(segment)}
                                        confidence={segment.confidence}
                                        speaker={segment.speaker}
                                        speakerId={segment.speakerId}
                                        speakerOptions={speakerOptions}
                                        onSpeakerChange={onSpeakerChange}
                                        onRenameSpeaker={onRenameSpeaker}
                                        onSeek={onSeek}
                                        isActive={segment.id === activeSegmentId}
                                        isMatch={matchIds?.has(segment.id) ?? false}
                                        isActiveMatch={segment.id === activeMatchId}
                                        highlight={highlightQuery}
                                        isStreaming={isStreaming}
                                        showConfidence={showConfidence}
                                    />
                                </div>
                            );
                        })}
                    </div>

                    {/* Infinite scroll trigger and loading indicator */}
                    {(hasMore || isLoadingMore) && !isRecording && segments.length > 0 && (
                        <div ref={loadMoreTriggerRef} className="flex justify-center items-center py-4 mt-2">
                            {isLoadingMore ? (
                                <div className="flex items-center gap-2 text-ink-muted">
                                    <div className="w-4 h-4 border-2 border-hairline border-t-gray-600 rounded-full animate-spin" />
                                    <span className="text-sm">Loading more...</span>
                                </div>
                            ) : hasMore && totalCount > 0 ? (
                                <span className="text-sm text-ink-subtle">
                                    Showing {loadedCount} of {totalCount} segments
                                </span>
                            ) : null}
                        </div>
                    )}

                    {/* Listening indicator when recording */}
                    {!isStopping && isRecording && !isPaused && !isProcessing && segments.length > 0 && (
                        <motion.div
                            initial={{ opacity: 0 }}
                            animate={{ opacity: 1 }}
                            exit={{ opacity: 0 }}
                            className="flex items-center gap-2 mt-4 text-ink-muted"
                        >
                            <div className="w-2 h-2 bg-blue-500 rounded-full animate-pulse"></div>
                            <span className="text-sm">Listening...</span>
                        </motion.div>
                    )}
                </>
            ) : (
                // Simple rendering for small lists (better animations)
                <>
                    <div className="space-y-1">
                        {segments.map((segment) => {
                            const isStreaming = streamingSegmentId === segment.id;

                            return (
                                <motion.div
                                    key={segment.id}
                                    initial={{ opacity: 0, y: 5 }}
                                    animate={{ opacity: 1, y: 0 }}
                                    transition={{ duration: 0.15 }}
                                >
                                    <TranscriptSegment
                                        id={segment.id}
                                        timestamp={segment.timestamp}
                                        text={getDisplayText(segment)}
                                        confidence={segment.confidence}
                                        speaker={segment.speaker}
                                        speakerId={segment.speakerId}
                                        speakerOptions={speakerOptions}
                                        onSpeakerChange={onSpeakerChange}
                                        onRenameSpeaker={onRenameSpeaker}
                                        onSeek={onSeek}
                                        isActive={segment.id === activeSegmentId}
                                        isMatch={matchIds?.has(segment.id) ?? false}
                                        isActiveMatch={segment.id === activeMatchId}
                                        highlight={highlightQuery}
                                        isStreaming={isStreaming}
                                        showConfidence={showConfidence}
                                    />
                                </motion.div>
                            );
                        })}
                    </div>

                    {/* Infinite scroll trigger (for small lists that grow) */}
                    {(hasMore || isLoadingMore) && !isRecording && segments.length > 0 && (
                        <div ref={loadMoreTriggerRef} className="flex justify-center items-center py-4 mt-2">
                            {isLoadingMore ? (
                                <div className="flex items-center gap-2 text-ink-muted">
                                    <div className="w-4 h-4 border-2 border-hairline border-t-gray-600 rounded-full animate-spin" />
                                    <span className="text-sm">Loading more...</span>
                                </div>
                            ) : hasMore && totalCount > 0 ? (
                                <span className="text-sm text-ink-subtle">
                                    Showing {loadedCount} of {totalCount} segments
                                </span>
                            ) : null}
                        </div>
                    )}

                    {/* Listening indicator when recording */}
                    {!isStopping && isRecording && !isPaused && !isProcessing && segments.length > 0 && (
                        <motion.div
                            initial={{ opacity: 0 }}
                            animate={{ opacity: 1 }}
                            exit={{ opacity: 0 }}
                            className="flex items-center gap-2 mt-4 text-ink-muted"
                        >
                            <div className="w-2 h-2 bg-blue-500 rounded-full animate-pulse"></div>
                            <span className="text-sm">Listening...</span>
                        </motion.div>
                    )}
                </>
            )}
            </div>
        </div>
    );
};
