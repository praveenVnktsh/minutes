import { describe, expect, test } from 'bun:test';
import {
  acceptSnapshot,
  entryDisplayText,
  formatTimelineTime,
  groupByTopic,
  kindMarker,
  statusNotice,
  type TimelineEntry,
  type TimelineSnapshot,
} from './liveTimeline';

function entry(id: string, topic: string | null | undefined, overrides: Partial<TimelineEntry> = {}): TimelineEntry {
  return {
    id,
    start_ms: 0,
    end_ms: 1_000,
    kind: 'point',
    text: `entry ${id}`,
    speaker: null,
    provider: 'builtin-ai',
    model: 'qwen3.5:4b',
    created_at: '2026-10-16T00:00:00Z',
    ...(topic === undefined ? {} : { topic }),
    ...overrides,
  };
}

describe('groupByTopic', () => {
  test('starts a section whenever the topic changes', () => {
    const sections = groupByTopic([
      entry('a', 'Launch Plan'),
      entry('b', 'Launch Plan'),
      entry('c', 'Pricing'),
      entry('d', 'Launch Plan'),
    ]);
    expect(sections.map((s) => [s.topic, s.entries.map((e) => e.id)])).toEqual([
      ['Launch Plan', ['a', 'b']],
      ['Pricing', ['c']],
      ['Launch Plan', ['d']],
    ]);
  });

  test('entries without a topic stay in the section before them, or render without a header', () => {
    // Stored before topics existed: no field at all.
    expect(groupByTopic([entry('a', undefined), entry('b', null)])).toEqual([
      { topic: null, entries: [entry('a', undefined), entry('b', null)] },
    ]);
    const mixed = groupByTopic([entry('a', 'Budget'), entry('b', null), entry('c', '  ')]);
    expect(mixed).toHaveLength(1);
    expect(mixed[0].entries).toHaveLength(3);
    expect(groupByTopic([])).toEqual([]);
  });
});

describe('entryDisplayText', () => {
  test('an action the user owns reads "You — task"; no other speaker is shown', () => {
    expect(entryDisplayText({ kind: 'action', speaker: 'You', text: 'send the deck' })).toBe('You — send the deck');
    expect(entryDisplayText({ kind: 'action', speaker: 'Others', text: 'Priya sends the deck' })).toBe('Priya sends the deck');
    expect(entryDisplayText({ kind: 'point', speaker: 'You', text: 'Budget is fine' })).toBe('Budget is fine');
    // Older entries already name the user.
    expect(entryDisplayText({ kind: 'action', speaker: 'You', text: 'You suggest a call' })).toBe('You suggest a call');
  });
});

function snapshot(sessionId: string, status: TimelineSnapshot['status']): TimelineSnapshot {
  return { session_id: sessionId, meeting_id: null, status, error: null, entries: [] };
}

describe('formatTimelineTime', () => {
  test('formats minutes, and hours past the hour', () => {
    expect(formatTimelineTime(0)).toBe('00:00');
    expect(formatTimelineTime(61_900)).toBe('01:01');
    expect(formatTimelineTime(3_725_000)).toBe('1:02:05');
    expect(formatTimelineTime(-10)).toBe('00:00');
  });
});

describe('kindMarker', () => {
  test('marks decisions and actions with tones, points not at all', () => {
    expect(kindMarker('decision')).toEqual({ label: 'Decision', tone: 'success' });
    expect(kindMarker('action')).toEqual({ label: 'Action', tone: 'info' });
    expect(kindMarker('point')).toBeNull();
  });
});

describe('acceptSnapshot', () => {
  test('updates for the same session replace the snapshot', () => {
    const next = snapshot('a', 'generating');
    expect(acceptSnapshot(snapshot('a', 'listening'), next)).toBe(next);
    expect(acceptSnapshot(null, next)).toBe(next);
  });

  test('a new live session replaces an old one, a late stop from another does not', () => {
    const current = snapshot('b', 'listening');
    expect(acceptSnapshot(current, snapshot('a', 'stopped'))).toBe(current);
    const newer = snapshot('c', 'listening');
    expect(acceptSnapshot(current, newer)).toBe(newer);
  });
});

describe('statusNotice', () => {
  test('a failure is a non-blocking warning that says it retries', () => {
    const notice = statusNotice(snapshot('a', 'failed'));
    expect(notice?.tone).toBe('warning');
    expect(notice?.text).toContain('Live summary paused');
  });

  test('a refusing provider says why instead of asking for a model', () => {
    const refused = { ...snapshot('a', 'unavailable'), error: 'The summary provider refused the request.' };
    expect(statusNotice(refused)).toEqual({ tone: 'warning', text: 'The summary provider refused the request.' });
    expect(statusNotice(snapshot('a', 'unavailable'))?.text).toContain('needs a summary model');
  });

  test('normal states show no notice', () => {
    expect(statusNotice(snapshot('a', 'listening'))).toBeNull();
    expect(statusNotice(snapshot('a', 'generating'))).toBeNull();
    expect(statusNotice(null)).toBeNull();
  });
});
