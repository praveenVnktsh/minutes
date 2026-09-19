import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { MeetingMetadata, PaginatedTranscriptsResponse } from '../../src/types';

const originalCore = { ...await import('@tauri-apps/api/core') };
type Request = {
  command: string;
  args: Record<string, unknown>;
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
};
const requests: Request[] = [];
const invoke = mock((command: string, args: Record<string, unknown>) => new Promise((resolve, reject) => {
  requests.push({ command, args, resolve, reject });
}));
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
const { usePaginatedTranscripts } = await import('../../src/hooks/usePaginatedTranscripts');
afterAll(() => mock.module('@tauri-apps/api/core', () => originalCore));

let state: ReturnType<typeof usePaginatedTranscripts>;
let renderer: ReactTestRenderer | undefined;
function View({ meetingId }: { meetingId: string | null }) {
  state = usePaginatedTranscripts({ meetingId });
  return <output>{state.metadata?.id}:{state.transcripts.map(t => t.text).join(',')}</output>;
}
beforeEach(() => {
  requests.length = 0;
  invoke.mockClear();
  mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
});
afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});
async function show(meetingId: string | null) {
  await act(async () => {
    if (renderer) renderer.update(<View meetingId={meetingId} />);
    else renderer = create(<View meetingId={meetingId} />);
  });
}
function request(command: 'metadata' | 'transcripts', meetingId: string, index = 0) {
  const result = requests.filter(r => r.command === `api_get_meeting_${command}` && r.args.meetingId === meetingId)[index];
  expect(result).toBeDefined();
  return result;
}
const metadata = (id: string): MeetingMetadata => ({ id, title: id, created_at: '2026-09-07', updated_at: '2026-09-07' });
const page = (text: string, hasMore = false): PaginatedTranscriptsResponse => ({
  transcripts: [{ id: text, text, timestamp: '00:00', audio_start_time: 0 }],
  total_count: hasMore ? 2 : 1, has_more: hasMore,
});
async function resolve(req: Request, value: unknown) {
  await act(async () => { req.resolve(value); });
}
async function load(meetingId: string, hasMore = false) {
  await show(meetingId);
  await resolve(request('metadata', meetingId), metadata(meetingId));
  await resolve(request('transcripts', meetingId), page(meetingId, hasMore));
}

