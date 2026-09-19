import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import { createElement, createRef, useCallback, useEffect, useState } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { BlockNoteSummaryViewRef } from '../../src/components/AISummary/BlockNoteSummaryView';

const originalDynamic = { ...await import('next/dynamic') };
const originalBlockNoteReact = { ...await import('@blocknote/react') };
const originalBlockNoteShadcn = { ...await import('@blocknote/shadcn') };
const originalAISummary = { ...await import('../../src/components/AISummary') };
const originalCore = { ...await import('@tauri-apps/api/core') };
const originalShell = { ...await import('../../src/contexts/ShellContext') };
let saveHandler: () => Promise<unknown> = async () => ({ message: 'saved' });
const invoke = mock(async (command: string, _args?: Record<string, unknown>) => {
  if (command === 'api_save_meeting_summary') return saveHandler();
  if (command === 'api_get_meeting_transcripts') return { transcripts: [], total_count: 0, has_more: false };
  return '/tmp/export.md';
});
let onEditorChange: ((blocks: unknown[]) => void) | undefined;
let onMarkdownEditorChange: ((blocks: unknown[]) => void) | undefined;
let conversionFails = false;
let parseMarkdown: (markdown: string) => Promise<unknown[]> = async () => [];
let convertBlocks: (blocks: any[]) => Promise<string> = async (blocks) => (
  blocks.flatMap((block) => block.content ?? []).map((item: any) => item.text ?? '').join('')
);
const editorInitialContents: unknown[] = [];
let updateMarkdownView: ((blocks: unknown[]) => void) | undefined;

const editor = {
  document: [] as unknown[],
  replaceBlocks: (_previous: unknown[], blocks: unknown[]) => {
    editor.document = blocks;
    updateMarkdownView?.(blocks);
  },
  tryParseMarkdownToBlocks: (markdown: string) => parseMarkdown(markdown),
  blocksToMarkdownLossy: async (blocks: any[]) => {
    if (conversionFails) throw new Error('conversion failed');
    return convertBlocks(blocks);
  },
};

mock.module('next/dynamic', () => ({
  default: () => function Editor(props: { onChange: (blocks: unknown[]) => void; initialContent: unknown }) {
    const [document, setDocument] = useState(props.initialContent);
    onEditorChange = (blocks) => {
      setDocument(blocks);
      props.onChange(blocks);
    };
    useEffect(() => { editorInitialContents.push(props.initialContent); }, []);
    return <output data-editor="structured">{JSON.stringify(document)}</output>;
  },
}));
mock.module('@blocknote/react', () => ({ useCreateBlockNote: () => editor }));
mock.module('@blocknote/shadcn', () => ({
  BlockNoteView: (props: { onChange?: () => void }) => {
    const [document, setDocument] = useState(editor.document);
    useEffect(() => {
      updateMarkdownView = setDocument;
      onMarkdownEditorChange = (blocks) => {
        editor.document = blocks;
        setDocument(blocks);
        props.onChange?.();
      };
      return () => {
        updateMarkdownView = undefined;
        onMarkdownEditorChange = undefined;
      };
    }, [props.onChange]);
    return <output data-editor="markdown">{JSON.stringify(document)}</output>;
  },
}));
mock.module('../../src/components/AISummary', () => ({ AISummary: () => null }));
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('../../src/contexts/ShellContext', () => ({
  ...originalShell,
  useShell: () => ({ compact: true, collapsed: false }),
}));

const { BlockNoteSummaryView } = await import('../../src/components/AISummary/BlockNoteSummaryView');
const { useMeetingData } = await import('../../src/hooks/meeting-details/useMeetingData');
const { useCopyOperations } = await import('../../src/hooks/meeting-details/useCopyOperations');
const { MeetingWorkspace } = await import('../../src/components/MeetingDetails/MeetingWorkspace');
const { BlockNoteEditor } = await import('@blocknote/core');

afterAll(() => {
  mock.module('next/dynamic', () => originalDynamic);
  mock.module('@blocknote/react', () => originalBlockNoteReact);
  mock.module('@blocknote/shadcn', () => originalBlockNoteShadcn);
  mock.module('../../src/components/AISummary', () => originalAISummary);
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('../../src/contexts/ShellContext', () => originalShell);
});

