import { useRef, useState, useEffect, useCallback, RefObject } from "react";
import { Virtualizer } from "@tanstack/react-virtual";

interface UseAutoScrollProps {
    scrollRef: RefObject<HTMLDivElement | null>;
    segments: any[];
    isRecording: boolean;
    isPaused: boolean;
    activeSegmentId?: string;
    virtualizer?: Virtualizer<HTMLDivElement, Element>;
    virtualizationThreshold?: number;
    disableAutoScroll?: boolean; // Completely disable auto-scroll behavior (for meeting details page)
}

interface UseAutoScrollReturn {
    autoScroll: boolean;
    setAutoScroll: (value: boolean) => void;
    scrollToBottom: () => void;
}

const SCROLL_THRESHOLD = 100;
export const FOLLOW_THROTTLE_MS = 180;
export const USER_SCROLL_KEYS = new Set([
    "ArrowUp",
    "ArrowDown",
    "PageUp",
    "PageDown",
    "Home",
    "End",
    " ",
]);

export function isUserScrollKey(key: string): boolean {
    return USER_SCROLL_KEYS.has(key);
}

export function isScrollbarPointer(event: { clientX: number; clientY: number }, container: HTMLElement): boolean {
    const vertical = container.offsetWidth - container.clientWidth;
    const horizontal = container.offsetHeight - container.clientHeight;
    const rect = container.getBoundingClientRect();
    return (vertical > 0 && event.clientX >= rect.right - vertical)
        || (horizontal > 0 && event.clientY >= rect.bottom - horizontal);
}

export function isActiveSegmentJump(previousIndex: number, nextIndex: number): boolean {
    if (nextIndex < 0) return false;
    return previousIndex < 0 || Math.abs(nextIndex - previousIndex) > 1;
}

export function shouldFollowActiveSegment(
    previousIndex: number,
    nextIndex: number,
    followEnabled: boolean,
): boolean {
    if (nextIndex < 0) return false;
    return isActiveSegmentJump(previousIndex, nextIndex) || followEnabled;
}

/**
 * Custom hook to manage auto-scrolling behavior for transcript
 *
 * Features:
 * - Auto-scrolls to bottom when new content arrives during recording
 * - Pauses auto-scroll when user manually scrolls up
 * - Resumes auto-scroll when user scrolls back to the bottom
 *
 * @param segments - Array of transcript segments
 * @param isRecording - Whether recording is in progress
 * @param isPaused - Whether recording is paused
 * @param activeSegmentId - ID of the currently active segment
 * @returns Scroll ref, auto-scroll state, and scroll control functions
 */
