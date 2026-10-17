'use client';

import { useEffect, useRef, useState } from 'react';
import type { Block, PartialBlock } from '@blocknote/core';
import { useCreateBlockNote } from '@blocknote/react';
import { BlockNoteView } from '@blocknote/shadcn';
import { closeHistory } from '@tiptap/pm/history';
import { blocksToMarkdownSafely } from '@/lib/blocknote-markdown';
import type { LiveNote, LiveNotesDocument } from '@/lib/liveNotes';
import type { TimelineEntry } from '@/lib/liveTimeline';
import {
  AI_NOTES_PAUSE_MS,
  aiBlocksCss,
  aiNoteText,
  insertPlace,
  pendingGroups,
  remainingAiBlocks,
  type AiBlocks,
} from '@/lib/aiRawNotes';
import '@blocknote/shadcn/style.css';
import '@blocknote/core/fonts/inter.css';

function textFromBlock(block: Block): string {
  const content = block.content as unknown;
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content.map((item) => {
    if (typeof item === 'string') return item;
    if (item && typeof item === 'object' && 'text' in item) return String(item.text ?? '');
    return '';
  }).join('');
}

function initialBlocks(document: LiveNotesDocument): PartialBlock[] {
  if (Array.isArray(document.editorBlocks) && document.editorBlocks.length > 0) {
    return document.editorBlocks as PartialBlock[];
  }
  const notes = document.notes.filter((note) => note.text.trim());
  if (notes.length > 0) {
    return notes.map((note) => ({
      id: note.id,
      type: 'bulletListItem',
      content: note.text,
    }));
  }
  return [{ type: 'bulletListItem', content: '' }];
}

function documentSignature(document: LiveNotesDocument): string {
  return JSON.stringify([document.updatedAt, document.editorBlocks ?? document.notes]);
}

