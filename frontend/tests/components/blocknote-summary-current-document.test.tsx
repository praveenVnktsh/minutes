import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import { createElement, createRef, useEffect } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { BlockNoteSummaryViewRef } from '../../src/components/AISummary/BlockNoteSummaryView';

const originalDynamic = { ...await import('next/dynamic') };
const originalBlockNoteReact = { ...await import('@blocknote/react') };
const originalBlockNoteShadcn = { ...await import('@blocknote/shadcn') };
const originalAISummary = { ...await import('../../src/components/AISummary') };
const originalCore = { ...await import('@tauri-apps/api/core') };
const invoke = mock(async (_command: string, _args?: Record<string, unknown>) => ({ message: 'saved' }));
let onEditorChange: ((blocks: unknown[]) => void) | undefined;
let conversionFails = false;
const editorInitialContents: unknown[] = [];

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
  default: () => function Editor(props: { onChange: (blocks: unknown[]) => void; initialContent: unknown }) {
    onEditorChange = props.onChange;
    useEffect(() => { editorInitialContents.push(props.initialContent); }, []);
    return null;
  },
}));
mock.module('@blocknote/react', () => ({ useCreateBlockNote: () => editor }));
mock.module('@blocknote/shadcn', () => ({ BlockNoteView: () => null }));
mock.module('../../src/components/AISummary', () => ({ AISummary: () => null }));
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));

const { BlockNoteSummaryView } = await import('../../src/components/AISummary/BlockNoteSummaryView');
const { useMeetingData } = await import('../../src/hooks/meeting-details/useMeetingData');
const { useCopyOperations } = await import('../../src/hooks/meeting-details/useCopyOperations');

afterAll(() => {
  mock.module('next/dynamic', () => originalDynamic);
  mock.module('@blocknote/react', () => originalBlockNoteReact);
  mock.module('@blocknote/shadcn', () => originalBlockNoteShadcn);
  mock.module('../../src/components/AISummary', () => originalAISummary);
  mock.module('@tauri-apps/api/core', () => originalCore);
});

