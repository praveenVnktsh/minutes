import { describe, expect, mock, test } from 'bun:test';
import { createElement, type ComponentType } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { LiveNotesDocument } from '../../src/lib/liveNotes';

function document(text: string): LiveNotesDocument {
  return {
    version: 2,
    meetingStartedAtMs: 1,
    updatedAt: text,
    notes: [{ id: 'note', timestampSeconds: 0, text, important: false }],
    rawMarkdown: text,
  };
}

/** A meeting whose notes the AI wrote into during the recording. */
function aiDocument(): LiveNotesDocument {
  const content = (text: string) => [{ type: 'text', text, styles: {} }];
  return {
    version: 2,
    meetingStartedAtMs: 1,
    updatedAt: 'with-ai',
    notes: [],
    rawMarkdown: 'with ai',
    editorBlocks: [
      { id: 'mine', type: 'bulletListItem', props: {}, content: content('My line'), children: [] },
      { id: 'h', type: 'heading', props: { level: 3 }, content: content('Launch'), children: [] },
    ],
    aiBlocks: { h: { entryId: '', text: 'Launch', topic: 'Launch' } },
    aiEntries: ['d1'],
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

const values = new Map<string, string>();
Object.defineProperty(globalThis, 'localStorage', {
  configurable: true,
  value: {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  },
});
Object.defineProperty(globalThis, 'window', {
  configurable: true,
  value: {
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => true,
  },
});

const meetingB = deferred<LiveNotesDocument | null>();
let liveLoadAttempts = 0;
const invoke = mock(async (command: string, args?: Record<string, unknown>): Promise<unknown> => {
  if (command === 'get_meeting_live_notes') {
    if (args?.meetingId === 'identity-a') return document('meeting A notes');
    if (args?.meetingId === 'identity-b') return meetingB.promise;
  }
  if (command === 'get_meeting_live_notes' && args?.meetingId === 'with-ai') return aiDocument();
  if (command === 'get_meeting_live_notes' && args?.meetingId === 'old') return document('old meeting notes');
  if (command === 'list_meeting_timeline_entries') return [
    { id: 'd1', start_ms: 60_000, end_ms: 65_000, kind: 'decision', text: 'Launch on Nov 12', speaker: null, provider: 'p', model: 'm', created_at: '', topic: 'Launch', meeting_id: 'with-ai' },
  ];
  if (command === 'get_meeting_folder_path') return '/live/retry-session';
  if (command === 'load_live_notes') {
    liveLoadAttempts += 1;
    if (liveLoadAttempts === 1) throw new Error('temporary read failure');
    return document('recovered live notes');
  }
  if (command === 'save_meeting_live_notes' || command === 'save_live_notes') return undefined;
  if (command === 'api_get_summary') return null;
  throw new Error(`Unexpected command: ${command}`);
});

let recordingState = { isRecording: true, recordingDuration: 12 };
const renderedRawDocuments: string[] = [];

const originalCore = { ...await import('@tauri-apps/api/core') };
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('@tauri-apps/api/event', () => ({ listen: async () => () => {} }));
mock.module('../../src/contexts/RecordingStateContext', () => ({ useRecordingState: () => recordingState }));
mock.module('../../src/components/BlockNotesEditor', () => ({
  BlockNotesEditor: ({ document: value }: { document: LiveNotesDocument }) => {
    renderedRawDocuments.push(value.rawMarkdown ?? '');
    return createElement('div', { 'data-raw-document': value.rawMarkdown });
  },
}));
mock.module('next/dynamic', () => ({
  default: () => ({ document: value }: { document: LiveNotesDocument }) => (
    createElement('div', { 'data-live-document': value.rawMarkdown })
  ),
}));

const { MeetingNotesEditor } = await import('../../src/components/MeetingDetails/MeetingNotesEditor');
const { LiveNotesPad } = await import('../../src/components/LiveNotesPad');
const TestLiveNotesPad = LiveNotesPad as ComponentType<{ bare?: boolean }>;

async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

describe('note editor state boundaries', () => {
  test('never renders the previous meeting document under a new identity', async () => {
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(createElement(MeetingNotesEditor, { meetingId: 'identity-a' }));
    });
    await settle();
    expect(renderedRawDocuments).toContain('meeting A notes');

    renderedRawDocuments.length = 0;
    act(() => {
      renderer.update(createElement(MeetingNotesEditor, { meetingId: 'identity-b' }));
    });
    expect(renderedRawDocuments).not.toContain('meeting A notes');

    meetingB.resolve(document('meeting B notes'));
    await settle();
    expect(renderedRawDocuments).toContain('meeting B notes');
    renderer.unmount();
  });

  test('after the meeting, adds Decisions to a document the AI wrote into, once', async () => {
    renderedRawDocuments.length = 0;
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(createElement(MeetingNotesEditor, { meetingId: 'with-ai', legacyNotes: 'Stored summary' }));
    });
    await settle();
    await settle();
    const latest = renderedRawDocuments[renderedRawDocuments.length - 1];
    expect(latest).toContain('### Decisions');
    expect(latest).toContain('- Launch on Nov 12');
    expect(invoke.mock.calls.filter(([command]) => command === 'list_meeting_timeline_entries')).toHaveLength(1);
    // The doc has AI lines, so the stored summary is not shown as well.
    expect(JSON.stringify(renderer.toJSON())).not.toContain('Stored summary');
    renderer.unmount();
  });

  test('an old meeting shows its stored Enhanced notes below its notes, read-only', async () => {
    const calls = invoke.mock.calls.length;
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(createElement(MeetingNotesEditor, { meetingId: 'old', legacyNotes: 'Old enhanced summary' }));
    });
    await settle();
    expect(JSON.stringify(renderer.toJSON())).toContain('old meeting notes');
    expect(JSON.stringify(renderer.toJSON())).toContain('Old enhanced summary');
    // Nothing is written to an old meeting.
    expect(invoke.mock.calls.slice(calls).some(([command]) => command === 'list_meeting_timeline_entries')).toBe(false);
    renderer.unmount();
  });

  test('shows and recovers from a load failure in bare live mode', async () => {
    recordingState = { isRecording: true, recordingDuration: 12 };
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(createElement(TestLiveNotesPad, { bare: true }));
    });
    await settle();

    const retry = renderer.root.findAllByType('button').find((button) => button.children.includes('Retry'));
    expect(retry).toBeDefined();
    expect(JSON.stringify(renderer.toJSON())).toContain('Could not load your notes');

    await act(async () => { await retry!.props.onClick(); });
    await settle();
    expect(JSON.stringify(renderer.toJSON())).toContain('recovered live notes');
    expect(JSON.stringify(renderer.toJSON())).not.toContain('Could not load your notes');
    renderer.unmount();
  });
});
