import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { ReactNode } from 'react';
import type { RecordingStartRequest } from '../../src/types/meetingActivity';
import type { TranscriptionErrorPayload } from '../../src/services/transcriptService';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalEvent = { ...await import('@tauri-apps/api/event') };
const originalPath = { ...await import('@tauri-apps/api/path') };
const originalNavigation = { ...await import('next/navigation') };

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
  mock.module('@tauri-apps/api/path', () => originalPath);
  mock.module('next/navigation', () => originalNavigation);
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

const storage = new Map<string, string>();
Object.defineProperty(globalThis, 'localStorage', {
  configurable: true,
  value: {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
    removeItem: (key: string) => storage.delete(key),
  },
});
Object.defineProperty(globalThis, 'sessionStorage', {
  configurable: true,
  value: {
    getItem: (key: string) => storage.get(`session:${key}`) ?? null,
    setItem: (key: string, value: string) => storage.set(`session:${key}`, value),
    removeItem: (key: string) => storage.delete(`session:${key}`),
  },
});
Object.defineProperty(globalThis, 'window', {
  configurable: true,
  value: {
    dispatchEvent: mock(() => {}),
    addEventListener: mock(() => {}),
    removeEventListener: mock(() => {}),
  },
});

interface TestRecordingActivity {
  session_id: string;
  meeting_id: string | null;
  status: 'recording' | 'paused';
  error: null;
}
let activityRecording: TestRecordingActivity | null = null;
let activitySnapshot: { revision: number; recording: TestRecordingActivity | null; activities: Array<{
  task_id: string;
  meeting_id: string | null;
  kind: 'recording';
  status: 'ready' | 'failed';
}> } = { revision: 0, recording: activityRecording, activities: [] };
let recordingDuration: number | null = null;
let transcriptHistory: Array<Record<string, unknown>> = [];
const rehydrate = mock(async () => {});
const setStatus = mock(() => {});
const push = mock(() => {});
const refetchMeetings = mock(async () => {});
const setCurrentMeeting = mock(() => {});
const setIsMeetingActive = mock(() => {});
const clearTranscripts = mock(() => {});
const setMeetingTitle = mock(() => {});
const markMeetingAsSaved = mock(async () => {});
const flushBuffer = mock(() => {});
let flushNotesImpl: () => Promise<void> = async () => {};
const flushNotes = mock(() => flushNotesImpl());
let notesMarkdown = '';
let notesLoadError: Error | null = null;
const loadNotes = mock(async (target: unknown) => ({
  target,
  document: notesMarkdown ? {
    version: 1, meetingStartedAtMs: 1, updatedAt: '2026-09-19T00:00:00Z',
    notes: [], editorBlocks: [{ type: 'paragraph', content: [{ type: 'text', text: notesMarkdown }] }],
  } : null,
  loadState: notesLoadError ? 'error' : 'ready',
  saveState: 'saved', revision: 0, acknowledgedRevision: 0,
  loadError: notesLoadError, saveError: null,
}));

let startResult = deferred<{ session_id: string }>();
const startRecordingWithDevices = mock((
  _micDevice: string | null,
  _systemDevice: string | null,
  _title: string,
  _requestId?: string,
) => startResult.promise);
const stopRecording = mock(async () => {});
const pauseRecording = mock(async () => {});
const resumeRecording = mock(async () => {});
const createMeeting = mock(async () => ({ status: 'created', meeting_id: 'meeting-created' }));
const saveMeeting = mock(async (
  _title: string,
  _transcripts: unknown[],
  _folderPath: string | null,
  _webhookOnComplete?: boolean,
  _meetingId?: string | null,
) => ({ meeting_id: 'meeting-created' }));
const bindActiveRecordingMeeting = mock(async () => ({ revision: 1, recording: null, activities: [] }));
const claimRecordingRequest = mock(async (requestId: string) => ({
  request_id: requestId,
  source: 'tray',
  status: 'claimed' as const,
  claimed_by: 'main-recording-controller',
  started_session_id: null,
}));
const acknowledgeRecordingRequest = mock(async (
  _requestId: string,
  _accepted: boolean,
  _error?: string | null,
) => {});
const getActivitySnapshot = mock(async () => activitySnapshot);
const getRecordingState = mock(async () => ({
  is_recording: Boolean(activityRecording),
  is_paused: activityRecording?.status === 'paused',
  is_active: Boolean(activityRecording),
  recording_duration: recordingDuration,
  active_duration: recordingDuration,
  session_id: activityRecording?.session_id ?? null,
  active_meeting_id: activityRecording?.meeting_id ?? null,
}));
const getRecordingMeetingName = mock(async () => 'Recovered title');
const getTranscriptHistory = mock(async () => transcriptHistory);
let requestCallback: ((request: RecordingStartRequest) => void) | null = null;
let transcriptionErrorCallback: ((error: TranscriptionErrorPayload) => void) | null = null;
let requestListener = Promise.resolve(mock(() => {}));
const onRecordingStartRequested = mock(async (callback: (request: RecordingStartRequest) => void) => {
  requestCallback = callback;
  return requestListener;
});
const onRecordingStopped = mock(async () => mock(() => {}));
const onChunkDropWarning = mock(async () => mock(() => {}));
const onTranscriptionError = mock(async (callback: (error: TranscriptionErrorPayload) => void) => {
  transcriptionErrorCallback = callback;
  return mock(() => {});
});
const getPendingRecordingRequest = mock(async () => null);

