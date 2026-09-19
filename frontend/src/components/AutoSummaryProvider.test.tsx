import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create } from 'react-test-renderer';
import type { MeetingActivitySnapshot } from '@/types/meetingActivity';

let snapshot: MeetingActivitySnapshot = { revision: 1, recording: null, activities: [] };
let model = 'model-a';
let saving = false;
let configError: Error | null = null;
const generateAutomaticSummary = mock(async () => ({ started: true, reason: 'started' as const }));
const claims = new Set<string>();

const originalConfig = { ...await import('@/contexts/ConfigContext') };
const originalActivity = { ...await import('@/contexts/MeetingActivityContext') };
const originalAutoSummary = { ...await import('@/lib/autoSummary') };
const originalWindow = globalThis.window;
afterAll(() => {
  mock.module('@/contexts/ConfigContext', () => originalConfig);
  mock.module('@/contexts/MeetingActivityContext', () => originalActivity);
  mock.module('@/lib/autoSummary', () => originalAutoSummary);
  if (originalWindow) Object.defineProperty(globalThis, 'window', { configurable: true, value: originalWindow });
  else Reflect.deleteProperty(globalThis, 'window');
});
mock.module('@/contexts/ConfigContext', () => ({
  useConfig: () => ({
    isAutoSummary: true,
    isModelConfigLoading: false,
    isModelConfigSaving: saving,
    modelConfigSaveError: configError,
    modelConfig: { provider: 'ollama', model },
  }),
}));
mock.module('@/contexts/MeetingActivityContext', () => ({
  useMeetingActivity: () => ({ snapshot }),
}));
mock.module('@/lib/autoSummary', () => ({
  generateAutomaticSummary,
  claimAutoSummaryJob: (taskId: string, config: { provider: string; model: string }) => {
    const key = `${taskId}:${config.provider}:${config.model}`;
    if (claims.has(key)) return false;
    claims.add(key);
    return true;
  },
  releaseAutoSummaryJob: () => {},
}));

const { AutoSummaryProvider } = await import('./AutoSummaryProvider');

describe('AutoSummaryProvider', () => {
  beforeEach(() => {
    Object.defineProperty(globalThis, 'window', { configurable: true, value: new EventTarget() });
    snapshot = { revision: 1, recording: null, activities: [] };
    claims.clear();
    generateAutomaticSummary.mockClear();
    model = 'model-a';
    saving = false;
    configError = null;
  });

  test('starts once per terminal task and committed model configuration', async () => {
    snapshot = {
      revision: 2,
      recording: null,
      activities: [{
        task_id: 'task-a', meeting_id: 'meeting-a', kind: 'retranscription', status: 'ready',
        title: 'A', stage: 'complete', progress_percentage: 100, message: null,
        error: null, warning: null, controls_available: false, revision: 2,
      }],
    };
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<AutoSummaryProvider />); });
    await act(async () => { renderer.update(<AutoSummaryProvider />); });
    expect(generateAutomaticSummary).toHaveBeenCalledTimes(1);

    model = 'model-b';
    await act(async () => { renderer.update(<AutoSummaryProvider />); });
    expect(generateAutomaticSummary).toHaveBeenCalledTimes(2);
    await act(async () => renderer.unmount());
  });

  test('does not start from uncommitted or failed model configuration', async () => {
    snapshot = {
      revision: 3,
      recording: null,
      activities: [{
        task_id: 'task-b', meeting_id: 'meeting-b', kind: 'import', status: 'ready',
        title: 'B', stage: 'complete', progress_percentage: 100, message: null,
        error: null, warning: null, controls_available: false, revision: 3,
      }],
    };
    saving = true;
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<AutoSummaryProvider />); });
    expect(generateAutomaticSummary).not.toHaveBeenCalled();
    saving = false;
    configError = new Error('save failed');
    await act(async () => renderer.update(<AutoSummaryProvider />));
    expect(generateAutomaticSummary).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  test('does not silently retry a failed automatic start on rerender', async () => {
    snapshot = {
      revision: 4,
      recording: null,
      activities: [{
        task_id: 'task-c', meeting_id: 'meeting-c', kind: 'import', status: 'ready',
        title: 'C', stage: 'complete', progress_percentage: 100, message: null,
        error: null, warning: null, controls_available: false, revision: 4,
      }],
    };
    generateAutomaticSummary.mockRejectedValueOnce(new Error('provider offline'));
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<AutoSummaryProvider />); });
    await act(async () => { renderer.update(<AutoSummaryProvider />); });
    expect(generateAutomaticSummary).toHaveBeenCalledTimes(1);
    await act(async () => renderer.unmount());
  });

  test('waits for transcript persistence before starting a live recording summary', async () => {
    snapshot = {
      revision: 5,
      recording: null,
      activities: [{
        task_id: 'recording-session', meeting_id: 'meeting-live', kind: 'recording', status: 'ready',
        title: 'Live meeting', stage: 'complete', progress_percentage: 100, message: null,
        error: null, warning: null, controls_available: false, revision: 5,
      }],
    };
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<AutoSummaryProvider />); });
    expect(generateAutomaticSummary).not.toHaveBeenCalled();

    await act(async () => {
      const event = Object.assign(new Event('meetily:meeting-ready-for-summary'), {
        detail: { meetingId: 'meeting-live' },
      });
      window.dispatchEvent(event);
    });
    expect(generateAutomaticSummary).toHaveBeenCalledWith(
      'meeting-live',
      expect.objectContaining({ provider: 'ollama', model: 'model-a' }),
    );
    await act(async () => renderer.unmount());
  });

  test('retains persistence readiness until model configuration finishes saving', async () => {
    saving = true;
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<AutoSummaryProvider />); });
    await act(async () => {
      const event = Object.assign(new Event('meetily:meeting-ready-for-summary'), {
        detail: { meetingId: 'meeting-saving' },
      });
      window.dispatchEvent(event);
    });
    expect(generateAutomaticSummary).not.toHaveBeenCalled();

    saving = false;
    await act(async () => renderer.update(<AutoSummaryProvider />));
    expect(generateAutomaticSummary).toHaveBeenCalledWith(
      'meeting-saving',
      expect.objectContaining({ provider: 'ollama', model: 'model-a' }),
    );
    await act(async () => renderer.unmount());
  });
});