export function useAutoScroll({
    scrollRef,
    segments,
    isRecording,
    isPaused,
    activeSegmentId,
    virtualizer,
    virtualizationThreshold = 10,
    disableAutoScroll = false,
}: UseAutoScrollProps): UseAutoScrollReturn {
    const useVirtualization = virtualizer && segments.length >= virtualizationThreshold;
    const [autoScroll, setAutoScroll] = useState(true);
    // Ref to always have current autoScroll value in effects
    const autoScrollRef = useRef(autoScroll);
    autoScrollRef.current = autoScroll;

    const userScrolledRef = useRef(false);
    const prevSegmentCountRef = useRef(segments.length);
    const followPlaybackRef = useRef(true);
    const lastFollowedIdRef = useRef<string | undefined>(undefined);
    const lastFollowAtRef = useRef(0);

    /**
     * Check if the user is scrolled near the bottom
     */
    const isNearBottom = useCallback(() => {
        if (!scrollRef.current) return true;
        const { scrollTop, scrollHeight, clientHeight } = scrollRef.current;
        return scrollHeight - scrollTop - clientHeight <= SCROLL_THRESHOLD;
    }, [scrollRef]);

    /**
     * Scroll to bottom programmatically
     */
    const scrollToBottom = useCallback(() => {
        if (scrollRef.current) {
            scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
            userScrolledRef.current = false;
            setAutoScroll(true);
        }
    }, [scrollRef]);

    useEffect(() => {
        const container = scrollRef.current;
        if (!container) return;

        let settleTimeout: ReturnType<typeof setTimeout> | null = null;
        const userIntentRef = { current: false };

        const settleUserScroll = () => {
            if (settleTimeout) clearTimeout(settleTimeout);
            settleTimeout = setTimeout(() => {
                userIntentRef.current = false;
                const nearBottom = isNearBottom();
                if (nearBottom) {
                    userScrolledRef.current = false;
                    setAutoScroll(true);
                } else {
                    userScrolledRef.current = true;
                    setAutoScroll(false);
                }
            }, 100);
        };

        const noteUserScrollIntent = () => {
            followPlaybackRef.current = false;
            userIntentRef.current = true;
            settleUserScroll();
        };

        const handleScroll = () => {
            if (!userIntentRef.current) return;
            settleUserScroll();
        };

        const handleKeyDown = (event: KeyboardEvent) => {
            if (isUserScrollKey(event.key)) noteUserScrollIntent();
        };

        const handlePointerDown = (event: PointerEvent) => {
            if (isScrollbarPointer(event, container)) noteUserScrollIntent();
        };

        container.addEventListener("scroll", handleScroll, { passive: true });
        container.addEventListener("wheel", noteUserScrollIntent, { passive: true });
        container.addEventListener("touchmove", noteUserScrollIntent, { passive: true });
        container.addEventListener("keydown", handleKeyDown);
        container.addEventListener("pointerdown", handlePointerDown);

        return () => {
            container.removeEventListener("scroll", handleScroll);
            container.removeEventListener("wheel", noteUserScrollIntent);
            container.removeEventListener("touchmove", noteUserScrollIntent);
            container.removeEventListener("keydown", handleKeyDown);
            container.removeEventListener("pointerdown", handlePointerDown);
            if (settleTimeout) clearTimeout(settleTimeout);
        };
    }, [isNearBottom, scrollRef]);

    // Auto-scroll to bottom when new segments arrive during recording
    useEffect(() => {
        // EARLY RETURN: If auto-scroll is completely disabled (e.g., meeting details page)
        if (disableAutoScroll) {
            return;
        }

        const segmentCount = segments.length;
        const prevCount = prevSegmentCountRef.current;
        const hasNewSegments = segmentCount > prevCount;

        // Update the ref for next comparison
        prevSegmentCountRef.current = segmentCount;

        // Only scroll if new segments arrived AND user is currently at bottom
        // Check isNearBottom() immediately to avoid race conditions with the debounced scroll handler
        if (hasNewSegments && autoScrollRef.current && isRecording && !isPaused && segmentCount > 0) {
            // Check if user is at bottom RIGHT NOW before scrolling
            const isCurrentlyAtBottom = isNearBottom();
            if (!isCurrentlyAtBottom) {
                // User has scrolled up - don't auto-scroll
                return;
            }

            if (useVirtualization && virtualizer) {
                const totalSize = virtualizer.getTotalSize();
                virtualizer.scrollToOffset(totalSize + 1000, { align: "end" });
                setTimeout(() => {
                    if (scrollRef.current) {
                        scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
                    }
                }, 50);
            } else if (scrollRef.current) {
                scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
            }
        }
    }, [segments.length, isRecording, isPaused, useVirtualization, virtualizer, scrollRef, isNearBottom, disableAutoScroll]);

    useEffect(() => {
        if (!activeSegmentId) return;

        const nextIndex = segments.findIndex((s: { id: string }) => s.id === activeSegmentId);
        const prevIndex = lastFollowedIdRef.current
            ? segments.findIndex((s: { id: string }) => s.id === lastFollowedIdRef.current)
            : -1;
        const jumped = isActiveSegmentJump(prevIndex, nextIndex);
        if (jumped) followPlaybackRef.current = true;
        lastFollowedIdRef.current = activeSegmentId;

        if (!shouldFollowActiveSegment(prevIndex, nextIndex, followPlaybackRef.current)) {
            return;
        }

        const segmentInView = () => {
            const root = scrollRef.current;
            if (!root || typeof document === "undefined") return false;
            const element = document.getElementById(`segment-${activeSegmentId}`);
            if (!element) return false;
            const rootRect = root.getBoundingClientRect();
            const rect = element.getBoundingClientRect();
            return rect.top >= rootRect.top + 8 && rect.bottom <= rootRect.bottom - 8;
        };

        const run = () => {
            if (segmentInView()) return;
            if (useVirtualization && virtualizer && nextIndex >= 0) {
                virtualizer.scrollToIndex(nextIndex, {
                    align: "center",
                    behavior: jumped ? "smooth" : "auto",
                });
            } else {
                const element = document.getElementById(`segment-${activeSegmentId}`);
                element?.scrollIntoView({ behavior: jumped ? "smooth" : "auto", block: "center" });
            }
            lastFollowAtRef.current = Date.now();
        };

        const elapsed = Date.now() - lastFollowAtRef.current;
        if (!jumped && elapsed < FOLLOW_THROTTLE_MS) {
            const timeout = window.setTimeout(run, FOLLOW_THROTTLE_MS - elapsed);
            return () => window.clearTimeout(timeout);
        }
        run();
    }, [activeSegmentId, useVirtualization, virtualizer, segments, scrollRef]);

    return {
        autoScroll,
        setAutoScroll,
        scrollToBottom,
    };
}
