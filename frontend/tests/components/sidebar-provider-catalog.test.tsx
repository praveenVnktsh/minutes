import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { ReactNode } from 'react';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalAnalytics = { ...await import('../../src/lib/analytics') };
const originalNavigation = { ...await import('next/navigation') };
const originalRecordingState = { ...await import('../../src/contexts/RecordingStateContext') };
const originalPersistence = { ...await import('../../src/services/notePersistenceService') };

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('../../src/lib/analytics', () => originalAnalytics);
  mock.module('next/navigation', () => originalNavigation);
  mock.module('../../src/contexts/RecordingStateContext', () => originalRecordingState);
  mock.module('../../src/services/notePersistenceService', () => originalPersistence);
});

let pathname = '/';
const push = mock(() => {});
const replace = mock(() => {});
mock.module('next/navigation', () => ({
  usePathname: () => pathname,
  useRouter: () => ({ push, replace }),
}));
mock.module('../../src/contexts/RecordingStateContext', () => ({
  useRecordingState: () => ({ isRecording: false }),
}));
mock.module('../../src/lib/analytics', () => ({
  default: { trackBackendConnection() {}, trackButtonClick() {} },
}));
let flush: () => Promise<void> = async () => {};
mock.module('../../src/services/notePersistenceService', () => ({
  flushNotes: () => flush(),
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
  pathname = '/';
  push.mockClear();
  replace.mockClear();
  flush = async () => {};
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

  test('serializes same-field writes and keeps a newer success after an older failure', async () => {
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
    expect(renameCount).toBe(1);
    const firstOutcome = firstRename.then(() => null, (error) => error as Error);
    await act(async () => first.reject(new Error('older failure')));
    expect(renameCount).toBe(2);
    await act(async () => second.resolve(true));
    expect((await firstOutcome)?.message).toBe('older failure');
    await secondRename;
    expect(sidebar.meetings[0]).toEqual({ ...catalog[0], title: 'Second' });
    expect(sidebar.meetingMutations['meeting-a']).toBeUndefined();
  });

  test('rolls two rejected renames back to the last acknowledged title', async () => {
    const first = deferred<unknown>();
    const second = deferred<unknown>();
    let renameCount = 0;
    handler = async (command) => {
      if (command === 'api_get_meetings') return catalog;
      if (command === 'api_save_meeting_title') return (++renameCount === 1 ? first : second).promise;
      throw new Error(`Unexpected command: ${command}`);
    };
    await render();

    const firstRename = sidebar.renameMeeting('meeting-a', 'First');
    const firstOutcome = firstRename.then(() => null, (error) => error as Error);
    const secondRename = sidebar.renameMeeting('meeting-a', 'Second');
    const secondOutcome = secondRename.then(() => null, (error) => error as Error);
    await act(async () => first.reject(new Error('first rejected')));
    expect(sidebar.meetings[0].title).toBe('Second');
    expect(renameCount).toBe(2);
    await act(async () => second.reject(new Error('second rejected')));

    expect((await firstOutcome)?.message).toBe('first rejected');
    expect((await secondOutcome)?.message).toBe('second rejected');
    expect(sidebar.meetings[0]).toEqual(catalog[0]);
    expect(sidebar.meetingMutations['meeting-a']?.rename).toEqual({
      status: 'error', error: 'second rejected',
    });
  });

  test('prevents successful native writes from completing out of order', async () => {
    const first = deferred<unknown>();
    const second = deferred<unknown>();
    let renameCount = 0;
    handler = async (command) => {
      if (command === 'api_get_meetings') return catalog;
      if (command === 'api_save_meeting_title') return (++renameCount === 1 ? first : second).promise;
      throw new Error(`Unexpected command: ${command}`);
    };
    await render();

    const firstRename = sidebar.renameMeeting('meeting-a', 'First');
    const secondRename = sidebar.renameMeeting('meeting-a', 'Second');
    await act(async () => second.resolve(true));
    expect(renameCount).toBe(1);
    await act(async () => first.resolve(true));
    await Promise.all([firstRename, secondRename]);

    expect(renameCount).toBe(2);
    expect(sidebar.meetings[0]).toEqual({ ...catalog[0], title: 'Second' });
    expect(sidebar.meetingMutations['meeting-a']).toBeUndefined();
  });

  test('rolls a rejected newer rename back to the preceding acknowledged success', async () => {
    const first = deferred<unknown>();
    const second = deferred<unknown>();
    let renameCount = 0;
    handler = async (command) => {
      if (command === 'api_get_meetings') return catalog;
      if (command === 'api_save_meeting_title') return (++renameCount === 1 ? first : second).promise;
      throw new Error(`Unexpected command: ${command}`);
    };
    await render();

    const firstRename = sidebar.renameMeeting('meeting-a', 'First');
    const secondRename = sidebar.renameMeeting('meeting-a', 'Second');
    const secondOutcome = secondRename.then(() => null, (error) => error as Error);
    await act(async () => first.resolve(true));
    await firstRename;
    await act(async () => second.reject(new Error('second rejected')));

    expect((await secondOutcome)?.message).toBe('second rejected');
    expect(sidebar.meetings[0]).toEqual({ ...catalog[0], title: 'First' });
  });

  test('does not let an older refresh overwrite a mutation that settles after it started', async () => {
    const refresh = deferred<unknown>();
    let catalogReads = 0;
    handler = async (command) => {
      if (command === 'api_get_meetings') return ++catalogReads === 1 ? catalog : refresh.promise;
      if (command === 'api_set_meeting_pinned') return true;
      throw new Error(`Unexpected command: ${command}`);
    };
    await render();

    let refreshing!: Promise<void>;
    await act(async () => { refreshing = sidebar.refetchMeetings(); });
    await act(async () => { await sidebar.setMeetingPinned('meeting-a', false); });
    await act(async () => refresh.resolve([{ ...catalog[0], title: 'Stale', pinned: true }]));
    await refreshing;

    expect(sidebar.meetings).toEqual([{ ...catalog[0], title: 'Stale', pinned: false }]);
  });

  test('retains a pending mutation and its catalog row when refresh omits it', async () => {
    const rename = deferred<unknown>();
    const refresh = deferred<unknown>();
    let catalogReads = 0;
    handler = async (command) => {
      if (command === 'api_get_meetings') return ++catalogReads === 1 ? catalog : refresh.promise;
      if (command === 'api_save_meeting_title') return rename.promise;
      throw new Error(`Unexpected command: ${command}`);
    };
    await render();

    const renaming = sidebar.renameMeeting('meeting-a', 'Pending');
    let refreshing!: Promise<void>;
    await act(async () => { refreshing = sidebar.refetchMeetings(); });
    await act(async () => refresh.resolve([]));
    await refreshing;
    expect(sidebar.meetings).toEqual([{ ...catalog[0], title: 'Pending' }]);
    await act(async () => rename.resolve(true));
    await renaming;
    expect(sidebar.meetings[0].title).toBe('Pending');
  });

  test('lets only the latest provider navigation complete after overlapping flushes', async () => {
    pathname = '/meeting-details';
    const first = deferred<void>();
    const second = deferred<void>();
    let flushCount = 0;
    flush = () => (++flushCount === 1 ? first : second).promise;
    const stored: Record<string, string> = {};
    Object.defineProperty(globalThis, 'sessionStorage', {
      configurable: true,
      value: { setItem: (key: string, value: string) => { stored[key] = value; } },
    });
    await render();

    const firstNavigation = sidebar.handleRecordingToggle();
    const secondNavigation = sidebar.handleRecordingToggle();
    await act(async () => second.resolve());
    expect(push).toHaveBeenCalledTimes(1);
    expect(stored.autoStartRecording).toBe('true');
    await act(async () => first.reject(new Error('stale flush failure')));
    await Promise.all([firstNavigation, secondNavigation]);

    expect(push).toHaveBeenCalledTimes(1);
    expect(sidebar.navigationError).toBeNull();
    delete (globalThis as { sessionStorage?: Storage }).sessionStorage;
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
