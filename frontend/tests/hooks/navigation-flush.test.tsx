import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const originalNavigation = { ...await import('next/navigation') };
const originalProvider = { ...await import('../../src/components/Sidebar/SidebarProvider') };
const originalPersistence = { ...await import('../../src/services/notePersistenceService') };

afterAll(() => {
  mock.module('next/navigation', () => originalNavigation);
  mock.module('../../src/components/Sidebar/SidebarProvider', () => originalProvider);
  mock.module('../../src/services/notePersistenceService', () => originalPersistence);
});

const push = mock(() => {});
const setCurrentMeeting = mock(() => {});
let flush: () => Promise<void>;
mock.module('next/navigation', () => ({ useRouter: () => ({ push }) }));
mock.module('../../src/components/Sidebar/SidebarProvider', () => ({
  useSidebar: () => ({
    meetings: [{ id: 'legacy / id', title: 'Catalog title', pinned: true }],
    setCurrentMeeting,
  }),
}));
mock.module('../../src/services/notePersistenceService', () => ({
  flushNotes: () => flush(),
}));

const { useMeetingNavigation, useNavigation } = await import('../../src/hooks/useNavigation');
let navigation: ReturnType<typeof useMeetingNavigation>;
let legacyNavigation: ReturnType<typeof useNavigation>;

function Probe() {
  navigation = useMeetingNavigation();
  legacyNavigation = useNavigation('legacy / id', 'Legacy');
  return <output>{navigation.isNavigating}:{navigation.navigationError?.message}</output>;
}

let renderer: ReactTestRenderer | undefined;
beforeEach(async () => {
  push.mockClear();
  setCurrentMeeting.mockClear();
  flush = async () => {};
  await act(async () => { renderer = create(<Probe />); });
});
afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

describe('meeting navigation', () => {
  test('flushes before setting identity and opening a canonical URL', async () => {
    const order: string[] = [];
    flush = async () => { order.push('flush'); };
    setCurrentMeeting.mockImplementation(() => { order.push('meeting'); });
    push.mockImplementation(() => { order.push('push'); });

    await act(async () => { await legacyNavigation(); });
    expect(order).toEqual(['flush', 'meeting', 'push']);
    expect(setCurrentMeeting).toHaveBeenCalledWith({
      id: 'legacy / id', title: 'Legacy', pinned: true,
    });
    expect(push).toHaveBeenCalledWith('/meeting-details?id=legacy%20%2F%20id');
  });

  test('keeps the current screen and identity when draft flush fails', async () => {
    flush = async () => { throw new Error('notes not saved'); };

    let navigationFailure: Error | undefined;
    await act(async () => {
      try {
        await navigation.openMeeting({ id: 'next', title: 'Next' });
      } catch (error) {
        navigationFailure = error as Error;
      }
    });
    expect(navigationFailure?.message).toBe('notes not saved');
    expect(setCurrentMeeting).not.toHaveBeenCalled();
    expect(push).not.toHaveBeenCalled();
    expect(navigation.navigationError?.message).toBe('notes not saved');
    expect(navigation.isNavigating).toBe(false);
  });

  test('only the latest overlapping navigation can change meeting or route', async () => {
    const first = deferred<void>();
    const second = deferred<void>();
    let flushCount = 0;
    flush = () => (++flushCount === 1 ? first : second).promise;

    let firstNavigation!: Promise<void>;
    let secondNavigation!: Promise<void>;
    await act(async () => {
      firstNavigation = navigation.openMeeting({ id: 'first', title: 'First' });
      secondNavigation = navigation.openMeeting({ id: 'second', title: 'Second' });
    });
    await act(async () => second.resolve());
    expect(push).toHaveBeenCalledTimes(1);
    expect(push).toHaveBeenCalledWith('/meeting-details?id=second');
    expect(navigation.isNavigating).toBe(false);

    await act(async () => first.resolve());
    await Promise.all([firstNavigation, secondNavigation]);
    expect(push).toHaveBeenCalledTimes(1);
    expect(setCurrentMeeting).toHaveBeenCalledTimes(1);
  });

  test('a stale failed navigation cannot replace the latest navigation state', async () => {
    const first = deferred<void>();
    const second = deferred<void>();
    let flushCount = 0;
    flush = () => (++flushCount === 1 ? first : second).promise;

    const staleNavigation = navigation.navigate('/first');
    const latestNavigation = navigation.navigate('/second');
    await act(async () => second.resolve());
    await act(async () => first.reject(new Error('stale failure')));
    await Promise.all([staleNavigation, latestNavigation]);

    expect(push).toHaveBeenCalledTimes(1);
    expect(push).toHaveBeenCalledWith('/second');
    expect(navigation.navigationError).toBeNull();
    expect(navigation.isNavigating).toBe(false);
  });
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}
