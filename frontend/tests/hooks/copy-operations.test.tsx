import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { LiveNotesDocument } from '../../src/lib/liveNotes';
import type { MeetingSummary } from '../../src/types';
import type { NoteDocumentTarget } from '../../src/services/notePersistenceService';

const originalTauri = { ...await import('@tauri-apps/api/core') };
const originalSonner = { ...await import('sonner') };
const originalPersistence = { ...await import('../../src/services/notePersistenceService') };
const originalAnalytics = { ...await import('../../src/lib/analytics') };
const originalNavigator = globalThis.navigator;

const invoke = mock(async (_command: string, _args?: Record<string, unknown>): Promise<unknown> => undefined);
const success = mock(() => {});
const errorToast = mock(() => {});
const flushNotes = mock(async () => {});
const loadNotes = mock(async (): Promise<{
  loadState: string;
  loadError: unknown | null;
  document: LiveNotesDocument | null;
}> => ({
  loadState: 'ready',
  loadError: null,
  document: null,
}));

mock.module('@tauri-apps/api/core', () => ({ ...originalTauri, invoke }));
mock.module('sonner', () => ({ toast: { success, error: errorToast } }));
mock.module('../../src/services/notePersistenceService', () => ({
  meetingNotesTarget: (meetingId: string) => ({ kind: 'meeting', meetingId }),
  notePersistenceService: { flushNotes, loadNotes },
}));
mock.module('../../src/lib/analytics', () => ({
  default: { trackCopy: () => Promise.resolve() },
}));

const { useCopyOperations } = await import('../../src/hooks/meeting-details/useCopyOperations');

const text = (value: string) => [{ type: 'text', text: value, styles: {} }];
/** The user's line, then an AI topic heading and line. */
const mixedDocument: LiveNotesDocument = {
  version: 2,
  meetingStartedAtMs: 0,
  updatedAt: '2026-09-18T10:00:00Z',
  notes: [],
  editorBlocks: [
    { id: 'mine', type: 'bulletListItem', content: text('My own line') },
    { id: 'ai-h', type: 'heading', props: { level: 3 }, content: text('Pricing') },
    { id: 'ai-1', type: 'bulletListItem', content: text('Free tier held back') },
  ],
  aiBlocks: {
    'ai-h': { entryId: '', text: 'Pricing', topic: 'Pricing' },
    'ai-1': { entryId: 'e1', text: 'Free tier held back', topic: 'Pricing' },
  },
  aiEntries: ['e1'],
};

let operations!: ReturnType<typeof useCopyOperations>;
function Probe({
  summary = null,
  showAiNotes = true,
  notesTarget,
  currentNotesDocument,
}: {
  summary?: MeetingSummary | null;
  showAiNotes?: boolean;
  notesTarget?: NoteDocumentTarget;
  currentNotesDocument?: LiveNotesDocument | null;
}) {
  operations = useCopyOperations({
    meeting: { id: 'meeting-1', title: 'Planning', created_at: '2026-09-18T10:00:00Z' },
    transcripts: [],
    meetingTitle: 'Planning',
    aiSummary: summary,
    showAiNotes,
    notesTarget,
    currentNotesDocument,
  });
  return null;
}

function exported(): string {
  const save = invoke.mock.calls.find(([command]) => command === 'save_text_export');
  expect(save).toBeDefined();
  return (save?.[1] as { contents: string }).contents;
}

function clipboard() {
  const writeText = mock(async (_text: string) => {});
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: { clipboard: { writeText } },
  });
  return writeText;
}

let renderer: ReactTestRenderer | undefined;
beforeEach(async () => {
  invoke.mockClear();
  success.mockClear();
  errorToast.mockClear();
  flushNotes.mockClear();
  loadNotes.mockClear();
  loadNotes.mockImplementation(async () => ({
    loadState: 'ready',
    loadError: null,
    document: {
      version: 2,
      meetingStartedAtMs: 0,
      updatedAt: '2026-09-18T10:00:00Z',
      notes: [],
      editorBlocks: [{ id: 'private', type: 'paragraph', content: text('Only note') }],
    },
  }));
  invoke.mockImplementation(async (command) => {
    if (command === 'api_get_meeting_transcripts') return { transcripts: [], total_count: 0, has_more: false };
    if (command === 'save_text_export') return '/tmp/Planning.md';
    return undefined;
  });
  await act(async () => { renderer = create(<Probe />); });
});

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: originalNavigator });
});

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalTauri);
  mock.module('sonner', () => originalSonner);
  mock.module('../../src/services/notePersistenceService', () => originalPersistence);
  mock.module('../../src/lib/analytics', () => originalAnalytics);
});

