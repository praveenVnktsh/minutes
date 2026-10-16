import { describe, expect, test } from 'bun:test';
import {
  acceptSnapshot,
  formatTimelineTime,
  kindMarker,
  statusNotice,
  type TimelineSnapshot,
} from './liveTimeline';

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

  test('normal states show no notice', () => {
    expect(statusNotice(snapshot('a', 'listening'))).toBeNull();
    expect(statusNotice(snapshot('a', 'generating'))).toBeNull();
    expect(statusNotice(null)).toBeNull();
  });
});
