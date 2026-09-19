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
  if (command === 'get_meeting_folder_path') return '/live/retry-session';
  if (command === 'load_live_notes') {
    liveLoadAttempts += 1;
    if (liveLoadAttempts === 1) throw new Error('temporary read failure');
    return document('recovered live notes');
  }
  if (command === 'save_meeting_live_notes' || command === 'save_live_notes') return undefined;
  throw new Error(`Unexpected command: ${command}`);
});

let recordingState = { isRecording: true, recordingDuration: 12 };
const renderedRawDocuments: string[] = [];

mock.module('@tauri-apps/api/core', () => ({ invoke }));
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

const { MeetingRawNotesEditor } = await import('../../src/components/MeetingDetails/MeetingRawNotesEditor');
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
      renderer = create(createElement(MeetingRawNotesEditor, { meetingId: 'identity-a' }));
    });
    await settle();
    expect(renderedRawDocuments).toContain('meeting A notes');

    renderedRawDocuments.length = 0;
    act(() => {
      renderer.update(createElement(MeetingRawNotesEditor, { meetingId: 'identity-b' }));
    });
    expect(renderedRawDocuments).not.toContain('meeting A notes');

    meetingB.resolve(document('meeting B notes'));
    await settle();
    expect(renderedRawDocuments).toContain('meeting B notes');
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
