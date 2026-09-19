import { afterAll, describe, expect, mock, test } from 'bun:test';
import { act, create } from 'react-test-renderer';
import type { MeetingActivitySnapshot } from '@/types/meetingActivity';

let snapshot: MeetingActivitySnapshot = { revision: 1, recording: null, activities: [] };
let model = 'model-a';
const generateAutomaticSummary = mock(async () => ({ started: true, reason: 'started' as const }));
const claims = new Set<string>();

const originalConfig = { ...await import('@/contexts/ConfigContext') };
const originalActivity = { ...await import('@/contexts/MeetingActivityContext') };
const originalAutoSummary = { ...await import('@/lib/autoSummary') };
afterAll(() => {
  mock.module('@/contexts/ConfigContext', () => originalConfig);
  mock.module('@/contexts/MeetingActivityContext', () => originalActivity);
  mock.module('@/lib/autoSummary', () => originalAutoSummary);
});
mock.module('@/contexts/ConfigContext', () => ({
  useConfig: () => ({ isAutoSummary: true, isModelConfigLoading: false, modelConfig: { provider: 'ollama', model } }),
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
});