beforeEach(() => {
  conversionFails = false;
  parseMarkdown = async () => [];
  convertBlocks = async (blocks) => blocks
    .flatMap((block) => block.content ?? [])
    .map((item: any) => item.text ?? '')
    .join('');
  onEditorChange = undefined;
  onMarkdownEditorChange = undefined;
  updateMarkdownView = undefined;
  editor.document = [];
  editorInitialContents.length = 0;
  invoke.mockClear();
  saveHandler = async () => ({ message: 'saved' });
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: { getItem: () => null, setItem: () => {} },
  });
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: Object.assign(new EventTarget(), { innerWidth: 1200 }),
  });
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

  test('invalidates a dirty structured cleanup save before deferred Markdown parsing completes', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    const save = mock(async () => {});
    let resolveParse!: (blocks: unknown[]) => void;
    parseMarkdown = () => new Promise((resolve) => { resolveParse = resolve; });
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<BlockNoteSummaryView
        ref={ref}
        summaryData={{ summary_json: [{ id: 'a', type: 'paragraph', content: [{ type: 'text', text: 'A', styles: {} }] }] }}
        onSave={save}
      />);
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    await act(async () => onEditorChange?.([{ id: 'dirty-a', type: 'paragraph', content: [{ type: 'text', text: 'A dirty', styles: {} }] }]));
    await act(async () => renderer.update(<BlockNoteSummaryView ref={ref} summaryData={{ markdown: 'Acknowledged B' }} onSave={save} />));
    expect(await ref.current?.getMarkdownResult?.()).toEqual({ ok: true, markdown: 'Acknowledged B', empty: false });
    await act(async () => renderer.unmount());
    resolveParse([]);
    await Promise.resolve();
    expect(save).not.toHaveBeenCalled();
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
    const initialContent = editorInitialContents.at(-1) as any[];
    expect(initialContent).toEqual([{ type: 'paragraph', content: [] }]);
    expect(() => BlockNoteEditor.create({ initialContent, _headless: true })).not.toThrow();
    await act(async () => renderer.unmount());
  });

  test('keeps a newer dirty revision while its older save is acknowledged', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    const saveResolvers: Array<(value: unknown) => void> = [];
    saveHandler = () => new Promise((resolve) => { saveResolvers.push(resolve); });
    const meeting = { id: 'meeting-deferred', title: 'Planning', created_at: '2026-09-19', transcripts: [] };
    function Owner() {
      const data = useMeetingData({ meeting, summaryData: { summary_json: [{ id: 'block-A', type: 'paragraph', content: [{ type: 'text', text: 'A', styles: {} }] }] } });
      return <BlockNoteSummaryView ref={ref} summaryData={data.aiSummary} onSave={data.handleSaveSummary} />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<Owner />);
    });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 110)); });
    const block = (text: string) => [{ id: `block-${text}`, type: 'paragraph', content: [{ type: 'text', text, styles: {} }] }];
    await act(async () => onEditorChange?.(block('B')));
    let saving!: Promise<void>;
    await act(async () => { saving = ref.current!.saveSummary(); await Promise.resolve(); });
    await act(async () => onEditorChange?.(block('C')));
    let savingC!: Promise<void>;
    await act(async () => { savingC = ref.current!.saveSummary(); await Promise.resolve(); });
    expect(invoke.mock.calls.filter(([command]) => command === 'api_save_meeting_summary')).toHaveLength(1);
    await act(async () => { saveResolvers[0]!({ message: 'saved' }); await saving; });

    expect(ref.current?.isDirty).toBe(true);
    expect(ref.current?.getCurrentBlocks?.()).toEqual(block('C'));
    expect(saveResolvers).toHaveLength(2);
    await act(async () => { saveResolvers[1]!({ message: 'saved' }); await savingC; });
    const savedDocuments = invoke.mock.calls
      .filter(([command]) => command === 'api_save_meeting_summary')
      .map(([, args]) => args?.summary);
    expect(savedDocuments).toEqual([
      expect.objectContaining({ markdown: 'B', summary_json: block('B') }),
      expect.objectContaining({ markdown: 'C', summary_json: block('C') }),
    ]);
    expect(ref.current?.isDirty).toBe(false);
    await act(async () => renderer.unmount());
  });

  test('keeps the mounted Markdown editor on C when the deferred B save is acknowledged', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    const saveResolvers: Array<(value: unknown) => void> = [];
    saveHandler = () => new Promise((resolve) => { saveResolvers.push(resolve); });
    const block = (text: string) => [{ id: `block-${text}`, type: 'paragraph', content: [{ type: 'text', text, styles: {} }] }];
    parseMarkdown = async (markdown) => block(markdown);
    const meeting = { id: 'meeting-markdown-deferred', title: 'Planning', created_at: '2026-09-19', transcripts: [] };
    function Owner() {
      const data = useMeetingData({ meeting, summaryData: { markdown: 'A' } });
      return <BlockNoteSummaryView ref={ref} summaryData={data.aiSummary} onSave={data.handleSaveSummary} />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<Owner />);
      await new Promise((resolve) => setTimeout(resolve, 110));
    });

    await act(async () => onMarkdownEditorChange?.(block('B')));
    let savingB!: Promise<void>;
    await act(async () => { savingB = ref.current!.saveSummary(); await Promise.resolve(); });
    await act(async () => onMarkdownEditorChange?.(block('C')));
    await act(async () => { saveResolvers[0]!({ message: 'saved' }); await savingB; });

    expect(ref.current?.isDirty).toBe(true);
    expect(ref.current?.getCurrentBlocks?.()).toEqual(block('C'));
    expect(renderer.root.findByProps({ 'data-editor': 'markdown' }).children.join('')).toContain('C');
    expect(editorInitialContents).toEqual([]);

    await act(async () => onMarkdownEditorChange?.(block('C plus next edit')));
    let savingC!: Promise<void>;
    await act(async () => { savingC = ref.current!.saveSummary(); await Promise.resolve(); });
    await act(async () => { saveResolvers[1]!({ message: 'saved' }); await savingC; });
    const savedDocuments = invoke.mock.calls
      .filter(([command]) => command === 'api_save_meeting_summary')
      .map(([, args]) => args?.summary);
    expect(savedDocuments).toEqual([
      expect.objectContaining({ markdown: 'B', summary_json: block('B') }),
      expect.objectContaining({ markdown: 'C plus next edit', summary_json: block('C plus next edit') }),
    ]);
    expect(ref.current?.isDirty).toBe(false);
    await act(async () => renderer.unmount());
  });

  test('drops an older conversion when a later save intent finishes first', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    const block = (text: string) => [{ id: `block-${text}`, type: 'paragraph', content: [{ type: 'text', text, styles: {} }] }];
    let resolveB!: (markdown: string) => void;
    convertBlocks = (blocks) => {
      const text = blocks[0]?.content?.[0]?.text;
      return text === 'B' ? new Promise((resolve) => { resolveB = resolve; }) : Promise.resolve(text ?? '');
    };
    const meeting = { id: 'meeting-save-order', title: 'Planning', created_at: '2026-09-19', transcripts: [] };
    function Owner() {
      const data = useMeetingData({ meeting, summaryData: { summary_json: block('A') } });
      return <BlockNoteSummaryView ref={ref} summaryData={data.aiSummary} onSave={data.handleSaveSummary} />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<Owner />);
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    await act(async () => onEditorChange?.(block('B')));
    let savingB!: Promise<void>;
    await act(async () => { savingB = ref.current!.saveSummary(); await Promise.resolve(); });
    await act(async () => onEditorChange?.(block('C')));
    await act(async () => ref.current!.saveSummary());
    await act(async () => { resolveB('B'); await savingB; });

    const writes = invoke.mock.calls
      .filter(([command]) => command === 'api_save_meeting_summary')
      .map(([, args]) => (args?.summary as { markdown: string }).markdown);
    expect(writes).toEqual(['C']);
    expect(ref.current?.getCurrentBlocks?.()).toEqual(block('C'));
    await act(async () => renderer.unmount());
  });

  test('drops an already-started conversion after an acknowledged document replacement', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    const block = (text: string) => [{ id: `block-${text}`, type: 'paragraph', content: [{ type: 'text', text, styles: {} }] }];
    let resolveConversion!: (markdown: string) => void;
    convertBlocks = () => new Promise((resolve) => { resolveConversion = resolve; });
    parseMarkdown = async (markdown) => block(markdown);
    let replaceDocument!: (summary: { markdown: string }) => void;
    let currentSummary: unknown;
    const meeting = { id: 'meeting-replacement', title: 'Planning', created_at: '2026-09-19', transcripts: [] };
    function Owner() {
      const data = useMeetingData({ meeting, summaryData: { summary_json: block('A') } });
      replaceDocument = data.setAiSummary;
      currentSummary = data.aiSummary;
      return <BlockNoteSummaryView ref={ref} summaryData={data.aiSummary} onSave={data.handleSaveSummary} />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<Owner />);
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    await act(async () => onEditorChange?.(block('Obsolete A')));
    let saving!: Promise<void>;
    await act(async () => { saving = ref.current!.saveSummary(); await Promise.resolve(); });
    await act(async () => replaceDocument({ markdown: 'Acknowledged B' }));
    await act(async () => { resolveConversion('Obsolete A'); await saving; });
    convertBlocks = async (blocks) => blocks
      .flatMap((block) => block.content ?? [])
      .map((item: any) => item.text ?? '')
      .join('');

    expect(invoke.mock.calls.filter(([command]) => command === 'api_save_meeting_summary')).toEqual([]);
    expect(currentSummary).toEqual({ markdown: 'Acknowledged B' });
    expect(await ref.current?.getMarkdown()).toBe('Acknowledged B');
    expect(ref.current?.isDirty).toBe(false);
    await act(async () => renderer.unmount());
  });

  test('copy reacquires the live imperative ref instead of saving retained B after C', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    const block = (text: string) => [{ id: `block-${text}`, type: 'paragraph', content: [{ type: 'text', text, styles: {} }] }];
    let resolveCopyB!: (markdown: string) => void;
    let heldB = false;
    convertBlocks = (blocks) => {
      const text = blocks[0]?.content?.[0]?.text ?? '';
      if (text === 'B' && !heldB) {
        heldB = true;
        return new Promise((resolve) => { resolveCopyB = resolve; });
      }
      return Promise.resolve(text);
    };
    const writeText = mock(async (_text: string) => {});
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { clipboard: { writeText } },
    });
    let copySummary!: () => Promise<void>;
    let currentSummary: unknown;
    const meeting = { id: 'meeting-copy-authority', title: 'Planning', created_at: '2026-09-19', transcripts: [] };
    function Owner() {
      const data = useMeetingData({ meeting, summaryData: { summary_json: block('A') } });
      currentSummary = data.aiSummary;
      copySummary = useCopyOperations({
        meeting,
        transcripts: [],
        meetingTitle: meeting.title,
        aiSummary: data.aiSummary,
        blockNoteSummaryRef: ref,
      }).handleCopySummary;
      return <BlockNoteSummaryView ref={ref} summaryData={data.aiSummary} onSave={data.handleSaveSummary} />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<Owner />);
      await new Promise((resolve) => setTimeout(resolve, 110));
    });

    await act(async () => onEditorChange?.(block('B')));
    let copying!: Promise<void>;
    await act(async () => { copying = copySummary(); await Promise.resolve(); });
    await act(async () => onEditorChange?.(block('C')));
    await act(async () => ref.current!.saveSummary());
    await act(async () => { resolveCopyB('B'); await copying; });

    const writes = invoke.mock.calls
      .filter(([command]) => command === 'api_save_meeting_summary')
      .map(([, args]) => (args?.summary as { markdown: string }).markdown);
    expect(writes).toEqual(['C']);
    expect(currentSummary).toMatchObject({ markdown: 'C', summary_json: block('C') });
    expect(ref.current?.getCurrentBlocks?.()).toEqual(block('C'));
    expect(ref.current?.isDirty).toBe(false);
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText.mock.calls[0][0]).toContain('\n\nC\n');
    expect(writeText.mock.calls[0][0]).not.toContain('\n\nB\n');
    await act(async () => renderer.unmount());
  });

  test('an obsolete imperative save callback cannot acquire replacement authority', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    const block = (text: string) => [{ id: `block-${text}`, type: 'paragraph', content: [{ type: 'text', text, styles: {} }] }];
    parseMarkdown = async (markdown) => block(markdown);
    let replaceDocument!: (summary: { markdown: string }) => void;
    const meeting = { id: 'meeting-old-callback', title: 'Planning', created_at: '2026-09-19', transcripts: [] };
    function Owner() {
      const data = useMeetingData({ meeting, summaryData: { summary_json: block('A') } });
      replaceDocument = data.setAiSummary;
      return <BlockNoteSummaryView ref={ref} summaryData={data.aiSummary} onSave={data.handleSaveSummary} />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<Owner />);
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    await act(async () => onEditorChange?.(block('Obsolete B')));
    const obsoleteSave = ref.current!.saveSummary;
    await act(async () => {
      replaceDocument({ markdown: 'Acknowledged D' });
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    await act(async () => obsoleteSave());

    expect(invoke.mock.calls.filter(([command]) => command === 'api_save_meeting_summary')).toEqual([]);
    expect(await ref.current?.getMarkdown()).toBe('Acknowledged D');
    expect(ref.current?.isDirty).toBe(false);
    await act(async () => renderer.unmount());
  });

  test('uses the acknowledged document after the actual workspace unmounts and remounts the editor', async () => {
    const meeting = { id: 'meeting-workspace', title: 'Planning', created_at: '2026-09-19', transcripts: [] };
    const block = (text: string) => [{ id: `block-${text}`, type: 'paragraph', content: [{ type: 'text', text, styles: {} }] }];
    let switchMode!: (mode: 'enhanced' | 'raw') => Promise<void>;
    let exportMarkdown!: () => Promise<void>;
    let currentSummary: unknown;
    let summaryRef!: ReturnType<typeof useMeetingData>['blockNoteSummaryRef'];
    function Owner() {
      const [mode, setMode] = useState<'enhanced' | 'raw'>('enhanced');
      const data = useMeetingData({ meeting, summaryData: { summary_json: block('A') } });
      currentSummary = data.aiSummary;
      summaryRef = data.blockNoteSummaryRef;
      exportMarkdown = useCopyOperations({
        meeting, transcripts: [], meetingTitle: meeting.title, aiSummary: data.aiSummary,
        blockNoteSummaryRef: data.blockNoteSummaryRef, currentNotesDocument: null,
      }).handleExportMarkdown;
      switchMode = useCallback(async (next) => {
        if (data.blockNoteSummaryRef.current?.isDirty) await data.blockNoteSummaryRef.current.saveSummary();
        setMode(next);
      }, [data.blockNoteSummaryRef]);
      return <MeetingWorkspace
        title={meeting.title} createdAt={meeting.created_at} notesMode={mode}
        onNotesModeChange={(next) => void switchMode(next)} canShowEnhanced
        summary={<BlockNoteSummaryView ref={data.blockNoteSummaryRef} summaryData={data.aiSummary} onSave={data.handleSaveSummary} />}
        rawNotes={<span>Raw</span>} transcript={null} assistant={null} showAssistant={false} peopleCount={0}
      />;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<Owner />);
    });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 110)); });
    await act(async () => onEditorChange?.(block('B')));
    expect(summaryRef.current?.isDirty).toBe(true);
    await act(async () => summaryRef.current?.saveSummary());
    expect(currentSummary).toMatchObject({ markdown: 'B', summary_json: block('B') });
    await act(async () => switchMode('raw'));
    expect(summaryRef.current).toBeNull();
    await act(async () => exportMarkdown());
    const exported = invoke.mock.calls.find(([command]) => command === 'save_text_export')?.[1]?.contents;
    expect(exported).toContain('B');
    expect(exported).not.toContain('\nA\n');
    await act(async () => {
      await switchMode('enhanced');
      await new Promise((resolve) => setTimeout(resolve, 110));
    });
    expect(await summaryRef.current?.getMarkdown()).toBe('B');
    await act(async () => renderer.unmount());
  });

  test('saves the first edit made immediately after the enhanced editor remounts', async () => {
    const ref = createRef<BlockNoteSummaryViewRef>();
    const save = mock(async () => {});
    const block = (text: string) => [{ id: `block-${text}`, type: 'paragraph', content: [{ type: 'text', text, styles: {} }] }];
    let renderer!: ReactTestRenderer;

    await act(async () => {
      renderer = create(<BlockNoteSummaryView ref={ref} summaryData={{ summary_json: block('BRAVO') }} onSave={save} />);
    });
    await act(async () => renderer.update(<span>Raw</span>));
    await act(async () => {
      renderer.update(<BlockNoteSummaryView ref={ref} summaryData={{ summary_json: block('BRAVO') }} onSave={save} />);
    });
    await act(async () => onEditorChange?.(block('DELTA')));

    expect(ref.current?.isDirty).toBe(true);
    await act(async () => ref.current?.saveSummary());
    expect(save).toHaveBeenCalledWith(expect.objectContaining({
      markdown: 'DELTA',
      summary_json: block('DELTA'),
    }));
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
