import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { useEffect, useState } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { parseSummaryContent } from '../../src/lib/summary-content';
import type { MeetingSummary, SummaryProcessResponse } from '../../src/types';

// Bun shares module mocks between test files; restore EVERY module this suite
// mocks, not only the application ones. Test files run in directory order, which
// differs between macOS and Linux: on Linux this file runs first, so a mock left
// in place here reaches every file after it. The partial next/navigation mock
// below has no useSearchParams, which crashed meeting-details-refresh on import,
// and the fake invoke broke summary-language-preferences.
const originalCore = { ...await import('@tauri-apps/api/core') };
const originalAnalytics = { ...await import('../../src/lib/analytics') };
const originalPreferences = { ...await import('../../src/lib/summary-language-preferences') };
const originalToast = { ...await import('sonner') };
const originalNavigation = { ...await import('next/navigation') };
const originalRecordingState = { ...await import('../../src/contexts/RecordingStateContext') };
const originalConfig = { ...await import('../../src/contexts/ConfigContext') };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('../../src/lib/analytics', () => originalAnalytics);
  mock.module('../../src/lib/summary-language-preferences', () => originalPreferences);
  mock.module('sonner', () => originalToast);
  mock.module('next/navigation', () => originalNavigation);
  mock.module('../../src/contexts/RecordingStateContext', () => originalRecordingState);
  mock.module('../../src/contexts/ConfigContext', () => originalConfig);
});

mock.module('next/navigation', () => ({ usePathname: () => '/meeting-details', useRouter: () => ({}) }));
mock.module('../../src/contexts/RecordingStateContext', () => ({ useRecordingState: () => ({ isRecording: false }) }));
mock.module('../../src/contexts/ConfigContext', () => ({
  useConfig: () => ({
    modelConfig: { provider: 'ollama', model: configuredModel, whisperModel: 'base' },
    isModelConfigLoading: false,
    isModelConfigSaving: configSaving,
    modelConfigSaveError: configError,
  }),
}));
const notify = mock(() => {});
mock.module('sonner', () => ({ toast: { info: notify, error: notify, success: notify, warning: notify } }));
const trackCompletion = mock(async (..._args: Parameters<typeof originalAnalytics.default.trackSummaryGenerationCompleted>) => {});
mock.module('../../src/lib/analytics', () => ({ default: {
  trackBackendConnection() {}, trackSummaryGenerationStarted: async () => {},
  trackSummaryGenerationCompleted: trackCompletion,
} }));
let startProcess: () => Promise<{ process_id: string }>;
let getSummary: (meetingId: string) => Promise<SummaryProcessResponse>;
let cancelProcess: () => Promise<{ cancelled: boolean }>;
const invoke = mock(async (command: string, args?: Record<string, unknown>): Promise<unknown> => {
  if (command === 'api_get_meetings') return [];
  if (command === 'api_get_summary') return getSummary(args!.meetingId as string);
  if (command === 'get_meeting_live_notes') return null;
  if (command === 'api_get_meeting_transcripts') return { transcripts: [{ text: 'Meeting transcript', timestamp: '00:00' }], total_count: 1 };
  if (command === 'get_ollama_models') return [{ name: 'test' }];
  if (command === 'api_process_transcript') return startProcess();
  if (command === 'api_cancel_summary') return cancelProcess();
  throw new Error(`Unexpected command: ${command}`);
});
mock.module('@tauri-apps/api/core', () => ({ invoke }));
mock.module('../../src/lib/summary-language-preferences', () => ({
  readCachedDetectedSummaryLanguage: async () => null,
  detectAndCacheSummaryLanguage: async () => ({ language: 'en' }),
  readMeetingSummaryLanguage: async () => ({ language: 'en', storage: 'metadata' }),
}));
const { SidebarProvider, useSidebar } = await import('../../src/components/Sidebar/SidebarProvider');
const { meetingActivityStore } = await import('../../src/contexts/MeetingActivityContext');
const { useSummaryGeneration } = await import('../../src/hooks/meeting-details/useSummaryGeneration');

