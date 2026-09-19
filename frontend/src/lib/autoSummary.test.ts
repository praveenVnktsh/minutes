import { beforeEach, describe, expect, test } from 'bun:test';
import {
  claimAutoSummaryJob,
  consumeDeferredMeetingForAutoSummary,
  markDeferredMeetingForAutoSummary,
  releaseAutoSummaryJob,
} from './autoSummary';

const values = new Map<string, string>();
Object.defineProperty(globalThis, 'localStorage', {
  configurable: true,
  value: {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  },
});

describe('deferred auto-summary markers', () => {
  beforeEach(() => values.clear());

  test('persists and consumes a pending meeting once', () => {
    markDeferredMeetingForAutoSummary('meeting-1');

    expect(consumeDeferredMeetingForAutoSummary('meeting-1')).toBe(true);
    expect(consumeDeferredMeetingForAutoSummary('meeting-1')).toBe(false);
  });

  test('does not consume another pending meeting', () => {
    markDeferredMeetingForAutoSummary('meeting-1');
    markDeferredMeetingForAutoSummary('meeting-2');

    expect(consumeDeferredMeetingForAutoSummary('meeting-1')).toBe(true);
    expect(consumeDeferredMeetingForAutoSummary('meeting-2')).toBe(true);
  });

  test('claims a terminal task once per committed model configuration', () => {
    const first = { provider: 'ollama' as const, model: 'model-a' };
    const second = { provider: 'ollama' as const, model: 'model-b' };
    expect(claimAutoSummaryJob('task-1', first)).toBe(true);
    expect(claimAutoSummaryJob('task-1', first)).toBe(false);
    expect(claimAutoSummaryJob('task-1', second)).toBe(true);
    releaseAutoSummaryJob('task-1', first);
    expect(claimAutoSummaryJob('task-1', first)).toBe(true);
  });
});