beforeEach(() => {
  conversionFails = false;
  onEditorChange = undefined;
  editorInitialContents.length = 0;
  invoke.mockClear();
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

  test('returns mounted legacy markdown instead of reporting a successful empty document', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(createElement(BlockNoteSummaryView, {
        ref,
        summaryData: {
          decisions: {
            title: 'Decisions',
            blocks: [{ id: 'legacy', type: 'bullet', content: 'Launch Friday', color: 'default' }],
          },
        },
      }));
    });
    expect(await ref.current?.getMarkdownResult?.()).toEqual({
      ok: true,
      markdown: '## Decisions\n\n- Launch Friday',
      empty: false,
    });
    await act(async () => renderer.unmount());
  });

  test('copies legacy markdown through the mounted real ref and copy hook', async () => {
    const writeText = mock(async (_text: string) => {});
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { clipboard: { writeText } },
    });
    let copySummary!: () => Promise<void>;
    const legacySummary = {
      decisions: {
        title: 'Decisions',
        blocks: [{ id: 'legacy', type: 'bullet', content: 'Launch Friday', color: 'default' }],
      },
    };
    function CopyOwner() {
      const ref = createRef<BlockNoteSummaryViewRef>();
      copySummary = useCopyOperations({
        meeting: { id: 'meeting-1', title: 'Planning' }, transcripts: [], meetingTitle: 'Planning',
        aiSummary: legacySummary, blockNoteSummaryRef: ref,
      }).handleCopySummary;
      return <BlockNoteSummaryView ref={ref} summaryData={legacySummary} />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<CopyOwner />); });
    await act(async () => copySummary());
    expect(writeText.mock.calls[0][0]).toContain('## Decisions\n\n- Launch Friday');
    await act(async () => renderer.unmount());
  });

  test('does not fall back when conversion fails through the mounted real ref and copy hook', async () => {
    const writeText = mock(async (_text: string) => {});
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { clipboard: { writeText } },
    });
    const structured = {
      summary_json: [{ id: 'one', type: 'paragraph', content: [{ type: 'text', text: 'Known content', styles: {} }] }],
    };
    let copySummary!: () => Promise<void>;
    function CopyOwner() {
      const ref = createRef<BlockNoteSummaryViewRef>();
      copySummary = useCopyOperations({
        meeting: { id: 'meeting-1', title: 'Planning' }, transcripts: [], meetingTitle: 'Planning',
        aiSummary: structured, blockNoteSummaryRef: ref,
      }).handleCopySummary;
      return <BlockNoteSummaryView ref={ref} summaryData={structured} />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<CopyOwner />);
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    conversionFails = true;
    await act(async () => copySummary());
    expect(writeText).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  test('remounts the editor boundary when an acknowledged structured document is replaced', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    let renderer!: ReactTestRenderer;
    const first = [{ id: 'first', type: 'paragraph', content: [{ type: 'text', text: 'First', styles: {} }] }];
    const second = [{ id: 'second', type: 'paragraph', content: [{ type: 'text', text: 'Second', styles: {} }] }];
    await act(async () => {
      renderer = create(createElement(BlockNoteSummaryView, { ref, summaryData: { summary_json: first } }));
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    await act(async () => {
      renderer.update(createElement(BlockNoteSummaryView, { ref, summaryData: { summary_json: second } }));
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    expect(editorInitialContents).toEqual([first, second]);
    expect(ref.current?.getCurrentBlocks?.()).toEqual(second);
    await act(async () => renderer.unmount());
  });

  test('persists and reopens a deliberately cleared document through the actual save owner', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    const meeting = { id: 'meeting-1', title: 'Planning', created_at: '2026-09-19', transcripts: [] };
    function Owner({ summaryData }: { summaryData: { markdown: string; summary_json: any[]; manually_cleared?: boolean } }) {
      const data = useMeetingData({ meeting, summaryData });
      return <BlockNoteSummaryView ref={ref} summaryData={data.aiSummary} onSave={data.handleSaveSummary} />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<Owner summaryData={{ markdown: 'Existing', summary_json: [{ id: 'old', type: 'paragraph', content: [{ type: 'text', text: 'Old' }] }] }} />);
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    await act(async () => onEditorChange?.([]));
    await act(async () => ref.current?.saveSummary());
    expect(invoke.mock.calls.find(([command]) => command === 'api_save_meeting_summary')?.[1]?.summary).toEqual({
      markdown: '', summary_json: [], manually_cleared: true,
    });

    await act(async () => {
      renderer.update(<Owner summaryData={{ markdown: '', summary_json: [], manually_cleared: true }} />);
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    expect(await ref.current?.getMarkdownResult?.()).toEqual({ ok: true, markdown: '', empty: true });
    await act(async () => renderer.unmount());
  });

  test('autosaves the explicit cleared representation through the actual save owner', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    const meeting = { id: 'meeting-autosave', title: 'Planning', created_at: '2026-09-19', transcripts: [] };
    function Owner() {
      const data = useMeetingData({
        meeting,
        summaryData: { markdown: 'Existing', summary_json: [{ id: 'old', type: 'paragraph', content: [{ type: 'text', text: 'Old' }] }] },
      });
      return <BlockNoteSummaryView ref={ref} summaryData={data.aiSummary} onSave={data.handleSaveSummary} />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<Owner />);
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    await act(async () => {
      onEditorChange?.([]);
      await new Promise((resolve) => setTimeout(resolve, 700));
    });
    expect(invoke.mock.calls.find(([command]) => command === 'api_save_meeting_summary')?.[1]?.summary).toEqual({
      markdown: '', summary_json: [], manually_cleared: true,
    });
    await act(async () => renderer.unmount());
  });
});