const response = (overrides: Partial<SummaryProcessResponse> = {}): SummaryProcessResponse => ({
  meeting_id: 'meeting-a', status: 'pending', start: 'attempt-a', end: null,
  data: null, error: null, meetingName: 'Meeting A', ...overrides,
});
let configuredModel = 'test';
let configSaving = false;
let configError: Error | null = null;
let state: ReturnType<typeof useSummaryGeneration>;
function Status({ initialSummary, meetingId = 'meeting-a' }: {
  initialSummary: SummaryProcessResponse; meetingId?: string;
}) {
  const [summary, setAiSummary] = useState<MeetingSummary | null>(() => parseSummaryContent(initialSummary.data));
  const [title, updateMeetingTitle] = useState('Original title');
  state = useSummaryGeneration({
    meeting: { id: meetingId, created_at: '2026-09-01T00:00:00Z' },
    transcripts: [], modelConfig: { provider: 'ollama', model: configuredModel, whisperModel: 'base' },
    isModelConfigLoading: false, selectedTemplate: 'daily_standup',
    setAiSummary, updateMeetingTitle, initialSummary,
  });
  return <output>{state.summaryStatus}:{state.summaryError}:{title}:{JSON.stringify(summary)}</output>;
}

let renderer: ReactTestRenderer;
const timers = new Map<number, () => Promise<void>>();
let nextTimer = 0;
const realSetInterval = globalThis.setInterval;
const realClearInterval = globalThis.clearInterval;
beforeEach(() => {
  renderer = undefined as unknown as ReactTestRenderer;
  configuredModel = 'test';
  configSaving = false;
  configError = null;
  timers.clear(); notify.mockClear(); trackCompletion.mockClear(); invoke.mockClear();
  getSummary = async () => response();
  startProcess = async () => ({ process_id: 'attempt-a' });
  cancelProcess = async () => ({ cancelled: true });
  globalThis.setInterval = ((callback: () => Promise<void>) => {
    const id = ++nextTimer; timers.set(id, callback); return id;
  }) as typeof setInterval;
  globalThis.clearInterval = ((id: number) => { timers.delete(id); }) as typeof clearInterval;
});
afterEach(async () => {
  if (renderer) await act(async () => renderer.unmount());
  meetingActivityStore.stopSummaryPolling('meeting-a');
  meetingActivityStore.stopSummaryPolling('meeting-b');
  for (const activity of meetingActivityStore.getState().summaries) {
    if (activity.processId) {
      meetingActivityStore.hydrateSummary(response({
        meeting_id: activity.meetingId,
        start: activity.processId,
        status: 'cancelled',
        data: activity.response?.data ?? null,
      }));
    }
  }
  meetingActivityStore.dismissSummary('meeting-a');
  meetingActivityStore.dismissSummary('meeting-b');
  globalThis.setInterval = realSetInterval;
  globalThis.clearInterval = realClearInterval;
});
async function show(initialSummary: SummaryProcessResponse | null, meetingId = 'meeting-a') {
  await act(async () => {
    const view = <SidebarProvider>{initialSummary && <Status key={meetingId} initialSummary={initialSummary} meetingId={meetingId} />}</SidebarProvider>;
    if (renderer) renderer.update(view); else renderer = create(view);
  });
}
const text = () => JSON.stringify(renderer.toJSON());
async function tick() {
  await act(async () => { for (const callback of [...timers.values()]) callback(); });
}

