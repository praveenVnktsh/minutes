import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { MeetingMetadata, StoredTranscript } from '@/services/indexedDBService';

let metadata: MeetingMetadata;
const transcripts: StoredTranscript[] = [
  { meetingId: 'recovery-1', text: 'hello', timestamp: '2026-01-01T00:00:00Z', sequenceId: 1 } as StoredTranscript,
];

let audioStatus = 'success';
const invoke = mock(async (command: string) =>
  command === 'recover_audio_from_checkpoints'
    ? { status: audioStatus, chunk_count: 1, estimated_duration_seconds: 1, message: '' }
    : undefined
);
const getMeetingMetadata = mock(async () => ({ ...metadata }));
const getTranscriptsStrict = mock(async () => transcripts.map((t) => ({ ...t })));
const markMeetingSavedStrict = mock(async () => {});
let markResumeAppendedError: Error | null = null;
const markResumeAppendedStrict = mock(async () => {
  if (markResumeAppendedError) throw markResumeAppendedError;
  metadata = { ...metadata, resumeAppended: true };
});
const saveMeeting = mock(async (...args: unknown[]) => ({
  status: 'success',
  meeting_id: (args[4] as string | null) ?? 'saved-id',
}));

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalIndexedDB = { ...await import('@/services/indexedDBService') };
const originalStorage = { ...await import('@/services/storageService') };
const originalSummaryLanguage = { ...await import('@/lib/summary-language-preferences') };
const originalToast = { ...await import('sonner') };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@/services/indexedDBService', () => originalIndexedDB);
  mock.module('@/services/storageService', () => originalStorage);
  mock.module('@/lib/summary-language-preferences', () => originalSummaryLanguage);
  mock.module('sonner', () => originalToast);
});
mock.module('@tauri-apps/api/core', () => ({ invoke }));
mock.module('@/services/indexedDBService', () => ({
  indexedDBService: { getMeetingMetadata, getTranscriptsStrict, markMeetingSavedStrict, markResumeAppendedStrict },
}));
mock.module('@/services/storageService', () => ({ storageService: { saveMeeting } }));
mock.module('@/lib/summary-language-preferences', () => ({
  applyPinnedSummaryLanguageToMeeting: async () => {},
}));
mock.module('sonner', () => ({ toast: { warning() {} } }));

const { useTranscriptRecovery } = await import('./useTranscriptRecovery');

let hook: ReturnType<typeof useTranscriptRecovery>;
let renderer: ReactTestRenderer | undefined;

function Probe() {
  hook = useTranscriptRecovery();
  return null;
}

async function recover(meetingId: string) {
  await act(async () => {
    renderer = create(<Probe />);
  });
  return attempt(meetingId);
}

/** Runs recoverMeeting on the already-rendered hook, returning its result or error. */
async function attempt(meetingId: string) {
  let outcome: unknown;
  await act(async () => {
    outcome = await hook.recoverMeeting(meetingId).catch((error: unknown) => error);
  });
  return outcome;
}

const realSessionStorage = Object.getOwnPropertyDescriptor(globalThis, 'sessionStorage');

function stubSessionStorage(initial: Record<string, string> = {}) {
  const store = new Map(Object.entries(initial));
  Object.defineProperty(globalThis, 'sessionStorage', {
    configurable: true,
    value: {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => void store.set(key, value),
      removeItem: (key: string) => void store.delete(key),
    },
  });
  return store;
}

function entry(extra: Partial<MeetingMetadata> = {}): MeetingMetadata {
  return {
    meetingId: 'recovery-1',
    title: 'Standup',
    startTime: 1,
    lastUpdated: 2,
    transcriptCount: 1,
    savedToSQLite: false,
    folderPath: '/meetings/standup',
    ...extra,
  };
}

const expectedTranscripts = [expect.objectContaining({ text: 'hello', sequence_id: 1 })];

beforeEach(() => {
  invoke.mockClear();
  saveMeeting.mockClear();
  markMeetingSavedStrict.mockClear();
  markResumeAppendedStrict.mockClear();
  markResumeAppendedError = null;
  audioStatus = 'success';
});

afterEach(async () => {
  if (realSessionStorage) Object.defineProperty(globalThis, 'sessionStorage', realSessionStorage);
  else Reflect.deleteProperty(globalThis, 'sessionStorage');
  if (renderer) {
    const current = renderer;
    renderer = undefined;
    await act(async () => current.unmount());
  }
});