const invoke = mock(async (command: string, _args?: Record<string, unknown>): Promise<unknown> => {
  switch (command) {
    case 'api_get_transcript_config': return { provider: 'test' };
    case 'initialize_test': return undefined;
    case 'has_test': return true;
    case 'set_live_transcription_enabled': return undefined;
    case 'get_meeting_folder_path': return '/recordings/live';
    default: return undefined;
  }
});
const listen = mock(async (
  _eventName: string,
  _callback: (event: { payload: unknown }) => void,
) => mock(() => {}));

mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen }));
mock.module('@tauri-apps/api/path', () => ({ ...originalPath, appDataDir: async () => '/data' }));
mock.module('next/navigation', () => ({ useRouter: () => ({ push }) }));
mock.module('../../src/contexts/ConfigContext', () => ({
  useConfig: () => ({
    selectedDevices: { micDevice: 'mic', systemDevice: 'system' },
    betaFeatures: { liveTranscription: true },
    selectedLanguage: 'auto',
    transcriptModelConfig: { model: 'model', provider: 'test' },
  }),
}));
mock.module('../../src/contexts/MeetingActivityContext', () => ({
  useMeetingActivity: () => ({
    recording: activityRecording,
    activeMeetingId: activityRecording?.meeting_id ?? null,
    snapshot: activitySnapshot,
    rehydrate,
  }),
}));
mock.module('../../src/contexts/RecordingStateContext', () => ({
  RecordingStatus: {
    IDLE: 'idle', STARTING: 'starting', STOPPING: 'stopping',
    PROCESSING_TRANSCRIPTS: 'processing', SAVING: 'saving', ERROR: 'error',
  },
  useRecordingState: () => ({ setStatus, recordingDuration }),
}));
mock.module('../../src/contexts/TranscriptContext', () => ({
  useTranscripts: () => ({
    transcriptsRef: { current: [] }, flushBuffer, clearTranscripts,
    setMeetingTitle, markMeetingAsSaved,
  }),
}));
mock.module('../../src/components/Sidebar/SidebarProvider', () => ({
  useSidebar: () => ({ setCurrentMeeting, setIsMeetingActive, refetchMeetings }),
}));
mock.module('../../src/lib/debugMode', () => ({ cachedDebugMode: () => false }));
mock.module('../../src/lib/analytics', () => ({
  default: {
    trackButtonClick: mock(() => {}),
    getMeetingsCountToday: mock(async () => 0),
    trackMeetingCompleted: mock(async () => {}),
    updateMeetingCount: mock(async () => {}),
  },
}));
mock.module('../../src/lib/recordingNotification', () => ({ showRecordingNotification: mock(async () => {}) }));
mock.module('../../src/lib/transcription-model-readiness', () => ({
  getProviderCommands: () => ({ initialize: 'initialize_test', hasAvailableModels: 'has_test', getAvailableModels: 'models_test' }),
  hasDownloadingModel: () => false,
}));
mock.module('../../src/lib/autoSummary', () => ({ markDeferredMeetingForAutoSummary: () => {} }));
mock.module('../../src/lib/summary-language-preferences', () => ({
  applyPinnedSummaryLanguageToMeeting: async () => true,
  detectAndCacheSummaryLanguage: async () => {},
}));
mock.module('../../src/services/notePersistenceService', () => ({
  flushNotes,
  meetingNotesTarget: (meetingId: string) => ({ kind: 'meeting', meetingId }),
  notePersistenceService: { loadNotes, saveNotes: mock(() => 1) },
}));
mock.module('../../src/lib/meetingExport', () => ({ originalNotesMarkdown: () => notesMarkdown }));
mock.module('../../src/services/meetingActivityService', () => ({
  meetingActivityService: {
    bindActiveRecordingMeeting,
    claimRecordingRequest,
    acknowledgeRecordingRequest,
    onRecordingStartRequested,
    getPendingRecordingRequest,
    getSnapshot: getActivitySnapshot,
  },
}));
mock.module('../../src/services/recordingService', () => ({
  recordingService: {
    startRecordingWithDevices,
    stopRecording,
    pauseRecording,
    resumeRecording,
    onRecordingStopped,
    onChunkDropWarning,
    getRecordingState,
    getRecordingMeetingName,
  },
}));
mock.module('../../src/services/storageService', () => ({ storageService: { createMeeting, saveMeeting } }));
mock.module('../../src/services/transcriptService', () => ({
  transcriptService: {
    onTranscriptionError,
    getTranscriptHistory,
    onTranscriptionComplete: async () => mock(() => {}),
    getTranscriptionStatus: async () => ({ is_processing: false, chunks_in_queue: 0 }),
  },
}));
mock.module('../../src/services/indexedDBService', () => ({
  indexedDBService: { deleteOldMeetings: async () => 0, deleteSavedMeetings: async () => 0 },
}));
const recovery = {
  recoverableMeetings: [], isLoading: false, isRecovering: false,
  checkForRecoverableTranscripts: mock(async () => {}),
  recoverMeeting: mock(async () => ({ success: true, transcriptCount: 0 })),
  loadMeetingTranscripts: mock(async () => []),
  deleteRecoverableMeeting: mock(async () => {}),
};
mock.module('../../src/hooks/useTranscriptRecovery', () => ({ useTranscriptRecovery: () => recovery }));