describe('summary state restored when returning to a meeting', () => {
  test('repeated recovery read failures end regeneration and retain visible notes', async () => {
    await show(response({ data: { markdown: 'Previous summary' } }));
    getSummary = async () => { throw new Error('Database unavailable'); };
    await tick();
    expect(state.summaryStatus).toBe('regenerating');
    expect(meetingActivityStore.getState().summaries[0]?.reconciliationError).toContain('Database unavailable');
    expect(text()).toContain('Previous summary');
    expect(timers.size).toBe(1);
  });

  test('failed cancellation recovery ends loading without discarding visible notes', async () => {
    await show(response({ data: { markdown: 'Previous summary' } }));
    let reads = 0;
    getSummary = async () => {
      if (++reads === 1) return response({ status: 'cancelled' });
      throw new Error('Database unavailable');
    };
    await tick();
    expect(state.summaryStatus).toBe('error');
    expect(text()).toContain('Previous summary');
    expect(timers.size).toBe(0);
  });

  test.each(['read', 'callback'])('polling safely handles a %s failure and a rejecting consumer', async (failure) => {
    function RejectingConsumer() {
      const { startSummaryPolling } = useSidebar();
      useEffect(() => {
        startSummaryPolling('meeting-a', 'attempt-a', async () => {
          throw new Error('Consumer failed');
        });
      }, [startSummaryPolling]);
      return null;
    }
    await act(async () => {
      renderer = create(<SidebarProvider><RejectingConsumer /></SidebarProvider>);
    });
    getSummary = async () => {
      if (failure === 'read') throw new Error('Database unavailable');
      return response({ status: 'completed', data: { markdown: 'Finished summary' } });
    };
    await tick();
    expect(timers.size).toBe(failure === 'read' ? 1 : 0);
  });

  test('resumes pending generation after leaving and returning, then displays completion', async () => {
    await show(response());
    expect(text()).toContain('processing');
    await show(null);
    expect(timers.size).toBe(1);
    await show(response());
    expect(text()).toContain('processing');
    getSummary = async () => response({ status: 'completed', data: { markdown: 'Finished summary' }, meetingName: 'New title' });
    await tick();
    expect(text()).toContain('completed');
    expect(text()).toContain('Finished summary');
    expect(text()).toContain('New title');
    expect(timers.size).toBe(0);
  });

  test('keeps regeneration loading with old content and restores content on failure', async () => {
    await show(response({ data: { markdown: 'Previous summary' } }));
    expect(text()).toContain('regenerating');
    getSummary = async () => response({ status: 'failed', error: 'Model unavailable', data: { markdown: 'Previous summary' } });
    await tick();
    expect(text()).toContain('completed');
    expect(text()).toContain('Previous summary');
  });

  test('shows a failure that happened while the meeting was closed', async () => {
    await show(response({ status: 'failed', error: 'Model unavailable' }));
    expect(text()).toContain('error');
    expect(text()).toContain('Model unavailable');
    expect(timers.size).toBe(0);
  });

  test('keeps restored notes and exposes a regeneration failure that happened while away', async () => {
    await show(response({ status: 'failed', error: 'Model unavailable', data: { markdown: 'Previous summary' } }));
    expect(state.summaryStatus).toBe('error');
    expect(text()).toContain('Model unavailable');
    expect(text()).toContain('Previous summary');
    expect(notify).not.toHaveBeenCalled();
  });

  test('ordinary rerenders keep the existing poll attached', async () => {
    const initial = response();
    await show(initial);
    const timer = [...timers.keys()][0];
    await show(initial);
    expect([...timers.keys()]).toEqual([timer]);
    getSummary = async () => response({ status: 'completed', data: { markdown: 'Finished summary' } });
    await tick();
    expect(state.summaryStatus).toBe('completed');
  });

  test('hydrates completed and cancelled records without replaying notifications or analytics', async () => {
    await show(response({ status: 'completed', data: { markdown: 'Finished while away' } }));
    expect(state.summaryStatus).toBe('completed');
    expect(text()).toContain('Finished while away');
    await show(null);
    await show(response({ status: 'cancelled', data: { markdown: 'Restored summary' } }));
    expect(state.summaryStatus).toBe('completed');
    expect(text()).toContain('Restored summary');
    expect(timers.size).toBe(0);
    expect(notify).not.toHaveBeenCalled();
    expect(trackCompletion).not.toHaveBeenCalled();
  });

  test('ignores a response belonging to another meeting', async () => {
    await show(response({ meeting_id: 'meeting-b' }));
    expect(state.summaryStatus).toBe('idle');
    expect(timers.size).toBe(0);
  });

  test('Stop uses the restored process token', async () => {
    await show(response());
    await act(async () => state.handleStopGeneration());
    expect(invoke.mock.calls).toContainEqual(['api_cancel_summary', { meetingId: 'meeting-a', processId: 'attempt-a' }]);
    expect(state.summaryStatus).toBe('idle');
    expect(timers.size).toBe(0);
  });

  test('a late completion from meeting A cannot change meeting B', async () => {
    let resolve!: (value: SummaryProcessResponse) => void;
    getSummary = () => new Promise(done => { resolve = done; });
    await show(response());
    await tick();
    await show(response({ meeting_id: 'meeting-b', status: 'idle', start: null }), 'meeting-b');
    await act(async () => resolve(response({ status: 'completed', data: { markdown: 'Wrong meeting' } })));
    expect(state.summaryStatus).toBe('idle');
    expect(text()).not.toContain('Wrong meeting');
    expect(notify).not.toHaveBeenCalled();
  });

  test('leaving before the start response arrives keeps the backend job running for a later visit', async () => {
    let resolve!: (value: { process_id: string }) => void;
    startProcess = () => new Promise(done => { resolve = done; });
    getSummary = async () => response({ status: 'idle', start: null });
    await show(response({ status: 'idle', start: null }));
    let generation!: Promise<void>;
    await act(async () => { generation = state.handleGenerateSummary(); });
    await show(null);
    await act(async () => { resolve({ process_id: 'attempt-a' }); await generation; });
    expect(invoke.mock.calls.filter(([command]) => command === 'api_cancel_summary')).toEqual([]);
    expect(timers.size).toBe(1);
    await show(response());
    expect(state.summaryStatus).toBe('processing');
    getSummary = async () => response({ status: 'completed', data: { markdown: 'Finished summary' } });
    await tick();
    expect(state.summaryStatus).toBe('completed');
  });

  test('Stop during native start cancels the exact process after its token resolves', async () => {
    const processId = '2026-09-18T10:00:00.123456789Z';
    let resolve!: (value: { process_id: string }) => void;
    startProcess = () => new Promise(done => { resolve = done; });
    getSummary = async () => response({ status: 'idle', start: null });
    await show(response({ status: 'idle', start: null }));
    let generation!: Promise<void>;
    await act(async () => { generation = state.handleGenerateSummary(); });
    let stopping!: Promise<void>;
    await act(async () => { stopping = state.handleStopGeneration(); });
    expect(state.summaryStatus).toBe('processing');

    await act(async () => {
      resolve({ process_id: processId });
      await Promise.all([generation, stopping]);
    });
    expect(invoke.mock.calls).toContainEqual([
      'api_cancel_summary',
      { meetingId: 'meeting-a', processId },
    ]);
    expect(timers.size).toBe(0);
  });

  test('remounts a pending regeneration with Stop attached to the delayed native start', async () => {
    const initial = response({ status: 'completed', data: { markdown: 'Previous summary' } });
    const processId = '2026-09-18T10:00:00.223456789Z';
    let resolve!: (value: { process_id: string }) => void;
    startProcess = () => new Promise(done => { resolve = done; });
    getSummary = async () => initial;
    await show(initial);
    let generation!: Promise<void>;
    await act(async () => {
      generation = state.handleRegenerateSummary();
      await new Promise((done) => setTimeout(done, 0));
    });
    await show(null);
    await show(initial);
    expect(state.summaryStatus).toBe('regenerating');

    let stopping!: Promise<void>;
    await act(async () => { stopping = state.handleStopGeneration(); });
    await act(async () => {
      resolve({ process_id: processId });
      await Promise.all([generation, stopping]);
    });
    expect(invoke.mock.calls).toContainEqual([
      'api_cancel_summary',
      { meetingId: 'meeting-a', processId },
    ]);
  });

  test('remounts with Stop while the owner pre-start summary read is still pending', async () => {
    const initial = response({ status: 'completed', data: { markdown: 'Previous summary' } });
    const processId = '2026-09-18T10:00:00.323456789Z';
    let resolveRead!: (value: SummaryProcessResponse) => void;
    let firstRead = true;
    getSummary = () => {
      if (firstRead) {
        firstRead = false;
        return new Promise((resolve) => { resolveRead = resolve; });
      }
      return Promise.resolve(response({ start: processId, status: 'cancelled', data: { markdown: 'Previous summary' } }));
    };
    startProcess = async () => ({ process_id: processId });
    await show(initial);
    let generation!: Promise<void>;
    await act(async () => {
      generation = state.handleRegenerateSummary();
      await new Promise((done) => setTimeout(done, 0));
    });
    expect(meetingActivityStore.getState().summaries.at(-1)).toMatchObject({ processId: null, status: 'queued' });
    await show(null);
    await show(initial);
    expect(state.summaryStatus).toBe('regenerating');

    let stopping!: Promise<void>;
    await act(async () => { stopping = state.handleStopGeneration(); });
    await act(async () => {
      resolveRead(initial);
      await Promise.all([generation, stopping]);
    });
    expect(invoke.mock.calls).toContainEqual([
      'api_cancel_summary',
      { meetingId: 'meeting-a', processId },
    ]);
  });

  test.each(['false', 'error'])('keeps observing when pending-start cancellation returns %s', async (outcome) => {
    const processId = '2026-09-18T10:00:00.123456789Z';
    let resolve!: (value: { process_id: string }) => void;
    startProcess = () => new Promise(done => { resolve = done; });
    cancelProcess = outcome === 'false'
      ? async () => ({ cancelled: false })
      : async () => { throw new Error('cancel unavailable'); };
    getSummary = async () => response({ status: 'idle', start: null });
    await show(response({ status: 'idle', start: null }));
    let generation!: Promise<void>;
    await act(async () => { generation = state.handleGenerateSummary(); });
    let stopping!: Promise<void>;
    await act(async () => { stopping = state.handleStopGeneration(); });

    await act(async () => {
      resolve({ process_id: processId });
      await Promise.all([generation, stopping]);
    });
    expect(state.summaryStatus).toBe('processing');
    expect(timers.size).toBe(1);

    getSummary = async () => response({ start: processId, status: 'completed', data: { markdown: 'Finished summary' } });
    await tick();
    expect(state.summaryStatus).toBe('completed');
  });

  test('an older initial response follows the newer owner process', async () => {
    const older = '2026-09-18T10:00:00.123456700Z';
    const newer = '2026-09-18T10:00:00.123456789Z';
    getSummary = async () => response({ start: newer, status: 'processing' });
    meetingActivityStore.hydrateSummary(response({ start: newer, status: 'processing' }));
    await show(response({ start: older, status: 'pending' }));

    await act(async () => state.handleStopGeneration());
    expect(invoke.mock.calls).toContainEqual([
      'api_cancel_summary',
      { meetingId: 'meeting-a', processId: newer },
    ]);
    expect(timers.size).toBe(0);
  });

  test('completion analytics retain the model used to start the attempt after settings change', async () => {
    const initial = response({ status: 'idle', start: null });
    await show(initial);
    await act(async () => state.handleGenerateSummary());
    configuredModel = 'different-model';
    await show(initial);
    getSummary = async () => response({ status: 'completed', data: { markdown: 'Finished summary' } });
    await tick();
    expect(trackCompletion.mock.calls[0]?.slice(0, 3)).toEqual(['ollama', 'test', true]);
  });

  test('old in-flight poll cannot stop a resumed poll for the same process', async () => {
    let resolve!: (value: SummaryProcessResponse) => void;
    getSummary = () => new Promise(done => { resolve = done; });
    await show(response());
    await tick();
    await show(null);
    await show(response());
    await act(async () => resolve(response({ status: 'completed', data: { markdown: 'Finished summary' } })));
    getSummary = async () => response({ status: 'completed', data: { markdown: 'Finished summary' } });
    await tick();
    expect(state.summaryStatus).toBe('completed');
    expect(text()).toContain('Finished summary');
  });

  test('does not generate while committed model configuration is saving or failed', async () => {
    const initial = response({ status: 'idle', start: null });
    configSaving = true;
    await show(initial);
    await act(async () => state.handleGenerateSummary());
    expect(invoke.mock.calls.some(([command]) => command === 'api_process_transcript')).toBe(false);

    configSaving = false;
    configError = new Error('save failed');
    await show(initial);
    await act(async () => state.handleRegenerateSummary());
    expect(invoke.mock.calls.some(([command]) => command === 'api_process_transcript')).toBe(false);
    expect(state.summaryStatus).toBe('error');
  });
});
