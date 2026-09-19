"use client";

import { useState, useEffect, useCallback, useRef, forwardRef, useImperativeHandle } from 'react';
import dynamic from 'next/dynamic';
import { Summary, SummaryDataResponse, SummaryFormat, BlockNoteBlock } from '@/types';
import { AISummary } from './index';
import { Block } from '@blocknote/core';
import { useCreateBlockNote } from '@blocknote/react';
import { BlockNoteView } from '@blocknote/shadcn';
import { blocksToMarkdownSafely } from '@/lib/blocknote-markdown';
import { toast } from 'sonner';
import { storedSummaryMarkdown } from '@/lib/meetingExport';
import "@blocknote/shadcn/style.css";

// Dynamically import BlockNote Editor to avoid SSR issues
const Editor = dynamic(() => import('../BlockNoteEditor/Editor'), { ssr: false });

interface BlockNoteSummaryViewProps {
  summaryData: SummaryDataResponse | Summary | null;
  onSave?: (data: Pick<SummaryDataResponse, 'markdown' | 'summary_json' | 'manually_cleared'>) => Promise<void>;
  onSummaryChange?: (summary: Summary) => void;
  status?: 'idle' | 'processing' | 'summarizing' | 'regenerating' | 'completed' | 'error';
  error?: string | null;
  onRegenerateSummary?: () => void;
  meeting?: {
    id: string;
    title: string;
    created_at: string;
  };
  onDirtyChange?: (isDirty: boolean) => void;
}

export interface BlockNoteSummaryViewRef {
  saveSummary: () => Promise<void>;
  getMarkdown: () => Promise<string>;
  getMarkdownResult?: () => Promise<{ ok: true; markdown: string; empty: boolean } | { ok: false; error: unknown }>;
  getCurrentBlocks?: () => BlockNoteBlock[];
  isDirty: boolean;
}

// Format detection helper
function detectSummaryFormat(data: any): { format: SummaryFormat; data: any } {
  if (!data) {
    return { format: 'legacy', data: null };
  }

  // Priority 1: BlockNote format (has summary_json)
  if (data.summary_json && Array.isArray(data.summary_json)) {
    console.log('✅ FORMAT: BLOCKNOTE (summary_json exists)');
    return { format: 'blocknote', data };
  }

  // Priority 2: Markdown format
  if (data.markdown && typeof data.markdown === 'string') {
    console.log('✅ FORMAT: MARKDOWN (will parse to BlockNote)');
    return { format: 'markdown', data };
  }

  // Priority 3: Legacy JSON
  const hasLegacyStructure = data.MeetingName || Object.keys(data).some(key =>
    typeof data[key] === 'object' && data[key]?.title && data[key]?.blocks
  );

  if (hasLegacyStructure) {
    console.log('✅ FORMAT: LEGACY (custom JSON)');
    return { format: 'legacy', data };
  }

  return { format: 'legacy', data: null };
}