const { RecordingControllerProvider, useRecordingController } = await import('../../src/contexts/RecordingControllerContext');
const { RecordingControllerFeedback } = await import('../../src/components/RecordingControllerFeedback');
type Controller = ReturnType<typeof useRecordingController>;
let controller: Controller;
let renderer: ReactTestRenderer | undefined;

function Probe() {
  controller = useRecordingController();
  return <output>{controller.feedback?.title ?? controller.command ?? 'idle'}</output>;
}

async function mount(children: ReactNode = <Probe />) {
  await act(async () => {
    renderer = create(<RecordingControllerProvider>{children}</RecordingControllerProvider>);
  });
}

beforeEach(() => {
  storage.clear();
  storage.set('liveTranscriptEnabled', 'true');
  activityRecording = null;
  activitySnapshot = { revision: 0, recording: null, activities: [] };
  recordingDuration = null;
  transcriptHistory = [];
  notesMarkdown = '';
  notesLoadError = null;
  startResult = deferred();
  requestListener = Promise.resolve(mock(() => {}));
  requestCallback = null;
  transcriptionErrorCallback = null;
  flushNotesImpl = async () => {};
  [
    invoke, listen, rehydrate, setStatus, push, refetchMeetings, setCurrentMeeting,
    setIsMeetingActive, clearTranscripts, setMeetingTitle, markMeetingAsSaved,
    flushBuffer, flushNotes, startRecordingWithDevices, stopRecording,
    pauseRecording, resumeRecording, createMeeting, saveMeeting,
    bindActiveRecordingMeeting, claimRecordingRequest, acknowledgeRecordingRequest,
    onRecordingStartRequested, onRecordingStopped, onChunkDropWarning,
    onTranscriptionError, getPendingRecordingRequest, getActivitySnapshot,
    getRecordingState, getRecordingMeetingName, getTranscriptHistory, loadNotes,
  ].forEach((fn) => fn.mockClear());
});

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

