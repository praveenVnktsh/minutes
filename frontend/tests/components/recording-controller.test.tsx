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

let activityRecording: {
  session_id: string;
  meeting_id: string | null;
  status: 'recording' | 'paused';
  error: null;
} | null = null;
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
const saveMeeting = mock(async () => ({ meeting_id: 'meeting-created' }));
const bindActiveRecordingMeeting = mock(async () => ({ revision: 1, recording: null, activities: [] }));
const claimRecordingRequest = mock(async (requestId: string) => ({
  request_id: requestId,
  source: 'tray',
  status: 'claimed' as const,
  claimed_by: 'main-recording-controller',
  started_session_id: null,
}));
const acknowledgeRecordingRequest = mock(async () => {});
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

const invoke = mock(async (command: string): Promise<unknown> => {
  switch (command) {
    case 'api_get_transcript_config': return { provider: 'test' };
    case 'initialize_test': return undefined;
    case 'has_test': return true;
    case 'set_live_transcription_enabled': return undefined;
    case 'get_meeting_folder_path': return '/recordings/live';
    default: return undefined;
  }
});
const listen = mock(async () => mock(() => {}));

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
    rehydrate,
  }),
}));
mock.module('../../src/contexts/RecordingStateContext', () => ({
  RecordingStatus: {
    IDLE: 'idle', STARTING: 'starting', STOPPING: 'stopping',
    PROCESSING_TRANSCRIPTS: 'processing', SAVING: 'saving', ERROR: 'error',
  },
  useRecordingState: () => ({ setStatus }),
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
mock.module('../../src/services/notePersistenceService', () => ({ flushNotes }));
mock.module('../../src/services/meetingActivityService', () => ({
  meetingActivityService: {
    bindActiveRecordingMeeting,
    claimRecordingRequest,
    acknowledgeRecordingRequest,
    onRecordingStartRequested,
    getPendingRecordingRequest,
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
  },
}));
mock.module('../../src/services/storageService', () => ({ storageService: { createMeeting, saveMeeting } }));
mock.module('../../src/services/transcriptService', () => ({
  transcriptService: {
    onTranscriptionError,
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
    onTranscriptionError, getPendingRecordingRequest,
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
    await mount();

    await act(async () => { await controller.pauseRecording().catch(() => {}); });

    expect(activityRecording.status).toBe('recording');
    expect(controller.feedback?.title).toBe('Recording could not be paused');
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
});
