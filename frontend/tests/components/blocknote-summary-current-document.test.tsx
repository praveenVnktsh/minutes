import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import { createElement, createRef } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { BlockNoteSummaryViewRef } from '../../src/components/AISummary/BlockNoteSummaryView';

const originalDynamic = { ...await import('next/dynamic') };
const originalBlockNoteReact = { ...await import('@blocknote/react') };
const originalBlockNoteShadcn = { ...await import('@blocknote/shadcn') };
let onEditorChange: ((blocks: unknown[]) => void) | undefined;
let conversionFails = false;

const editor = {
  document: [] as unknown[],
  replaceBlocks: () => {},
  tryParseMarkdownToBlocks: async () => [],
  blocksToMarkdownLossy: async (blocks: unknown[]) => {
    if (conversionFails) throw new Error('conversion failed');
    return blocks.length === 0 ? '' : 'Current content';
  },
};

mock.module('next/dynamic', () => ({
  default: () => function Editor(props: { onChange: (blocks: unknown[]) => void }) {
    onEditorChange = props.onChange;
    return null;
  },
}));
mock.module('@blocknote/react', () => ({ useCreateBlockNote: () => editor }));
mock.module('@blocknote/shadcn', () => ({ BlockNoteView: () => null }));

const { BlockNoteSummaryView } = await import('../../src/components/AISummary/BlockNoteSummaryView');

afterAll(() => {
  mock.module('next/dynamic', () => originalDynamic);
  mock.module('@blocknote/react', () => originalBlockNoteReact);
  mock.module('@blocknote/shadcn', () => originalBlockNoteShadcn);
});

beforeEach(() => {
  conversionFails = false;
  onEditorChange = undefined;
});

describe('BlockNoteSummaryView current document contract', () => {
  test('reports an edited empty document and propagates conversion failure without stale fallback', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(createElement(BlockNoteSummaryView, {
        ref,
        summaryData: {
          markdown: 'Stale markdown',
          summary_json: [{ id: 'old', type: 'paragraph', content: [{ type: 'text', text: 'Old', styles: {} }] }],
        },
        onSave: async () => {},
      }));
      await new Promise((resolve) => setTimeout(resolve, 110));
    });

    await act(async () => onEditorChange?.([]));
    expect(ref.current?.isDirty).toBe(true);
    expect(await ref.current?.getMarkdownResult?.()).toEqual({ ok: true, markdown: '', empty: true });
    expect(ref.current?.getCurrentBlocks?.()).toEqual([]);

    conversionFails = true;
    const failed = await ref.current?.getMarkdownResult?.();
    expect(failed?.ok).toBe(false);
    if (failed && !failed.ok) expect(String(failed.error)).toContain('Could not convert');

    await act(async () => renderer.unmount());
  });
});