function newBlockId(): string {
  return typeof crypto !== 'undefined' && crypto.randomUUID
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

/** The index of the top-level block holding block `id` (it may be nested). */
function topLevelIndex(blocks: Block[], id: string): number {
  const contains = (block: Block): boolean =>
    block.id === id || block.children.some((child) => contains(child as Block));
  return blocks.findIndex(contains);
}

export function BlockNotesEditor({
  document,
  onChange,
  editable = true,
  aiEntries,
}: {
  document: LiveNotesDocument;
  onChange?: (document: LiveNotesDocument) => void;
  editable?: boolean;
  /** Live notes to write into the notes as AI lines ("Write live notes into
   * my notes"); undefined when that is off. */
  aiEntries?: TimelineEntry[];
}) {
  const startingBlocks = useRef(initialBlocks(document)).current;
  const timestamps = useRef(new Map(document.notes.map((note) => [note.id, note.timestampSeconds])));
  const latestDocument = useRef(document);
  latestDocument.current = document;
  const changeVersion = useRef(0);
  const applyingExternalDocument = useRef(false);
  const appliedSignature = useRef(documentSignature(document));
  const aiBlocks = useRef<AiBlocks>({ ...(document.aiBlocks ?? {}) });
  const aiInserted = useRef(new Set(document.aiEntries ?? []));
  const writingAi = useRef(false);
  const lastUserEditAt = useRef(0);
  const latestAiEntries = useRef(aiEntries);
  latestAiEntries.current = aiEntries;
  const [aiCss, setAiCss] = useState(() => aiBlocksCss(aiBlocks.current));
  const editor = useCreateBlockNote({
    initialContent: startingBlocks,
    placeholders: {
      default: 'Start typing your notes…',
      emptyDocument: 'Start typing your notes…',
    },
  });

  useEffect(() => () => {
    changeVersion.current += 1;
  }, []);

  useEffect(() => {
    const nextSignature = documentSignature(document);
    if (nextSignature === appliedSignature.current) return;
    changeVersion.current += 1;
    applyingExternalDocument.current = true;
    appliedSignature.current = nextSignature;
    timestamps.current = new Map(document.notes.map((note) => [note.id, note.timestampSeconds]));
    aiBlocks.current = { ...(document.aiBlocks ?? {}) };
    aiInserted.current = new Set(document.aiEntries ?? []);
    setAiCss(aiBlocksCss(aiBlocks.current));
    editor.replaceBlocks(editor.document, initialBlocks(document));
    void Promise.resolve().then(() => {
      applyingExternalDocument.current = false;
    });
  }, [document, editor]);

  useEffect(() => {
    if (!onChange || !editable) return;
    return editor.onChange(() => {
      if (applyingExternalDocument.current) return;
      if (!writingAi.current) lastUserEditAt.current = Date.now();
      const version = ++changeVersion.current;
      const blocks = editor.document;
      const nowSeconds = Math.max(0, (Date.now() - latestDocument.current.meetingStartedAtMs) / 1000);
      const previousById = new Map(latestDocument.current.notes.map((note) => [note.id, note]));
      const notes: LiveNote[] = blocks.map((block) => {
        const previous = previousById.get(block.id);
        const timestampSeconds = previous?.timestampSeconds ?? timestamps.current.get(block.id) ?? nowSeconds;
        timestamps.current.set(block.id, timestampSeconds);
        return {
          id: block.id,
          timestampSeconds,
          text: textFromBlock(block),
          important: previous?.important ?? false,
        };
      });
      // An AI line the user deleted or edited is no longer the AI's.
      const before = Object.keys(aiBlocks.current).length;
      aiBlocks.current = remainingAiBlocks(
        aiBlocks.current,
        new Map(notes.map((note) => [note.id, note.text])),
      );
      if (Object.keys(aiBlocks.current).length !== before) setAiCss(aiBlocksCss(aiBlocks.current));
      const hasAi = Object.keys(aiBlocks.current).length > 0 || aiInserted.current.size > 0;
      const draft: LiveNotesDocument = {
        ...latestDocument.current,
        version: 2,
        updatedAt: new Date().toISOString(),
        notes,
        rawMarkdown: undefined,
        editorBlocks: blocks,
        aiBlocks: hasAi ? { ...aiBlocks.current } : undefined,
        aiEntries: hasAi ? [...aiInserted.current] : undefined,
      };
      appliedSignature.current = documentSignature(draft);
      latestDocument.current = draft;
      onChange(draft);

      void blocksToMarkdownSafely(editor, blocks, {
        source: 'BlockNotesEditor',
        fallbackMarkdown: document.rawMarkdown,
      }).then((result) => {
        if (version !== changeVersion.current || !result.ok) return;
        const nextDocument = {
          ...draft,
          updatedAt: new Date().toISOString(),
          rawMarkdown: result.markdown,
        };
        appliedSignature.current = documentSignature(nextDocument);
        latestDocument.current = nextDocument;
        onChange(nextDocument);
      });
    });
  }, [document.rawMarkdown, editable, editor, onChange]);

  // Live notes written into the notes, once the user pauses typing: under
  // their topic below the cursor, else at the end; one undo step of their own.
  const aiOn = aiEntries !== undefined && editable && Boolean(onChange);
  useEffect(() => {
    if (!aiOn) return;
    const timer = window.setInterval(() => {
      if (applyingExternalDocument.current) return;
      if (Date.now() - lastUserEditAt.current < AI_NOTES_PAUSE_MS) return;
      const groups = pendingGroups(latestAiEntries.current ?? [], aiInserted.current);
      if (groups.length === 0) return;
      writingAi.current = true;
      try {
        editor.transact((tr) => closeHistory(tr));
        for (const group of groups) {
          const blocks = editor.document;
          let cursorIndex = -1;
          try {
            cursorIndex = topLevelIndex(blocks, editor.getTextCursorPosition().block.id);
          } catch {
            cursorIndex = -1;
          }
          const place = insertPlace(blocks.map((block) => block.id), cursorIndex, aiBlocks.current, group.topic);
          if (!place) continue;
          const added: PartialBlock[] = [];
          if (place.withHeading) {
            const id = newBlockId();
            aiBlocks.current[id] = { entryId: '', text: group.topic, topic: group.topic };
            added.push({ id, type: 'heading', props: { level: 3 }, content: group.topic });
          }
          for (const entry of group.entries) {
            const id = newBlockId();
            const text = aiNoteText(entry);
            aiBlocks.current[id] = { entryId: entry.id, text, topic: group.topic };
            aiInserted.current.add(entry.id);
            added.push({ id, type: 'bulletListItem', content: text });
          }
          editor.transact(() => {
            editor.insertBlocks(added, place.afterBlockId, 'after');
          });
        }
        editor.transact((tr) => closeHistory(tr));
      } finally {
        writingAi.current = false;
      }
      setAiCss(aiBlocksCss(aiBlocks.current));
    }, 500);
    return () => window.clearInterval(timer);
  }, [aiOn, editor]);

  return (
    <div className="raw-notes-editor min-h-full text-ink">
      {aiCss && <style>{aiCss}</style>}
      <BlockNoteView editor={editor} editable={editable} theme="light" />
    </div>
  );
}
