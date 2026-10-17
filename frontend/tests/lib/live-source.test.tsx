import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

/* eslint-disable @typescript-eslint/no-explicit-any */

// The recording screen's live hooks must keep calling Tauri exactly as before
// when no replay provides a source: same events, same commands, same args.

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalEvent = { ...await import('@tauri-apps/api/event') };

const invoke = mock(async (command: string, _args?: Record<string, unknown>): Promise<unknown> => {
  if (command === 'get_recording_level') return 0;
  return null;
});
const unlisten = mock(() => {});
const listen = mock(async (_event: string, _handler: (event: any) => void) => unlisten);
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen }));

const { useCopilot } = await import('../../src/hooks/useCopilot');
const { useLiveTimeline } = await import('../../src/components/MeetingDetails/LiveSummaryPanel');
const { copilotApi, copilotApiFor } = await import('../../src/lib/copilot');
const { LiveSourceProvider, tauriLiveSource, useLiveSource } = await import('../../src/lib/liveSource');

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
});

let renderer: ReactTestRenderer | undefined;
beforeEach(() => {
  invoke.mockClear();
  listen.mockClear();
  unlisten.mockClear();
});
afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

function Hooks() {
  useCopilot();
  useLiveTimeline();
  return null;
}

describe('the default live source is Tauri', () => {
  test('the live hooks listen and read through Tauri', async () => {
    await act(async () => { renderer = create(<Hooks />); });
    expect(invoke.mock.calls.map((c) => c[0]).sort()).toEqual(['copilot_get_state', 'get_live_timeline']);
    // Reads with no arguments are still called without any.
    for (const call of invoke.mock.calls) expect(call.length).toBe(1);
    expect(listen.mock.calls.map((c) => c[0]).sort()).toEqual(['copilot-delta', 'copilot-state', 'live-timeline-update']);
    await act(async () => renderer!.unmount());
    renderer = undefined;
    expect(unlisten).toHaveBeenCalledTimes(3);
  });

  test('Copilot commands keep their names and arguments', async () => {
    await copilotApi.ask('catch_up', null);
    await copilotApi.stop('m1');
    await copilotApi.retry('m1', true);
    await copilotApi.dismissCard('m2', false);
    await copilotApi.setNotes('notes');
    expect(invoke.mock.calls).toEqual([
      ['copilot_ask', { action: 'catch_up', question: null }],
      ['copilot_stop', { id: 'm1' }],
      ['copilot_retry', { id: 'm1', useBuiltin: true }],
      ['copilot_dismiss_card', { id: 'm2', notForMe: false }],
      ['copilot_set_notes', { text: 'notes' }],
    ]);
  });

  test('without a provider the context is the Tauri source', async () => {
    let seen: unknown = null;
    function Probe() {
      seen = useLiveSource();
      return null;
    }
    await act(async () => { renderer = create(<Probe />); });
    expect(seen).toBe(tauriLiveSource);
  });

  test('a provided source replaces Tauri for its subtree only', async () => {
    const calls: string[] = [];
    const fake = {
      kind: 'replay' as const,
      listen: async (event: string) => { calls.push(`listen:${event}`); return () => {}; },
      invoke: async (command: string) => { calls.push(`invoke:${command}`); return null as any; },
    };
    await act(async () => {
      renderer = create(<LiveSourceProvider source={fake}><Hooks /></LiveSourceProvider>);
    });
    expect(invoke).not.toHaveBeenCalled();
    expect(listen).not.toHaveBeenCalled();
    expect(calls.sort()).toEqual(['invoke:copilot_get_state', 'invoke:get_live_timeline', 'listen:copilot-delta', 'listen:copilot-state', 'listen:live-timeline-update']);
    await copilotApiFor(fake).ask(null, 'q');
    expect(calls).toContain('invoke:copilot_ask');
  });
});