describe('useTranscriptRecovery recoverMeeting', () => {
  test('a resumed entry appends into the meeting it resumed', async () => {
    stubSessionStorage();
    metadata = entry({ resumeOfMeetingId: 'meeting-orig' });
    await recover('recovery-1');

    expect(saveMeeting).toHaveBeenCalledTimes(1);
    expect(saveMeeting.mock.calls[0]).toEqual([
      'Standup', expectedTranscripts, '/meetings/standup', false, 'meeting-orig', true,
    ]);
    expect(markMeetingSavedStrict).toHaveBeenCalledWith('recovery-1');
  });

  test('a resumed entry ignores the bound session and remembered recovery row', async () => {
    stubSessionStorage({
      active_recording_sessions: JSON.stringify([{ meetingId: 'meeting-bound', folderPath: '/meetings/standup' }]),
      transcript_recovery_row_ids: JSON.stringify({ 'recovery-1': 'meeting-remembered' }),
    });
    metadata = entry({ resumeOfMeetingId: 'meeting-orig' });
    await recover('recovery-1');

    expect(saveMeeting.mock.calls[0]).toEqual([
      'Standup', expectedTranscripts, '/meetings/standup', false, 'meeting-orig', true,
    ]);
  });

  test('a non-resumed entry saves into the bound meeting without appending', async () => {
    stubSessionStorage({
      active_recording_sessions: JSON.stringify([{ meetingId: 'meeting-bound', folderPath: '/meetings/standup' }]),
    });
    metadata = entry();
    await recover('recovery-1');

    expect(saveMeeting.mock.calls[0]).toEqual([
      'Standup', expectedTranscripts, '/meetings/standup', false, 'meeting-bound',
    ]);
  });

  test('a non-resumed entry with no bound session creates a meeting without appending', async () => {
    stubSessionStorage();
    metadata = entry();
    await recover('recovery-1');

    expect(saveMeeting.mock.calls[0]).toEqual([
      'Standup', expectedTranscripts, '/meetings/standup', false, null,
    ]);
  });

  test('a non-resumed entry still prefers its remembered recovery row', async () => {
    stubSessionStorage({
      active_recording_sessions: JSON.stringify([{ meetingId: 'meeting-bound', folderPath: '/meetings/standup' }]),
      transcript_recovery_row_ids: JSON.stringify({ 'recovery-1': 'meeting-remembered' }),
    });
    metadata = entry();
    await recover('recovery-1');

    expect(saveMeeting.mock.calls[0]).toEqual([
      'Standup', expectedTranscripts, '/meetings/standup', false, 'meeting-remembered',
    ]);
  });

  test('retrying a resumed entry after a failed audio merge does not append its segments again', async () => {
    stubSessionStorage();
    metadata = entry({ resumeOfMeetingId: 'meeting-orig' });
    audioStatus = 'failed';

    const first = await recover('recovery-1');
    expect(first).toMatchObject({ success: false, meetingId: 'meeting-orig' });
    expect(markMeetingSavedStrict).not.toHaveBeenCalled();
    expect(markResumeAppendedStrict).toHaveBeenCalledWith('recovery-1');

    // A restart loses sessionStorage; the IndexedDB flag alone must stop a second append.
    stubSessionStorage();
    audioStatus = 'success';
    const second = await attempt('recovery-1');

    expect(second).toMatchObject({ success: true, meetingId: 'meeting-orig' });
    expect(saveMeeting).toHaveBeenCalledTimes(1);
    expect(markMeetingSavedStrict).toHaveBeenCalledWith('recovery-1');
  });

  test('a retry in the same session does not append again when flagging the entry failed', async () => {
    stubSessionStorage();
    metadata = entry({ resumeOfMeetingId: 'meeting-orig' });
    markResumeAppendedError = new Error('IndexedDB unavailable');

    const first = await recover('recovery-1');
    expect(first).toBeInstanceOf(Error);
    expect(saveMeeting).toHaveBeenCalledTimes(1);

    markResumeAppendedError = null;
    const second = await attempt('recovery-1');

    expect(second).toMatchObject({ success: true, meetingId: 'meeting-orig' });
    expect(saveMeeting).toHaveBeenCalledTimes(1);
    expect(markResumeAppendedStrict).toHaveBeenCalledTimes(2);
  });
});
