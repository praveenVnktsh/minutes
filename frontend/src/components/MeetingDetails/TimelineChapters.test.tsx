import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { StoredTimelineEntry, TimelineSnapshot } from '@/lib/liveTimeline';

type EventHandler = (event: { payload: unknown }) => void;

const handlers = new Map<string, Set<EventHandler>>();
let stored: StoredTimelineEntry[] = [];
let current: TimelineSnapshot | null = null;

const invoke = mock(async (command: string) => {
  if (command === 'list_meeting_timeline_entries') return stored;
  if (command === 'get_live_timeline') return current;
  throw new Error(`unexpected command ${command}`);
});
const listen = mock(async (event: string, handler: EventHandler) => {
  const set = handlers.get(event) ?? new Set();
  set.add(handler);
  handlers.set(event, set);
  return () => set.delete(handler);
});

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalEvent = { ...await import('@tauri-apps/api/event') };
const originalWindow = globalThis.window;
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
  if (originalWindow) Object.defineProperty(globalThis, 'window', { configurable: true, value: originalWindow });
  else Reflect.deleteProperty(globalThis, 'window');
});
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen }));

const { TimelineChapters } = await import('./TimelineChapters');
const { LiveSummaryPanel } = await import('./LiveSummaryPanel');

function entry(id: string, startMs: number, kind: StoredTimelineEntry['kind'], text: string): StoredTimelineEntry {
  return {
    id,
    meeting_id: 'meeting-1',
    start_ms: startMs,
    end_ms: startMs + 20_000,
    kind,
    text,
    speaker: 'Others',
    provider: 'builtin-ai',
    model: 'qwen3.5:4b',
    created_at: '2026-10-16T00:00:00Z',
  };
}

let renderer: ReactTestRenderer | undefined;

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function emit(event: string, payload: unknown) {
  act(() => {
    for (const handler of handlers.get(event) ?? []) handler({ payload });
  });
}

function text(): string {
  const collect = (node: unknown): string => {
    if (typeof node === 'string') return node;
    if (Array.isArray(node)) return node.map(collect).join('');
    if (node && typeof node === 'object' && 'children' in node) {
      return collect((node as { children: unknown }).children ?? []);
    }
    return '';
  };
  return collect(renderer?.toJSON());
}

beforeEach(() => {
  stored = [];
  current = null;
  handlers.clear();
  Object.defineProperty(globalThis, 'window', { configurable: true, value: new EventTarget() });
});

afterEach(async () => {
  if (renderer) {
    const mounted = renderer;
    renderer = undefined;
    await act(async () => mounted.unmount());
  }
});

describe('TimelineChapters', () => {
  test('renders nothing for a meeting without entries', async () => {
    await act(async () => { renderer = create(<TimelineChapters meetingId="meeting-1" />); });
    await flush();
    expect(renderer?.toJSON()).toBeNull();
  });

  test('lists entries with marked decisions and jumps to an entry by time', async () => {
    stored = [entry('a', 65_000, 'point', 'Sync bug is the last blocker'), entry('b', 91_000, 'decision', 'Launch on the tenth')];
    await act(async () => { renderer = create(<TimelineChapters meetingId="meeting-1" />); });
    await flush();

    expect(text()).toContain('Chapters');
    expect(text()).toContain('01:05');
    expect(text()).toContain('Decision');
    expect(text()).toContain('Launch on the tenth');

    const jumps: number[] = [];
    window.addEventListener('meetily:jump-to-time', (event) => {
      jumps.push((event as CustomEvent<{ seconds: number }>).detail.seconds);
    });
    const buttons = renderer!.root.findAll((node) => node.type === 'button' && node.props.title === 'Jump to this point in the transcript');
    act(() => buttons[1].props.onClick());
    expect(jumps).toEqual([91]);
  });

  test('groups chapters under topic headers and still jumps by entry', async () => {
    stored = [
      { ...entry('a', 65_000, 'point', 'Sync bug is the last blocker'), topic: 'Launch Plan' },
      { ...entry('b', 91_000, 'decision', 'Launch on the tenth'), topic: 'Launch Plan' },
      { ...entry('c', 125_000, 'point', 'Free tier held back a month'), topic: 'Pricing' },
    ];
    await act(async () => { renderer = create(<TimelineChapters meetingId="meeting-1" />); });
    await flush();
    const sections = renderer!.root.findAll((node) => node.type === 'li' && typeof node.props['aria-label'] === 'string');
    expect(sections.map((node) => node.props['aria-label'])).toEqual(['Launch Plan', 'Pricing']);
    expect(text()).not.toContain('· Others');
    const buttons = renderer!.root.findAll((node) => node.type === 'button' && node.props.title === 'Jump to this point in the transcript');
    expect(buttons).toHaveLength(3);
  });

  test('reloads when the recording stores its last entry for this meeting', async () => {
    await act(async () => { renderer = create(<TimelineChapters meetingId="meeting-1" />); });
    await flush();
    stored = [entry('late', 200_000, 'action', 'Priya builds the pricing model')];
    emit('live-timeline-update', { session_id: 's', meeting_id: 'meeting-1', status: 'stopped', error: null, entries: [] });
    await flush();
    expect(text()).toContain('Priya builds the pricing model');
  });
});

describe('LiveSummaryPanel', () => {
  test('hydrates from the worker and shows a listening tail', async () => {
    current = { session_id: 's', meeting_id: 'meeting-1', status: 'listening', error: null, entries: [entry('a', 5_000, 'action', 'Ravi adds sync tests')] };
    await act(async () => { renderer = create(<LiveSummaryPanel active />); });
    await flush();
    expect(text()).toContain('Ravi adds sync tests');
    expect(text()).toContain('Action');
    expect(text()).toContain('Listening…');
  });

  test('shows topic headers, the user\'s actions as "You — task", and never the speaker otherwise', async () => {
    current = {
      session_id: 's',
      meeting_id: 'meeting-1',
      status: 'listening',
      error: null,
      entries: [
        { ...entry('a', 5_000, 'point', 'Team grew to ~100 people'), topic: 'Hiring' },
        { ...entry('b', 9_000, 'action', 'connect with founders'), speaker: 'You', topic: 'Hiring' },
        { ...entry('c', 30_000, 'point', 'Office opens in Austin'), topic: 'Expansion' },
      ],
    };
    await act(async () => { renderer = create(<LiveSummaryPanel active />); });
    await flush();
    const rendered = text();
    expect(rendered).toContain('Hiring');
    expect(rendered).toContain('Expansion');
    expect(rendered.indexOf('Hiring')).toBeLessThan(rendered.indexOf('Team grew'));
    expect(rendered.indexOf('Expansion')).toBeLessThan(rendered.indexOf('Office opens'));
    expect(rendered).toContain('You — connect with founders');
    expect(rendered).not.toContain('· Others');
    expect(rendered).not.toContain('· You');
  });

  test('a failure shows a non-blocking paused notice that clears on the next update', async () => {
    await act(async () => { renderer = create(<LiveSummaryPanel active />); });
    await flush();
    emit('live-timeline-update', { session_id: 's', meeting_id: null, status: 'failed', error: 'model offline', entries: [] });
    expect(text()).toContain('Live summary paused');
    emit('live-timeline-update', { session_id: 's', meeting_id: null, status: 'listening', error: null, entries: [entry('a', 0, 'point', 'Back again')] });
    expect(text()).not.toContain('Live summary paused');
    expect(text()).toContain('Back again');
  });
});
