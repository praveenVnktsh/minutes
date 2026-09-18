import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { MeetingMetadata, StoredTranscript } from '../../src/services/indexedDBService';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalIndexedDB = { ...await import('../../src/services/indexedDBService') };
const originalStorage = { ...await import('../../src/services/storageService') };
const originalSummaryLanguage = { ...await import('../../src/lib/summary-language-preferences') };
const originalToast = { ...await import('sonner') };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('../../src/services/indexedDBService', () => originalIndexedDB);
  mock.module('../../src/services/storageService', () => originalStorage);
  mock.module('../../src/lib/summary-language-preferences', () => originalSummaryLanguage);
  mock.module('sonner', () => originalToast);
});

type AudioRecoveryStatus = {
  status: string;
  chunk_count: number;
  estimated_duration_seconds: number;
  audio_file_path?: string;
  message: string;
};

const recent = Date.now() - 60_000;
let meetingMetadata: MeetingMetadata | null = null;
let meetingTranscripts: StoredTranscript[] = [];
let allMeetings: MeetingMetadata[] = [];
let audioRecoveryResult: AudioRecoveryStatus = {
  status: 'none', chunk_count: 0, estimated_duration_seconds: 0, message: 'none',
};
let audioCheckpoints: Record<string, boolean> = {};

const indexedDBService = {
  getAllMeetings: mock(async () => allMeetings),
  getMeetingMetadata: mock(async () => meetingMetadata),
  getTranscripts: mock(async () => meetingTranscripts),
  markMeetingSaved: mock(async () => {}),
};
const saveMeeting = mock(async () => ({ meeting_id: 'saved-meeting' }));
const storageService = { saveMeeting };
const applyPinnedSummaryLanguageToMeeting = mock(async () => {});
const toast = { success: mock(() => {}), error: mock(() => {}), warning: mock(() => {}), info: mock(() => {}) };

const invoke = mock(async (command: string, args?: Record<string, unknown>): Promise<unknown> => {
  switch (command) {
    case 'has_audio_checkpoints':
      return audioCheckpoints[args!.meetingFolder as string] ?? false;
    case 'recover_audio_from_checkpoints':
      return audioRecoveryResult;
    case 'get_meeting_folder_path':
      return null;
    case 'attach_live_notes':
    case 'cleanup_checkpoints':
      return undefined;
    default:
      throw new Error(`Unexpected command: ${command}`);
  }
});

mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('../../src/services/indexedDBService', () => ({ indexedDBService }));
mock.module('../../src/services/storageService', () => ({ storageService }));
mock.module('../../src/lib/summary-language-preferences', () => ({ applyPinnedSummaryLanguageToMeeting }));
mock.module('sonner', () => ({ toast }));

const { useTranscriptRecovery } = await import('../../src/hooks/useTranscriptRecovery');

let state: ReturnType<typeof useTranscriptRecovery>;
let renderer: ReactTestRenderer | undefined;

function View() {
  state = useTranscriptRecovery();
  return <output>{state.recoverableMeetings.map(m => m.meetingId).join(',')}</output>;
}

beforeEach(() => {
  invoke.mockClear();
  saveMeeting.mockClear();
  indexedDBService.getAllMeetings.mockClear();
  indexedDBService.getMeetingMetadata.mockClear();
  indexedDBService.getTranscripts.mockClear();
  indexedDBService.markMeetingSaved.mockClear();
  applyPinnedSummaryLanguageToMeeting.mockClear();
  toast.success.mockClear();
  meetingMetadata = null;
  meetingTranscripts = [];
  allMeetings = [];
  audioRecoveryResult = { status: 'none', chunk_count: 0, estimated_duration_seconds: 0, message: 'none' };
  audioCheckpoints = {};
});

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

async function mount() {
  await act(async () => { renderer = create(<View />); });
}

function commands(): string[] {
  return invoke.mock.calls.map(call => call[0] as string);
}

const audioOnlyMeeting: MeetingMetadata = {
  meetingId: 'meeting-audio',
  title: 'Audio only',
  startTime: recent,
  lastUpdated: recent,
  transcriptCount: 0,
  savedToSQLite: false,
  folderPath: '/recordings/audio-only',
};

const emptyMeeting: MeetingMetadata = {
  meetingId: 'meeting-empty',
  title: 'Empty',
  startTime: recent,
  lastUpdated: recent,
  transcriptCount: 0,
  savedToSQLite: false,
  folderPath: '/recordings/empty',
};

describe('recoverMeeting for an audio-only meeting', () => {
  test('aborts without saving or deleting checkpoints when the audio merge fails', async () => {
    meetingMetadata = audioOnlyMeeting;
    allMeetings = [audioOnlyMeeting];
    audioCheckpoints = { '/recordings/audio-only': true };
    audioRecoveryResult = {
      status: 'failed', chunk_count: 2, estimated_duration_seconds: 60, message: 'ffmpeg missing',
    };

    await mount();
    await act(async () => { await state.checkForRecoverableTranscripts(); });
    let error: unknown;
    await act(async () => {
      try { await state.recoverMeeting('meeting-audio'); } catch (caught) { error = caught; }
    });

    expect(error).toBeInstanceOf(Error);
    expect((error as Error).message).toContain('Audio recovery failed');
    expect(saveMeeting).not.toHaveBeenCalled();
    expect(commands()).not.toContain('cleanup_checkpoints');
    expect(indexedDBService.markMeetingSaved).not.toHaveBeenCalled();
    expect(state.recoverableMeetings.map(m => m.meetingId)).toEqual(['meeting-audio']);
  });

  test('saves the empty meeting and cleans up after a successful merge', async () => {
    meetingMetadata = audioOnlyMeeting;
    audioRecoveryResult = {
      status: 'success', chunk_count: 2, estimated_duration_seconds: 60,
      audio_file_path: '/recordings/audio-only/audio.mp4', message: 'recovered',
    };

    await mount();
    let result: Awaited<ReturnType<typeof state.recoverMeeting>> | undefined;
    await act(async () => { result = await state.recoverMeeting('meeting-audio'); });

    expect(saveMeeting).toHaveBeenCalledWith('Audio only', [], '/recordings/audio-only');
    expect(commands()).toContain('cleanup_checkpoints');
    expect(result!.transcriptCount).toBe(0);
    expect(result!.audioRecoveryStatus?.status).toBe('success');
  });
});

describe('checkForRecoverableTranscripts', () => {
  test('keeps an audio-only meeting and drops one with neither audio nor transcripts', async () => {
    allMeetings = [audioOnlyMeeting, emptyMeeting];
    audioCheckpoints = { '/recordings/audio-only': true, '/recordings/empty': false };

    await mount();
    await act(async () => { await state.checkForRecoverableTranscripts(); });

    expect(state.recoverableMeetings.map(m => m.meetingId)).toEqual(['meeting-audio']);
  });
});