describe('useCopyOperations', () => {
  test('flushes and exports a notes-only meeting', async () => {
    await act(async () => { await operations.handleExportMarkdown(); });

    expect(flushNotes).toHaveBeenCalledTimes(1);
    expect(loadNotes).toHaveBeenCalledTimes(1);
    expect(exported()).toContain('## Notes\n\nOnly note');
    expect(success).toHaveBeenCalledTimes(1);
    expect(errorToast).not.toHaveBeenCalled();
  });

  test('export keeps AI lines when they are shown and leaves them out when hidden', async () => {
    await act(async () => { renderer!.update(<Probe currentNotesDocument={mixedDocument} />); });
    await act(async () => { await operations.handleExportMarkdown(); });
    expect(exported()).toContain('- My own line\n\n### Pricing\n\n- Free tier held back');

    invoke.mockClear();
    await act(async () => { renderer!.update(<Probe currentNotesDocument={mixedDocument} showAiNotes={false} />); });
    await act(async () => { await operations.handleExportMarkdown(); });
    const contents = exported();
    expect(contents).toContain('- My own line');
    expect(contents).not.toContain('Pricing');
    expect(contents).not.toContain('Free tier held back');
  });

  test('copy notes follows the toggle too', async () => {
    const writeText = clipboard();
    await act(async () => { renderer!.update(<Probe currentNotesDocument={mixedDocument} showAiNotes={false} />); });
    await act(async () => { await operations.handleCopyNotes(); });
    expect(writeText.mock.calls[0][0]).toBe('# Planning\n\n- My own line\n');

    await act(async () => { renderer!.update(<Probe currentNotesDocument={mixedDocument} />); });
    await act(async () => { await operations.handleCopyNotes(); });
    expect(writeText.mock.calls[1][0]).toContain('Free tier held back');
    expect(errorToast).not.toHaveBeenCalled();
  });

  test('an old meeting exports its Enhanced notes as AI notes only while they are shown', async () => {
    const summary: MeetingSummary = { markdown: '## Decisions\n\n- Launch Friday' };
    await act(async () => { renderer!.update(<Probe summary={summary} />); });
    await act(async () => { await operations.handleExportMarkdown(); });
    expect(exported()).toContain('## Notes\n\nOnly note\n\n## AI notes\n\n## Decisions\n\n- Launch Friday');

    invoke.mockClear();
    await act(async () => { renderer!.update(<Probe summary={summary} showAiNotes={false} />); });
    await act(async () => { await operations.handleExportMarkdown(); });
    expect(exported()).not.toContain('Launch Friday');
  });

  test('a meeting with AI lines never adds its stored summary', async () => {
    await act(async () => {
      renderer!.update(<Probe summary={{ markdown: 'Stored summary' }} currentNotesDocument={mixedDocument} />);
    });
    await act(async () => { await operations.handleExportMarkdown(); });
    expect(exported()).not.toContain('Stored summary');
  });

  test('does not write or report success after a transcript read failure', async () => {
    invoke.mockImplementation(async (command) => {
      if (command === 'api_get_meeting_transcripts') throw new Error('database unavailable');
      if (command === 'save_text_export') throw new Error('must not save');
      return undefined;
    });

    await act(async () => { await operations.handleExportMarkdown(); });
    expect(invoke.mock.calls.some(([command]) => command === 'save_text_export')).toBe(false);
    expect(success).not.toHaveBeenCalled();
    expect(errorToast).toHaveBeenCalledTimes(1);
    expect(errorToast).toHaveBeenCalledWith('Failed to export meeting');
  });

  test('does not export after a note load failure', async () => {
    loadNotes.mockImplementation(async () => ({
      loadState: 'error',
      loadError: new Error('notes unavailable'),
      document: null,
    }));

    await act(async () => { await operations.handleExportMarkdown(); });
    expect(invoke).not.toHaveBeenCalled();
    expect(success).not.toHaveBeenCalled();
    expect(errorToast).toHaveBeenCalledTimes(1);
  });

  test('reports one failure and no success when the clipboard rejects', async () => {
    invoke.mockImplementation(async (command) => {
      if (command === 'api_get_meeting_transcripts') {
        return {
          transcripts: [{ id: 'one', text: 'Hello', timestamp: '10:00:00', speaker: 'mic' }],
          total_count: 1,
          has_more: false,
        };
      }
      return undefined;
    });
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { clipboard: { writeText: () => Promise.reject(new Error('permission denied')) } },
    });

    await act(async () => { await operations.handleCopyTranscript(); });
    expect(success).not.toHaveBeenCalled();
    expect(errorToast).toHaveBeenCalledTimes(1);
    expect(errorToast).toHaveBeenCalledWith('Failed to copy transcript');
  });

  test('flushes an optional live target and exports its supplied current document', async () => {
    const liveTarget: NoteDocumentTarget = {
      kind: 'live',
      sessionId: '/recordings/live',
      folderPath: '/recordings/live',
    };
    const liveDocument: LiveNotesDocument = {
      version: 2,
      meetingStartedAtMs: 0,
      updatedAt: '2026-09-18T10:00:00Z',
      notes: [],
      editorBlocks: [{ id: 'live', type: 'paragraph', content: text('Unsaved live note') }],
    };
    await act(async () => {
      renderer!.update(<Probe notesTarget={liveTarget} currentNotesDocument={liveDocument} />);
    });

    await act(async () => { await operations.handleExportMarkdown(); });
    expect(flushNotes).toHaveBeenCalledWith(liveTarget);
    expect(loadNotes).not.toHaveBeenCalled();
    expect(exported()).toContain('Unsaved live note');
  });
});
