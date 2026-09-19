import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { LiveNotesDocument } from '../../src/lib/liveNotes';
import type { MeetingSummary } from '../../src/types';
import type { BlockNoteSummaryViewRef } from '../../src/components/AISummary/BlockNoteSummaryView';
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

mock.module('@tauri-apps/api/core', () => ({ invoke }));
mock.module('sonner', () => ({ toast: { success, error: errorToast } }));
mock.module('../../src/services/notePersistenceService', () => ({
  meetingNotesTarget: (meetingId: string) => ({ kind: 'meeting', meetingId }),
  notePersistenceService: { flushNotes, loadNotes },
}));
mock.module('../../src/lib/analytics', () => ({
  default: { trackCopy: () => Promise.resolve() },
}));

const { useCopyOperations } = await import('../../src/hooks/meeting-details/useCopyOperations');

let operations!: ReturnType<typeof useCopyOperations>;
function Probe({
  summary = null,
  summaryRef = null,
  notesTarget,
  currentNotesDocument,
}: {
  summary?: MeetingSummary | null;
  summaryRef?: BlockNoteSummaryViewRef | null;
  notesTarget?: NoteDocumentTarget;
  currentNotesDocument?: LiveNotesDocument | null;
}) {
  operations = useCopyOperations({
    meeting: { id: 'meeting-1', title: 'Planning', created_at: '2026-09-18T10:00:00Z' },
    transcripts: [],
    meetingTitle: 'Planning',
    aiSummary: summary,
    blockNoteSummaryRef: { current: summaryRef },
    notesTarget,
    currentNotesDocument,
  });
  return null;
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
      editorBlocks: [{ id: 'private', type: 'paragraph', content: [{ type: 'text', text: 'Only note', styles: {} }] }],
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
    const save = invoke.mock.calls.find(([command]) => command === 'save_text_export');
    expect(save).toBeDefined();
    expect((save?.[1] as { contents: string }).contents).toContain('## Original notes\n\nOnly note');
    expect(success).toHaveBeenCalledTimes(1);
    expect(errorToast).not.toHaveBeenCalled();
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

  test('uses stored legacy enhanced notes when the mounted legacy ref returns empty', async () => {
    const writeText = mock(async (_text: string) => {});
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { clipboard: { writeText } },
    });
    const legacySummary: MeetingSummary = {
      decisions: {
        title: 'Decisions',
        blocks: [{ id: 'legacy', type: 'bullet', content: 'Launch Friday', color: 'default' }],
      },
    };
    await act(async () => {
      renderer!.update(<Probe
        summary={legacySummary}
        summaryRef={{ isDirty: false, saveSummary: async () => {}, getMarkdown: async () => '' }}
      />);
    });

    await act(async () => { await operations.handleCopySummary(); });
    expect(writeText.mock.calls[0][0]).toContain('## Decisions\n\n- Launch Friday');
    expect(success).toHaveBeenCalledTimes(1);
    expect(errorToast).not.toHaveBeenCalled();
  });

  test('fails export when a mounted structured editor returns empty for known content', async () => {
    const summary: MeetingSummary = {
      summary_json: [{ id: 'one', type: 'paragraph', content: [{ type: 'text', text: 'Known content', styles: {} }] }],
    };
    await act(async () => {
      renderer!.update(<Probe
        summary={summary}
        summaryRef={{ isDirty: false, saveSummary: async () => {}, getMarkdown: async () => '' }}
      />);
    });

    await act(async () => { await operations.handleExportMarkdown(); });
    expect(invoke.mock.calls.some(([command]) => command === 'save_text_export')).toBe(false);
    expect(success).not.toHaveBeenCalled();
    expect(errorToast).toHaveBeenCalledTimes(1);
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
      editorBlocks: [{ id: 'live', type: 'paragraph', content: [{ type: 'text', text: 'Unsaved live note', styles: {} }] }],
    };
    await act(async () => {
      renderer!.update(<Probe notesTarget={liveTarget} currentNotesDocument={liveDocument} />);
    });

    await act(async () => { await operations.handleExportMarkdown(); });
    expect(flushNotes).toHaveBeenCalledWith(liveTarget);
    expect(loadNotes).not.toHaveBeenCalled();
    const save = invoke.mock.calls.find(([command]) => command === 'save_text_export');
    expect((save?.[1] as { contents: string }).contents).toContain('Unsaved live note');
  });
});
