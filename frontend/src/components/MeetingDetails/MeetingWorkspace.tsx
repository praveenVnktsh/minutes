'use client';

import { ReactNode, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  FileText,
  ListTree,
  Loader2,
  MessageCircle,
  RotateCw,
  Sparkles,
} from 'lucide-react';
import { useShell, SIDEBAR_WIDTH, SIDEBAR_COLLAPSED_WIDTH } from '@/contexts/ShellContext';
import { ResizeSeparator } from '@/components/ui/resize-separator';
import { SegmentedControl } from '@/components/ui/segmented-control';

export type NotesMode = 'enhanced' | 'raw';

const DOCK_RATIO_KEY = 'meetily:workspace-dock-ratio';
const DOCK_WIDTH_KEY = 'meetily:workspace-dock-width';
const DEFAULT_DOCK_WIDTH = 520;

/** Keep the notes column readable: never let the dock take so much that the
 *  notes fall below MIN_NOTES_WIDTH. */
const MIN_NOTES_WIDTH = 520;

/** Below this workspace width the notes and the dock no longer fit side by
 *  side, so the dock becomes an overlay and panels open one at a time. */
const OVERLAY_WORKSPACE_WIDTH = 760;

function clampDockWidth(value: number, viewportWidth: number, sidebarWidth: number): number {
  const max = Math.max(360, viewportWidth - sidebarWidth - MIN_NOTES_WIDTH);
  return Math.min(max, Math.max(320, value));
}

function formatDateSubtitle(createdAt: string): string {
  const date = new Date(createdAt);
  if (Number.isNaN(date.getTime())) return '';
  const now = new Date();
  const time = date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
  if (date.toDateString() === now.toDateString()) return `Today, ${time}`;
  return date.toLocaleDateString([], { month: 'short', day: 'numeric', year: 'numeric' });
}