describe('paginated transcript request ownership', () => {
  test('ignores late A metadata after B finishes and does not start A transcripts', async () => {
    await show('A');
    const stale = request('metadata', 'A');
    await load('B');
    await resolve(stale, metadata('A'));
    expect(state.metadata?.id).toBe('B');
    expect(state.transcripts[0].text).toBe('B');
    expect(state.isLoading).toBe(false);
    expect(requests.filter(r => r.command === 'api_get_meeting_transcripts' && r.args.meetingId === 'A')).toHaveLength(0);
  });

  test('late A transcripts cannot replace B or finish its loading state', async () => {
    await show('A');
    await resolve(request('metadata', 'A'), metadata('A'));
    const stale = request('transcripts', 'A');
    await show('B');
    await resolve(stale, page('A'));
    expect(state.isLoading).toBe(true);
    expect(state.transcripts).toEqual([]);
    await resolve(request('metadata', 'B'), metadata('B'));
    await resolve(request('transcripts', 'B'), page('B'));
    expect(state.metadata?.id).toBe('B');
    expect(state.transcripts[0].text).toBe('B');
  });

  test('returning A to B to A does not revive the first A request', async () => {
    await show('A');
    const stale = request('metadata', 'A');
    await show('B');
    await show('A');
    await resolve(request('metadata', 'A', 1), { ...metadata('A'), title: 'Current A' });
    await resolve(request('transcripts', 'A'), page('Current A'));
    await resolve(stale, { ...metadata('A'), title: 'Old A' });
    expect(state.metadata?.title).toBe('Current A');
    expect(state.transcripts[0].text).toBe('Current A');
    expect(state.isLoading).toBe(false);
    expect(requests.filter(r => r.command === 'api_get_meeting_transcripts' && r.args.meetingId === 'A')).toHaveLength(1);
  });

  test('a stale failure cannot add an error or clear current loading', async () => {
    await show('A');
    const stale = request('metadata', 'A');
    await show('B');
    await act(async () => { stale.reject(new Error('A unavailable')); });
    expect(state.error).toBeNull();
    expect(state.isLoading).toBe(true);
  });

  test('newest refetch wins over an earlier request for the same meeting', async () => {
    await load('A');
    await act(async () => { void state.refetch(); });
    await resolve(request('metadata', 'A', 1), metadata('A'));
    const stale = request('transcripts', 'A', 1);
    await act(async () => { void state.refetch(); });
    await resolve(request('metadata', 'A', 2), metadata('A'));
    await resolve(request('transcripts', 'A', 2), page('newest'));
    await resolve(stale, page('stale'));
    expect(state.transcripts[0].text).toBe('newest');
    expect(state.isLoading).toBe(false);
  });

  test('initial load fetches the whole transcript and further loadMore is a no-op', async () => {
    await show('A');
    await resolve(request('metadata', 'A'), metadata('A'));
    // First page reports more rows, so the hook fetches the complete set.
    await resolve(request('transcripts', 'A', 0), page('A first', true));
    await resolve(request('transcripts', 'A', 1), page('A second'));

    expect(state.hasMore).toBe(false);
    expect(state.isLoading).toBe(false);
    expect(state.transcripts.map(t => t.text)).toEqual(['A first', 'A second']);

    // Nothing left to page in, so scrolling must not trigger another IPC call.
    const calls = requests.length;
    await act(async () => { void state.loadMore(); });
    expect(requests.length).toBe(calls);
  });

  test('loads and retains matches beyond the first 100 rows without duplicates', async () => {
    await show('long');
    await resolve(request('metadata', 'long'), metadata('long'));
    const firstPage: PaginatedTranscriptsResponse = {
      transcripts: Array.from({ length: 100 }, (_, index) => ({
        id: `row-${index}`,
        text: `Row ${index}`,
        timestamp: '00:00',
        audio_start_time: index,
      })),
      total_count: 101,
      has_more: true,
    };
    await resolve(request('transcripts', 'long', 0), firstPage);
    expect(request('transcripts', 'long', 1).args.offset).toBe(100);
    await resolve(request('transcripts', 'long', 1), {
      transcripts: [{ id: 'row-100', text: 'Needle after page one', timestamp: '01:40', audio_start_time: 100 }],
      total_count: 101,
      has_more: false,
    });

    expect(state.transcripts).toHaveLength(101);
    expect(state.transcripts[100].text).toBe('Needle after page one');
    expect(new Set(state.transcripts.map((item) => item.id)).size).toBe(101);
  });

  test('a stale full load cannot replace the current meeting transcripts', async () => {
    await show('A');
    await resolve(request('metadata', 'A'), metadata('A'));
    const stale = request('transcripts', 'A', 0);
    await show('B');
    await resolve(request('metadata', 'B'), metadata('B'));
    await resolve(request('transcripts', 'B', 0), page('B'));

    await resolve(stale, page('A'));
    expect(state.metadata?.id).toBe('B');
    expect(state.transcripts.map(t => t.text)).toEqual(['B']);
  });

  test('reset invalidates pending reads and null to same meeting reloads', async () => {
    await show('A');
    const stale = request('metadata', 'A');
    await act(async () => { state.reset(); });
    await resolve(stale, metadata('A'));
    expect(state.metadata).toBeNull();
    await show(null);
    await show('A');
    await resolve(request('metadata', 'A', 1), metadata('A'));
    await resolve(request('transcripts', 'A'), page('fresh'));
    expect(state.transcripts[0].text).toBe('fresh');
  });

  test('unmount prevents a pending metadata read from starting transcript IPC', async () => {
    await show('A');
    const stale = request('metadata', 'A');
    await act(async () => { renderer!.unmount(); });
    renderer = undefined;
    await resolve(stale, metadata('A'));
    expect(requests).toHaveLength(1);
  });
});