describe('RecordingControllerProvider', () => {
  test('shares a synchronous start lock and binds the exact returned native session', async () => {
    await mount();
    let first!: Promise<void>;
    let second!: Promise<void>;
    act(() => {
      first = controller.startRecording({ source: 'home' });
      second = controller.startRecording({ source: 'palette' });
    });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(first).toBe(second);
    expect(startRecordingWithDevices).toHaveBeenCalledTimes(1);
    startResult.resolve({ session_id: 'native-session-exact' });
    await act(async () => { await first; });

    expect(bindActiveRecordingMeeting).toHaveBeenCalledWith('native-session-exact', 'meeting-created');
    expect(push).toHaveBeenCalledWith('/meeting-details?id=meeting-created');
  });

  test('claims a pending native request and relies on the start command auto-ack', async () => {
    await mount();
    await act(async () => {
      requestCallback?.({
        request_id: 'prompt-request', source: 'meeting_prompt', status: 'pending',
        claimed_by: null, started_session_id: null,
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    startResult.resolve({ session_id: 'prompt-session' });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(claimRecordingRequest).toHaveBeenCalledWith('prompt-request', 'main-recording-controller');
    expect(startRecordingWithDevices.mock.calls[0]?.[3]).toBe('prompt-request');
    expect(acknowledgeRecordingRequest).not.toHaveBeenCalled();
  });

  test('negative-acknowledges a prompt once when preflight fails before native start', async () => {
    invoke.mockImplementationOnce(async () => { throw new Error('configuration unavailable'); });
    await mount();

    await act(async () => {
      requestCallback?.({
        request_id: 'preflight-request', source: 'tray', status: 'pending',
        claimed_by: null, started_session_id: null,
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(startRecordingWithDevices).not.toHaveBeenCalled();
    expect(acknowledgeRecordingRequest).toHaveBeenCalledTimes(1);
    expect(acknowledgeRecordingRequest.mock.calls[0]?.slice(0, 2)).toEqual(['preflight-request', false]);
  });

  test('serializes stop and pause through one synchronous lifecycle lock', async () => {
    activityRecording = {
      session_id: 'locked-session', meeting_id: 'meeting-active', status: 'recording', error: null,
    };
    recordingDuration = 30;
    transcriptHistory = [{
      id: 'final', text: 'final words', display_time: '00:30', sequence_id: 1,
      audio_start_time: 28, audio_end_time: 30, duration: 2,
    }];
    const stopped = deferred<void>();
    stopRecording.mockImplementationOnce(() => stopped.promise);
    await mount();

    let stop!: Promise<void>;
    let pause!: Promise<void>;
    act(() => {
      stop = controller.stopRecording();
      pause = controller.pauseRecording();
    });
    expect(stop).not.toBe(pause);
    await expect(pause).rejects.toThrow('Cannot pause while stop is in progress');
    expect(pauseRecording).not.toHaveBeenCalled();
    stopped.resolve();
    await act(async () => { await stop; });

    expect(stopRecording).toHaveBeenCalledTimes(1);
  });

  test('reads authoritative native history after stop before saving final segments', async () => {
    activityRecording = {
      session_id: 'history-session', meeting_id: 'meeting-history', status: 'recording', error: null,
    };
    recordingDuration = 45;
    transcriptHistory = [{
      id: 'last-segment', text: 'captured after the UI buffer', display_time: '12:00:45',
      speaker: 'mic', sequence_id: 9, audio_start_time: 43, audio_end_time: 45, duration: 2,
    }];
    await mount();

    await act(async () => { await controller.stopRecording(); });

    expect(getTranscriptHistory).toHaveBeenCalledTimes(1);
    expect(saveMeeting.mock.calls[0]?.[1]).toEqual([expect.objectContaining({
      id: 'last-segment', text: 'captured after the UI buffer', sequence_id: 9, is_partial: false,
    })]);
  });

  test('retries persistence without stopping or saving a completed phase twice', async () => {
    activityRecording = {
      session_id: 'retry-session', meeting_id: 'meeting-retry', status: 'recording', error: null,
    };
    recordingDuration = 30;
    transcriptHistory = [{ id: 'one', text: 'kept', display_time: '00:01', audio_end_time: 1 }];
    saveMeeting.mockImplementationOnce(async () => { throw new Error('database busy'); });
    await mount();

    await act(async () => { await controller.stopRecording().catch(() => {}); });
    expect(controller.feedback?.title).toBe('Meeting needs attention');
    await act(async () => { await controller.retryFeedback(); });

    expect(stopRecording).toHaveBeenCalledTimes(1);
    expect(getTranscriptHistory).toHaveBeenCalledTimes(1);
    expect(saveMeeting).toHaveBeenCalledTimes(2);
  });

  test('does not repeat a successful save when a later catalog refresh is retried', async () => {
    activityRecording = {
      session_id: 'catalog-retry-session', meeting_id: 'meeting-catalog-retry', status: 'recording', error: null,
    };
    recordingDuration = 30;
    transcriptHistory = [{ id: 'one', text: 'saved once', display_time: '00:01' }];
    refetchMeetings.mockImplementationOnce(async () => { throw new Error('catalog unavailable'); });
    await mount();

    await act(async () => { await controller.stopRecording().catch(() => {}); });
    expect(controller.feedback?.title).toBe('Meeting needs attention');
    await act(async () => { await controller.retryFeedback(); });

    expect(stopRecording).toHaveBeenCalledTimes(1);
    expect(saveMeeting).toHaveBeenCalledTimes(1);
    expect(markMeetingAsSaved).toHaveBeenCalledTimes(1);
  });

  test('finalizes after a rejected stop when native state confirms capture ended', async () => {
    activityRecording = {
      session_id: 'failed-stop-session', meeting_id: 'meeting-failed-stop', status: 'recording', error: null,
    };
    recordingDuration = 30;
    transcriptHistory = [{ id: 'one', text: 'preserved', display_time: '00:01' }];
    stopRecording.mockImplementationOnce(async () => {
      activityRecording = null;
      throw new Error('audio file was partial');
    });
    await mount();

    await act(async () => { await controller.stopRecording().catch(() => {}); });

    expect(saveMeeting).toHaveBeenCalledTimes(1);
    expect(controller.feedback?.title).toBe('Recording stopped with an error');
    expect(controller.canRetryFeedback).toBe(false);
  });

  test('does not classify unknown-duration tray completion as short and empty', async () => {
    sessionStorage.setItem('active_recording_sessions', JSON.stringify([{
      sessionId: 'tray-session', meetingId: 'meeting-tray', title: 'Tray meeting',
      folderPath: '/recordings/tray', recordingSeconds: null,
    }]));
    activitySnapshot = {
      revision: 2,
      recording: null,
      activities: [{
        task_id: 'tray-session', meeting_id: 'meeting-tray', kind: 'recording', status: 'ready',
      }],
    };
    await mount();
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(saveMeeting).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls.some((call) => call[0] === 'api_discard_meeting')).toBe(false);

    activitySnapshot = { ...activitySnapshot, revision: 3, activities: [...activitySnapshot.activities] };
    await act(async () => {
      renderer!.update(<RecordingControllerProvider><Probe /></RecordingControllerProvider>);
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(saveMeeting).toHaveBeenCalledTimes(1);
  });

  test('keeps structured notes and refuses destructive cleanup when note loading fails', async () => {
    activityRecording = {
      session_id: 'notes-session', meeting_id: 'meeting-notes', status: 'recording', error: null,
    };
    recordingDuration = 2;
    notesMarkdown = 'A structured editor note';
    await mount();
    await act(async () => { await controller.stopRecording(); });
    expect(invoke.mock.calls.some((call) => call[0] === 'api_discard_meeting')).toBe(false);

    await act(async () => renderer!.unmount());
    renderer = undefined;
    storage.clear();
    activityRecording = {
      session_id: 'notes-error-session', meeting_id: 'meeting-notes-error', status: 'recording', error: null,
    };
    notesMarkdown = '';
    notesLoadError = new Error('notes database unavailable');
    await mount();
    await act(async () => { await controller.stopRecording().catch(() => {}); });
    expect(controller.feedback?.title).toBe('Meeting needs attention');
    expect(invoke.mock.calls.some((call) => (
      call[0] === 'api_discard_meeting'
      && (call[1] as { meetingId?: string } | undefined)?.meetingId === 'meeting-notes-error'
    ))).toBe(false);
  });

  test('ignores an old terminal activity after a new session has started', async () => {
    sessionStorage.setItem('active_recording_sessions', JSON.stringify([
      {
        sessionId: 'old-session', meetingId: 'old-meeting', title: 'Old meeting',
        folderPath: '/recordings/old', recordingSeconds: 20,
      },
      {
        sessionId: 'new-session', meetingId: 'new-meeting', title: 'New meeting',
        folderPath: '/recordings/new', recordingSeconds: 1,
      },
    ]));
    activityRecording = {
      session_id: 'new-session', meeting_id: 'new-meeting', status: 'recording', error: null,
    };
    activitySnapshot = {
      revision: 4,
      recording: activityRecording,
      activities: [{ task_id: 'old-session', meeting_id: 'old-meeting', kind: 'recording', status: 'ready' }],
    };
    transcriptHistory = [{ id: 'old-transcript', text: 'old content', display_time: '00:20' }];
    await mount();
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(saveMeeting).not.toHaveBeenCalled();
    expect(invoke.mock.calls.some((call) => call[0] === 'attach_live_notes')).toBe(false);
    expect(clearTranscripts).not.toHaveBeenCalled();
  });

  test('retains the created meeting identity across a bind failure and remount', async () => {
    bindActiveRecordingMeeting.mockImplementationOnce(async () => { throw new Error('bind failed'); });
    await mount();
    const firstStart = controller.startRecording();
    startResult.resolve({ session_id: 'reload-session' });
    await act(async () => { await firstStart; });
    await act(async () => renderer!.unmount());
    renderer = undefined;

    activityRecording = {
      session_id: 'reload-session', meeting_id: null, status: 'recording', error: null,
    };
    bindActiveRecordingMeeting.mockClear();
    await mount();
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(bindActiveRecordingMeeting).toHaveBeenCalledWith('reload-session', 'meeting-created');
    expect(createMeeting).toHaveBeenCalledTimes(1);
  });

  test('does not call native stop when notes cannot flush and retains active truth', async () => {
    activityRecording = {
      session_id: 'active-session', meeting_id: 'meeting-active', status: 'recording', error: null,
    };
    flushNotesImpl = async () => { throw new Error('disk full'); };
    await mount();

    await act(async () => {
      await controller.stopRecording().catch(() => {});
    });

    expect(stopRecording).not.toHaveBeenCalled();
    expect(activityRecording.status).toBe('recording');
    expect(controller.feedback?.title).toBe('Recording is still active');
  });

  test('keeps pause rejection scoped to command feedback without changing native state', async () => {
    activityRecording = {
      session_id: 'active-session', meeting_id: 'meeting-active', status: 'recording', error: null,
    };
    pauseRecording.mockImplementationOnce(async () => { throw new Error('device busy'); });
    await mount(<><Probe /><RecordingControllerFeedback /></>);

    await act(async () => { await controller.pauseRecording().catch(() => {}); });

    expect(activityRecording.status).toBe('recording');
    expect(controller.feedback?.title).toBe('Recording could not be paused');
    const actionLabels = renderer!.root.findAllByType('button').map((button) => button.props.children);
    expect(actionLabels).toContain('Retry');
    expect(actionLabels).toContain('Open settings');
  });

  test('keeps resume rejection scoped to the paused session', async () => {
    activityRecording = {
      session_id: 'paused-session', meeting_id: 'meeting-active', status: 'paused', error: null,
    };
    resumeRecording.mockImplementationOnce(async () => { throw new Error('resume unavailable'); });
    await mount();

    await act(async () => { await controller.resumeRecording().catch(() => {}); });

    expect(activityRecording.status).toBe('paused');
    expect(controller.feedback?.title).toBe('Recording could not be resumed');
  });

  test('surfaces actionable transcription errors when no route-owned Home UI exists', async () => {
    await mount();
    await act(async () => {
      transcriptionErrorCallback?.({
        phase: 'startup', actionable: true, error: 'missing', userMessage: 'Install a model',
      });
    });

    expect(controller.feedback).toMatchObject({
      title: 'Transcription needs attention',
      message: 'Install a model',
      settingsSection: 'transcription',
    });
  });

  test('disposes an asynchronously registered request listener after unmount', async () => {
    const late = deferred<ReturnType<typeof mock>>();
    const unlisten = mock(() => {});
    requestListener = late.promise;
    await mount();
    await act(async () => renderer!.unmount());
    renderer = undefined;
    await act(async () => { late.resolve(unlisten); await late.promise; });

    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  test('registers unrelated listeners when request listener setup fails', async () => {
    requestListener = Promise.reject(new Error('request listener unavailable'));
    await mount();
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(onRecordingStopped).toHaveBeenCalledTimes(1);
    expect(onTranscriptionError).toHaveBeenCalledTimes(1);
    expect(onChunkDropWarning).toHaveBeenCalledTimes(1);
    expect(listen.mock.calls.some((call) => call[0] === 'recording-stop-complete')).toBe(true);
  });
});