export const BlockNoteSummaryView = forwardRef<BlockNoteSummaryViewRef, BlockNoteSummaryViewProps>(({
  summaryData,
  onSave,
  onSummaryChange,
  status = 'idle',
  error = null,
  onRegenerateSummary,
  meeting,
  onDirtyChange
}, ref) => {
  const { format, data } = detectSummaryFormat(summaryData);
  const [isDirty, setIsDirty] = useState(false);
  const [currentBlocks, setCurrentBlocks] = useState<Block[]>([]);
  const [hasCurrentDocument, setHasCurrentDocument] = useState(false);
  const isContentLoaded = useRef(false);
  const autoSaveTimerRef = useRef<ReturnType<typeof setTimeout>>();
  const structuredDocumentKey = format === 'blocknote' ? JSON.stringify(data?.summary_json ?? []) : '';
  const structuredBlocks = data?.summary_json;

  // Create BlockNote editor for markdown parsing
  const editor = useCreateBlockNote({
    initialContent: undefined
  });

  // Parse markdown to blocks when format is markdown
  useEffect(() => {
    if (format === 'markdown' && data?.markdown && editor) {
      const loadMarkdown = async () => {
        try {
          console.log('📝 Parsing markdown to BlockNote blocks...');
          const blocks = await editor.tryParseMarkdownToBlocks(data.markdown);
          editor.replaceBlocks(editor.document, blocks);
          console.log('✅ Markdown parsed successfully');

          // Delay to ensure editor has finished rendering before allowing onChange
          setTimeout(() => {
            isContentLoaded.current = true;
          }, 100);
        } catch (err) {
          console.error('❌ Failed to parse markdown:', err);
        }
      };
      loadMarkdown();
    }
  }, [format, data?.markdown, editor]);

  // Set content loaded flag for blocknote format
  useEffect(() => {
    if (format === 'blocknote' && structuredBlocks) {
      isContentLoaded.current = false;
      setCurrentBlocks([]);
      setHasCurrentDocument(false);
      setIsDirty(false);
      // Delay to ensure editor has finished rendering
      const timer = setTimeout(() => {
        isContentLoaded.current = true;
      }, 100);
      return () => clearTimeout(timer);
    }
  }, [format, structuredBlocks, structuredDocumentKey]);

  const handleEditorChange = useCallback((blocks: Block[]) => {
    // Only set dirty flag if content has finished loading
    if (isContentLoaded.current) {
      setCurrentBlocks(blocks);
      setHasCurrentDocument(true);
      setIsDirty(true);
    }
  }, []);

  // Notify parent of dirty state changes
  useEffect(() => {
    if (onDirtyChange) {
      onDirtyChange(isDirty);
    }
  }, [isDirty, onDirtyChange]);

  const handleSave = useCallback(async () => {
    if (!onSave || !isDirty) return;

    try {
      console.log('💾 Saving BlockNote content...');

      // Generate markdown from current blocks; preserve BlockNote JSON even if markdown conversion fails.
      const markdownResult = await blocksToMarkdownSafely(editor, currentBlocks, {
        source: 'BlockNoteSummaryView.handleSave',
      });

      const saveData: Pick<SummaryDataResponse, 'markdown' | 'summary_json' | 'manually_cleared'> = {
        summary_json: currentBlocks as unknown as BlockNoteBlock[]
      };

      if (markdownResult.markdown !== undefined) {
        saveData.markdown = markdownResult.markdown;
      }
      if (markdownResult.ok && markdownResult.markdown?.trim().length === 0) {
        saveData.markdown = '';
        saveData.manually_cleared = true;
      }

      await onSave(saveData);
      setIsDirty(false);
      console.log('✅ Save successful');
    } catch (error) {
      console.error('❌ Save failed:', error);
      throw error;
    }
  }, [onSave, isDirty, currentBlocks, editor]);

  // Enhanced notes behave like a normal notes surface: edits are persisted
  // after a short idle period, with no explicit Save action required.
  useEffect(() => {
    if (!isDirty || !onSave) return;
    if (autoSaveTimerRef.current) clearTimeout(autoSaveTimerRef.current);
    autoSaveTimerRef.current = setTimeout(() => {
      void handleSave().catch((error) => {
        toast.error('Could not autosave enhanced notes', { description: String(error) });
      });
    }, 650);

    return () => {
      if (autoSaveTimerRef.current) clearTimeout(autoSaveTimerRef.current);
    };
  }, [currentBlocks, handleSave, isDirty, onSave]);

  // Keep the latest save handler reachable from the unmount cleanup so a pending
  // debounce is flushed when the meeting page goes away instead of being dropped.
  const handleSaveRef = useRef(handleSave);
  const isDirtyRef = useRef(isDirty);
  useEffect(() => {
    handleSaveRef.current = handleSave;
    isDirtyRef.current = isDirty;
  }, [handleSave, isDirty]);

  useEffect(() => () => {
    if (autoSaveTimerRef.current) clearTimeout(autoSaveTimerRef.current);
    if (isDirtyRef.current) {
      void handleSaveRef.current().catch(() => {});
    }
  }, []);

  // Expose methods to parent via ref
  useImperativeHandle(ref, () => ({
    saveSummary: handleSave,
    getMarkdownResult: async () => {
      if (format === 'legacy') {
        const markdown = storedSummaryMarkdown(summaryData as Summary);
        return { ok: true, markdown, empty: markdown.trim().length === 0 };
      }
      const blocks = format === 'markdown'
        ? editor.document
        : hasCurrentDocument
          ? currentBlocks
          : (data?.summary_json as unknown as Block[] | undefined) || [];
      const result = await blocksToMarkdownSafely(editor, blocks, {
        source: 'BlockNoteSummaryView.getMarkdownResult',
      });
      if (!result.ok || result.markdown === undefined) {
        return { ok: false, error: new Error('Could not convert the current enhanced notes to Markdown') };
      }
      return { ok: true, markdown: result.markdown, empty: result.markdown.trim().length === 0 };
    },
    getMarkdown: async () => {
      const blocks = format === 'markdown'
        ? editor.document
        : hasCurrentDocument
          ? currentBlocks
          : (data?.summary_json as unknown as Block[] | undefined) || [];
      if (format === 'legacy') return storedSummaryMarkdown(summaryData as Summary);
      const result = await blocksToMarkdownSafely(editor, blocks, { source: 'BlockNoteSummaryView.getMarkdown' });
      if (!result.ok || result.markdown === undefined) throw new Error('Could not convert the current enhanced notes to Markdown');
      return result.markdown;
    },
    getCurrentBlocks: () => (format === 'markdown' ? editor.document : hasCurrentDocument
      ? currentBlocks
      : (data?.summary_json as unknown as Block[] | undefined) || []) as unknown as BlockNoteBlock[],
    isDirty
  }), [handleSave, isDirty, editor, format, currentBlocks, data, hasCurrentDocument, summaryData]);

  // Render legacy format
  if (format === 'legacy') {
    console.log('🎨 Rendering LEGACY format');
    return (
      <AISummary
        summary={summaryData as Summary}
        status={status}
        error={error}
        onSummaryChange={onSummaryChange || (() => { })}
        onRegenerateSummary={onRegenerateSummary || (() => { })}
        meeting={meeting}
      />
    );
  }

  // Render BlockNote format (has summary_json)
  if (format === 'blocknote') {
    console.log('🎨 Rendering BLOCKNOTE format (direct)');
    return (
      <div className="flex flex-col w-full">
        <div className="w-full">
          <Editor
            key={structuredDocumentKey}
            initialContent={structuredBlocks}
            onChange={(blocks) => {
              console.log('📝 Editor blocks changed:', blocks.length);
              handleEditorChange(blocks);
            }}
            editable={true}
          />
        </div>
      </div>
    );
  }

  // Render Markdown format (parse and display in BlockNote)
  if (format === 'markdown') {
    console.log('🎨 Rendering MARKDOWN format (parsed to BlockNote)');
    return (
      <div className="flex flex-col w-full">
        <div className="w-full">
          <BlockNoteView
            editor={editor}
            editable={true}
            onChange={() => {
              if (isContentLoaded.current) {
                handleEditorChange(editor.document);
              }
            }}
            theme="light"
          />
        </div>
      </div>
    );
  }

  return null;
});

BlockNoteSummaryView.displayName = 'BlockNoteSummaryView';
