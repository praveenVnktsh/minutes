import { describe, expect, mock, test } from 'bun:test';
import { createElement } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { Block } from '@blocknote/core';
import type { LiveNotesDocument } from '../../src/lib/liveNotes';
import { meetingNotesTarget, NotePersistenceService } from '../../src/services/notePersistenceService';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

function block(id: string, text: string): Block {
  return {
    id,
    type: 'paragraph',
    props: {},
    content: [{ type: 'text', text, styles: {} }],
    children: [],
  } as unknown as Block;
}

function document(text: string): LiveNotesDocument {
  return {
    version: 2,
    meetingStartedAtMs: Date.now(),
    updatedAt: text,
    notes: [{ id: 'note', timestampSeconds: 0, text, important: false }],
    rawMarkdown: text,
    editorBlocks: [block('note', text)],
  };
}

const listeners = new Set<() => void>();
const editor = {
  document: [block('note', 'initial')],
  onChange(listener: () => void) {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  replaceBlocks(_current: Block[], replacement: Block[]) {
    this.document = replacement;
    listeners.forEach((listener) => listener());
  },
};
const conversions: Array<ReturnType<typeof deferred<{ markdown?: string; ok: boolean }>>> = [];

mock.module('@blocknote/react', () => ({ useCreateBlockNote: () => editor }));
mock.module('@blocknote/shadcn', () => ({ BlockNoteView: () => createElement('div', { 'data-blocknote': true }) }));
mock.module('../../src/lib/blocknote-markdown', () => ({
  blocksToMarkdownSafely: () => {
    const conversion = deferred<{ markdown?: string; ok: boolean }>();
    conversions.push(conversion);
    return conversion.promise;
  },
}));

const { BlockNotesEditor } = await import('../../src/components/BlockNotesEditor');

function resetEditor(text = 'initial') {
  listeners.clear();
  conversions.length = 0;
  editor.document = [block('note', text)];
}

describe('BlockNotesEditor persistence boundary', () => {
  test('publishes complete current blocks before deferred Markdown conversion', async () => {
    resetEditor();
    const writes: LiveNotesDocument[] = [];
    const service = new NotePersistenceService({
      debounceMs: 60_000,
      storage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
      save: async (_target, value) => { writes.push(value); },
    });
    const target = meetingNotesTarget('editor-flush');
    let renderer!: ReactTestRenderer;
    act(() => {
      renderer = create(createElement(BlockNotesEditor, {
        document: document('initial'),
        onChange: (value: LiveNotesDocument) => service.saveNotes(target, value),
      }));
    });

    act(() => {
      editor.document = [block('note', 'current keystroke')];
      listeners.forEach((listener) => listener());
    });
    expect(conversions).toHaveLength(1);

    await service.flushNotes(target);
    expect(writes).toHaveLength(1);
    expect(writes[0].notes[0].text).toBe('current keystroke');
    expect((writes[0].editorBlocks as Block[])[0].id).toBe('note');
    expect(writes[0].rawMarkdown).toBeUndefined();
    act(() => renderer.unmount());
    conversions[0].resolve({ markdown: 'late conversion', ok: true });
    await conversions[0].promise;
    expect(service.getSnapshot(target).revision).toBe(1);
  });

  test('invalidates a deferred conversion when an external document replaces the editor', async () => {
    resetEditor();
    const changes: LiveNotesDocument[] = [];
    let renderer!: ReactTestRenderer;
    act(() => {
      renderer = create(createElement(BlockNotesEditor, {
        document: document('initial'),
        onChange: (value: LiveNotesDocument) => changes.push(value),
      }));
    });

    act(() => {
      editor.document = [block('note', 'local edit')];
      listeners.forEach((listener) => listener());
    });
    expect(changes.map((value) => value.notes[0].text)).toEqual(['local edit']);

    act(() => {
      renderer.update(createElement(BlockNotesEditor, {
        document: document('external replacement'),
        onChange: (value: LiveNotesDocument) => changes.push(value),
      }));
    });
    expect(editor.document[0].content).toEqual(block('note', 'external replacement').content);

    await act(async () => {
      conversions[0].resolve({ markdown: 'stale local edit', ok: true });
      await conversions[0].promise;
    });
    expect(changes.map((value) => value.notes[0].text)).toEqual(['local edit']);
    act(() => renderer.unmount());
  });
});
