import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { ReactNode } from 'react';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalAnalytics = { ...await import('../../src/lib/analytics') };
const originalNavigation = { ...await import('next/navigation') };
const originalRecordingState = { ...await import('../../src/contexts/RecordingStateContext') };

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('../../src/lib/analytics', () => originalAnalytics);
  mock.module('next/navigation', () => originalNavigation);
  mock.module('../../src/contexts/RecordingStateContext', () => originalRecordingState);
});

mock.module('next/navigation', () => ({
  usePathname: () => '/',
  useRouter: () => ({ push() {}, replace() {} }),
}));
mock.module('../../src/contexts/RecordingStateContext', () => ({
  useRecordingState: () => ({ isRecording: false }),
}));
mock.module('../../src/lib/analytics', () => ({
  default: { trackBackendConnection() {}, trackButtonClick() {} },
}));

type Invocation = { command: string; args?: Record<string, unknown> };
let handler: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
const invocations: Invocation[] = [];
const invoke = mock((command: string, args?: Record<string, unknown>) => {
  invocations.push({ command, args });
  return handler(command, args);
});
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));

const { SidebarProvider, useSidebar } = await import('../../src/components/Sidebar/SidebarProvider');
type Sidebar = ReturnType<typeof useSidebar>;
let sidebar: Sidebar;

function Probe() {
  sidebar = useSidebar();
  return <output>{sidebar.catalogStatus}:{sidebar.searchStatus}:{sidebar.meetings.length}</output>;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

let renderer: ReactTestRenderer | undefined;
async function render(children: ReactNode = <Probe />) {
  await act(async () => {
    renderer = create(<SidebarProvider>{children}</SidebarProvider>);
  });
}

beforeEach(() => {
  invocations.length = 0;
  invoke.mockClear();
  handler = async (command) => command === 'api_get_meetings' ? [] : undefined;
});

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

const catalog = [{
  id: 'meeting-a', title: 'Original', created_at: '2026-09-18', pinned: true, archived: false, debug: true,
}];
const result = (id: string, title: string) => ({ id, title, matchContext: 'hit', timestamp: '0' });

describe('SidebarProvider catalog state', () => {
  test('distinguishes loading and refresh failure while retaining the last catalog', async () => {
    const initial = deferred<unknown>();
    handler = async (command) => {
      if (command === 'api_get_meetings') return initial.promise;
      throw new Error(`Unexpected command: ${command}`);
    };

    await render();
    expect(sidebar.catalogStatus).toBe('loading');
    await act(async () => initial.resolve(catalog));
    expect(sidebar.catalogStatus).toBe('ready');
    expect(sidebar.meetings).toEqual(catalog);

    const refresh = deferred<unknown>();
    handler = async () => refresh.promise;
    await act(async () => { void sidebar.refetchMeetings(); });
    expect(sidebar.catalogStatus).toBe('refreshing');
    await act(async () => refresh.reject(new Error('database busy')));
    expect(sidebar.catalogStatus).toBe('error');
    expect(sidebar.catalogError).toBe('database busy');
    expect(sidebar.meetings).toEqual(catalog);
  });

  test('lets only the latest search own results and clear invalidates pending work', async () => {
    const searches = new Map<string, ReturnType<typeof deferred<unknown>>>();
    handler = async (command, args) => {
      if (command === 'api_get_meetings') return catalog;
      const request = deferred<unknown>();
      searches.set(args!.query as string, request);
      return request.promise;
    };
    await render();

    await act(async () => { void sidebar.searchTranscripts('old'); void sidebar.searchTranscripts('new'); });
    await act(async () => searches.get('new')!.resolve([result('meeting-a', 'Stale title')]));
    expect(sidebar.searchQuery).toBe('new');
    expect(sidebar.searchResults.map(({ id }) => id)).toEqual(['meeting-a']);
    await act(async () => searches.get('old')!.resolve([]));
    expect(sidebar.searchResults.map(({ id }) => id)).toEqual(['meeting-a']);

    await act(async () => { void sidebar.searchTranscripts('pending'); });
    await act(async () => { await sidebar.searchTranscripts(''); });
    await act(async () => searches.get('pending')!.resolve([result('meeting-a', 'Old')]));
    expect(sidebar.searchStatus).toBe('idle');
    expect(sidebar.searchQuery).toBe('');
    expect(sidebar.searchResults).toEqual([]);
  });

  test('reports search errors and excludes unresolved identities', async () => {
    handler = async (command) => {
      if (command === 'api_get_meetings') return catalog;
      if (command === 'api_search_transcripts') return [result('unknown', 'Unknown')];
      throw new Error('unexpected');
    };
    await render();
    await act(async () => { await sidebar.searchTranscripts('missing'); });
    expect(sidebar.searchStatus).toBe('success');
    expect(sidebar.searchResults).toEqual([]);
    expect(sidebar.hasNoSearchResults).toBe(true);

    handler = async (command) => {
      if (command === 'api_search_transcripts') throw new Error('search offline');
      return catalog;
    };
    await act(async () => { await sidebar.searchTranscripts('failure'); });
    expect(sidebar.searchStatus).toBe('error');
    expect(sidebar.searchError).toBe('search offline');
  });

  test('preserves metadata and ignores an older rename rollback after newer success', async () => {
    const first = deferred<unknown>();
    const second = deferred<unknown>();
    let renameCount = 0;
    handler = async (command) => {
      if (command === 'api_get_meetings') return catalog;
      if (command === 'api_save_meeting_title') return (++renameCount === 1 ? first : second).promise;
      throw new Error(`Unexpected command: ${command}`);
    };
    await render();

    let firstRename!: Promise<void>;
    let secondRename!: Promise<void>;
    await act(async () => {
      firstRename = sidebar.renameMeeting('meeting-a', 'First');
      secondRename = sidebar.renameMeeting('meeting-a', 'Second');
    });
    expect(sidebar.meetings[0]).toEqual({ ...catalog[0], title: 'Second' });
    expect(sidebar.meetingMutations['meeting-a']?.rename?.status).toBe('pending');
    const firstOutcome = firstRename.then(() => null, (error) => error as Error);
    await act(async () => second.resolve(true));
    await act(async () => first.reject(new Error('older failure')));
    expect((await firstOutcome)?.message).toBe('older failure');
    await secondRename;
    expect(sidebar.meetings[0]).toEqual({ ...catalog[0], title: 'Second' });
    expect(sidebar.meetingMutations['meeting-a']).toBeUndefined();
  });

  test('rolls back only the failed field and exposes retryable mutation error state', async () => {
    handler = async (command) => {
      if (command === 'api_get_meetings') return catalog;
      if (command === 'api_set_meeting_archived') throw new Error('write denied');
      return true;
    };
    await render();

    let mutationError: Error | undefined;
    await act(async () => {
      try {
        await sidebar.setMeetingArchived('meeting-a', true);
      } catch (error) {
        mutationError = error as Error;
      }
    });
    expect(mutationError?.message).toBe('write denied');
    expect(sidebar.meetings[0]).toEqual(catalog[0]);
    expect(sidebar.meetingMutations['meeting-a']?.archive).toEqual({
      status: 'error', error: 'write denied',
    });
  });
});
