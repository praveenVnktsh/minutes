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
  useSidebar: () => ({ setCurrentMeeting }),
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
});