export function MeetingWorkspace({
  title,
  createdAt,
  statusBanner,
  notesMode,
  onNotesModeChange,
  canShowEnhanced,
  hasEnhancedContent = canShowEnhanced,
  summary,
  rawNotes,
  transcript,
  assistant,
  showAssistant,
  peopleCount,
  toolbarActions,
  onTitleChange,
  onRegenerate,
  onStopGeneration,
  isGenerating = false,
  notesDirty = false,
  titlePending = false,
  titleError,
}: {
  title: string;
  createdAt: string;
  statusBanner?: ReactNode;
  notesMode: NotesMode;
  onNotesModeChange: (mode: NotesMode) => void;
  canShowEnhanced: boolean;
  hasEnhancedContent?: boolean;
  summary: ReactNode;
  rawNotes: ReactNode;
  transcript: ReactNode;
  assistant: ReactNode;
  showAssistant: boolean;
  peopleCount: number;
  toolbarActions?: ReactNode;
  onTitleChange?: (title: string) => void | Promise<void>;
  onRegenerate?: () => void;
  onStopGeneration?: () => void;
  isGenerating?: boolean;
  notesDirty?: boolean;
  titlePending?: boolean;
  titleError?: string | null;
}) {
  const { collapsed } = useShell();
  const sidebarWidth = collapsed ? SIDEBAR_COLLAPSED_WIDTH : SIDEBAR_WIDTH;
  const [transcriptOpen, setTranscriptOpen] = useState(true);
  const [chatOpen, setChatOpen] = useState(false);
  const [ratio, setRatio] = useState(62);
  const [dockWidth, setDockWidth] = useState(DEFAULT_DOCK_WIDTH);
  const [titleDraft, setTitleDraft] = useState(title);
  const [localTitleError, setLocalTitleError] = useState<string | null>(null);
  const skipBlurCommitRef = useRef(false);
  const workspaceRef = useRef<HTMLDivElement>(null);
  const [workspaceWidth, setWorkspaceWidth] = useState(0);
  const dockRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setTitleDraft(title);
    setLocalTitleError(null);
  }, [title]);

  const commitTitle = async () => {
    if (skipBlurCommitRef.current) {
      skipBlurCommitRef.current = false;
      return;
    }
    const next = titleDraft.trim();
    if (!next) {
      setTitleDraft(title);
      return;
    }
    if (next !== title) {
      setLocalTitleError(null);
      try {
        await onTitleChange?.(next);
      } catch (error) {
        setLocalTitleError(error instanceof Error ? error.message : String(error));
      }
    }
  };

  useEffect(() => {
    const node = workspaceRef.current;
    if (!node) return;
    if (typeof ResizeObserver === 'undefined') {
      setWorkspaceWidth(node.getBoundingClientRect().width || window.innerWidth - sidebarWidth);
      return;
    }
    const observer = new ResizeObserver(([entry]) => setWorkspaceWidth(entry.contentRect.width));
    observer.observe(node);
    setWorkspaceWidth(node.getBoundingClientRect().width);
    return () => observer.disconnect();
  }, [sidebarWidth]);

  const activePointerCleanupRef = useRef<(() => void) | null>(null);
  useEffect(() => () => activePointerCleanupRef.current?.(), []);

  useEffect(() => {
    const stored = localStorage.getItem(DOCK_RATIO_KEY);
    if (stored) {
      const parsed = Number(stored);
      if (!Number.isNaN(parsed)) setRatio(Math.min(80, Math.max(20, parsed)));
    }
  }, []);

  // Load the persisted dock width once.
  useEffect(() => {
    const stored = localStorage.getItem(DOCK_WIDTH_KEY);
    if (stored) {
      const parsed = Number(stored);
      if (!Number.isNaN(parsed)) setDockWidth(parsed);
    }
  }, []);

  // Keep the dock within bounds as the sidebar/compact state or window changes,
  // so the notes never get squeezed.
  useEffect(() => {
    const reclamp = () =>
      setDockWidth((current) => clampDockWidth(current, window.innerWidth, sidebarWidth));
    reclamp();
    window.addEventListener('resize', reclamp);
    return () => window.removeEventListener('resize', reclamp);
  }, [sidebarWidth]);

  const onDividerPointerDown = useCallback((event: React.PointerEvent) => {
    event.preventDefault();
    activePointerCleanupRef.current?.();
    const rect = dockRef.current?.getBoundingClientRect();
    if (!rect) return;
    const startY = event.clientY;
    const startRatio = ratio;

    const move = (moveEvent: PointerEvent) => {
      const deltaPct = ((moveEvent.clientY - startY) / rect.height) * 100;
      const next = Math.min(80, Math.max(20, startRatio + deltaPct));
      setRatio(next);
    };
    const cleanup = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      activePointerCleanupRef.current = null;
    };
    const up = () => {
      cleanup();
      setRatio((current) => {
        localStorage.setItem(DOCK_RATIO_KEY, String(current));
        return current;
      });
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
    activePointerCleanupRef.current = cleanup;
  }, [ratio]);

  const onColumnDividerPointerDown = useCallback((event: React.PointerEvent) => {
    event.preventDefault();
    activePointerCleanupRef.current?.();
    const startX = event.clientX;
    const startWidth = dockWidth;

    const move = (moveEvent: PointerEvent) => {
      const delta = moveEvent.clientX - startX;
      setDockWidth(clampDockWidth(startWidth - delta, window.innerWidth, sidebarWidth));
    };
    const cleanup = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      activePointerCleanupRef.current = null;
    };
    const up = () => {
      cleanup();
      setDockWidth((current) => {
        localStorage.setItem(DOCK_WIDTH_KEY, String(current));
        return current;
      });
    };

    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
    activePointerCleanupRef.current = cleanup;
  }, [dockWidth, sidebarWidth]);

  const dateSubtitle = useMemo(() => formatDateSubtitle(createdAt), [createdAt]);
  // Only a genuinely tight workspace overlays the dock; a non-maximized
  // (compact) window still keeps the transcript as a side rail.
  const narrow = workspaceWidth > 0 && workspaceWidth < OVERLAY_WORKSPACE_WIDTH;
  const dockVisible = transcriptOpen || (chatOpen && showAssistant);

  // The overlay layout defaults to everything closed; the user opens a panel on
  // demand. Entering it closes both, leaving it restores the transcript.
  const prevNarrowRef = useRef(narrow);
  useEffect(() => {
    if (narrow && !prevNarrowRef.current) {
      setTranscriptOpen(false);
      setChatOpen(false);
    } else if (!narrow && prevNarrowRef.current) {
      setTranscriptOpen(true);
      setChatOpen(false);
    }
    prevNarrowRef.current = narrow;
  }, [narrow]);

  // In the overlay layout there is only room for one panel, so transcript and
  // chat become mutually exclusive.
  useEffect(() => {
    if (narrow && transcriptOpen && chatOpen) setChatOpen(false);
  }, [narrow, transcriptOpen, chatOpen]);

  const toggleTranscript = () => {
    setTranscriptOpen((open) => {
      const next = !open;
      if (narrow && next) setChatOpen(false);
      return next;
    });
  };

  const toggleChat = () => {
    if (!showAssistant) return;
    setChatOpen((open) => {
      const next = !open;
      if (narrow && next) setTranscriptOpen(false);
      return next;
    });
  };

  return (
    <div ref={workspaceRef} className="relative flex min-h-0 flex-1 flex-col overflow-hidden bg-surface-0 text-ink">
      {/* Header */}
      <div className="flex min-h-[76px] shrink-0 items-center justify-between gap-6 px-8 py-3">
        <div className="min-w-0 flex-1">
          <input
            value={titleDraft}
            onChange={(event) => setTitleDraft(event.target.value)}
            onBlur={() => void commitTitle()}
            disabled={titlePending}
            onKeyDown={(event) => {
              if (event.key === 'Enter') {
                event.preventDefault();
                (event.target as HTMLInputElement).blur();
              } else if (event.key === 'Escape') {
                skipBlurCommitRef.current = true;
                setTitleDraft(title);
                (event.target as HTMLInputElement).blur();
              }
            }}
            spellCheck={false}
            aria-label="Meeting title"
            placeholder="Untitled meeting"
            className="w-full truncate rounded bg-transparent font-serif text-[26px] font-semibold tracking-[-0.02em] text-ink outline-none placeholder:text-ink-subtle focus-visible:ring-2 focus-visible:ring-focus"
          />
          {(dateSubtitle || peopleCount > 0) && (
            <p className="mt-0.5 text-xs text-ink-subtle">
              {[dateSubtitle, peopleCount > 0 ? `${peopleCount} ${peopleCount === 1 ? 'person' : 'people'}` : ''].filter(Boolean).join(' · ')}
            </p>
          )}
          {(localTitleError || titleError) && <p role="alert" className="mt-1 text-xs text-error">Could not rename: {localTitleError || titleError}</p>}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {statusBanner}
          <button
            type="button"
            onClick={toggleTranscript}
            title="Toggle transcript"
            aria-pressed={transcriptOpen}
            className={`flex h-8 items-center gap-1.5 rounded-full px-3 text-xs transition-colors ${transcriptOpen ? 'bg-surface-2 text-ink' : 'text-ink-subtle hover:bg-surface-2 hover:text-ink'}`}
          >
            <FileText className="h-3.5 w-3.5" /> Transcript
          </button>
          <button
            type="button"
            onClick={toggleChat}
            disabled={!showAssistant}
            title="Toggle chat"
            aria-pressed={chatOpen && showAssistant}
            className={`flex h-8 items-center gap-1.5 rounded-full px-3 text-xs transition-colors ${chatOpen && showAssistant ? 'bg-surface-2 text-ink' : 'text-ink-subtle hover:bg-surface-2 hover:text-ink'} disabled:opacity-40`}
          >
            <MessageCircle className="h-3.5 w-3.5" /> Chat
          </button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-row overflow-hidden">
        {/* Center: notes document */}
        <section className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
          {/* Toolbar */}
          <div className="flex shrink-0 flex-wrap items-center gap-1.5 px-8 pb-3">
            {/* Enhanced / Raw segmented toggle */}
            <div className="flex h-8 shrink-0 items-center rounded-full bg-surface-2 p-0.5">
              <SegmentedControl
                value={notesMode}
                aria-label="Notes view"
                className="h-8 rounded-full border-0 bg-transparent p-0"
                options={[
                  { value: 'raw', label: <span className="flex items-center gap-1.5"><ListTree className="h-3.5 w-3.5" /> Raw</span> },
                  { value: 'enhanced', label: <span className="flex items-center gap-1.5"><Sparkles className="h-3.5 w-3.5" /> Enhanced</span> },
                ]}
                onValueChange={onNotesModeChange}
              />
              <div className="flex h-7 items-center rounded-full pr-1 text-ink-muted">
                {hasEnhancedContent && (
                  <button
                    type="button"
                    onClick={() => (isGenerating ? onStopGeneration?.() : onRegenerate?.())}
                    title={isGenerating ? 'Stop generating' : notesDirty ? 'Your notes changed — re-enhance to include them' : 'Re-enhance notes'}
                    aria-label={isGenerating ? 'Stop generating' : notesDirty ? 'Your notes changed — re-enhance' : 'Re-enhance notes'}
                    className={`flex h-6 w-6 items-center justify-center rounded-full text-ink-muted hover:bg-surface-2 hover:text-ink ${notesDirty && !isGenerating ? 'animate-pulse text-warning ring-2 ring-warning/70' : ''}`}
                  >
                    {isGenerating ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <RotateCw className="h-3.5 w-3.5" />}
                  </button>
                )}
              </div>
            </div>

            {toolbarActions && (
              <div className="ml-1 flex shrink-0 items-center gap-1.5">{toolbarActions}</div>
            )}
          </div>

          {/* Document */}
          <div className="min-h-0 flex-1 overflow-hidden">
            {notesMode === 'enhanced' && canShowEnhanced ? summary : rawNotes}
          </div>
        </section>

        {/* Drag handle between the notes column and the side dock */}
        {dockVisible && !narrow && (
          <ResizeSeparator
            value={dockWidth}
            min={320}
            max={Math.max(320, workspaceWidth - MIN_NOTES_WIDTH)}
            onValueChange={setDockWidth}
            label="Resize transcript and chat panel"
            onPointerDown={onColumnDividerPointerDown}
            className="z-10 w-2 bg-transparent"
          />
        )}

        {/* Transcript / chat dock: always a right-side rail; overlay when the workspace is too tight */}
        <section
          aria-hidden={!dockVisible}
          className={`${dockVisible ? 'flex' : 'hidden'} min-h-0 min-w-0 shrink-0 flex-col overflow-hidden border-l border-hairline bg-surface-1 ${narrow ? 'absolute inset-x-0 bottom-0 top-[76px] z-20' : ''}`}
          style={narrow ? undefined : { width: dockWidth }}
        >
            <div ref={dockRef} className="flex min-h-0 flex-1 flex-col">
              {transcriptOpen && (
                <div className="min-h-0 flex-1 overflow-hidden" style={chatOpen && showAssistant ? { flexBasis: `${ratio}%`, flexGrow: 0 } : undefined}>
                  {transcript}
                </div>
              )}
              {transcriptOpen && chatOpen && showAssistant && (
                <ResizeSeparator
                  value={ratio}
                  min={20}
                  max={80}
                  onValueChange={setRatio}
                  orientation="horizontal"
                  label="Resize transcript and chat sections"
                  onPointerDown={onDividerPointerDown}
                  className="h-3 bg-transparent"
                />
              )}
              {showAssistant && (
                <div className={`${chatOpen ? 'block' : 'hidden'} min-h-0 flex-1 overflow-hidden`}>
                  {assistant}
                </div>
              )}
            </div>
        </section>
      </div>
    </div>
  );
}
