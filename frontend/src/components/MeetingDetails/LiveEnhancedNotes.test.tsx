import { afterEach, describe, expect, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { LiveSourceProvider, type LiveEvent, type LiveSource } from '@/lib/liveSource';
import type { LiveEnhancedSnapshot } from '@/lib/liveEnhanced';
import { LiveEnhancedNotes } from './LiveEnhancedNotes';

type Handler = (event: LiveEvent<unknown>) => void;

function fakeSource(current: LiveEnhancedSnapshot | null) {
  const handlers = new Map<string, Set<Handler>>();
  const source: LiveSource = {
    kind: 'replay',
    async listen<T>(event: string, handler: (event: LiveEvent<T>) => void) {
      const set = handlers.get(event) ?? new Set<Handler>();
      set.add(handler as Handler);
      handlers.set(event, set);
      return () => set.delete(handler as Handler);
    },
    async invoke<T>(command: string) {
      if (command !== 'get_live_enhanced') throw new Error(`unexpected ${command}`);
      return current as T;
    },
  };
  const emit = (payload: LiveEnhancedSnapshot) =>
    handlers.get('live-enhanced-update')?.forEach((handler) => handler({ payload }));
  return { source, emit };
}

function snapshot(partial: Partial<LiveEnhancedSnapshot>): LiveEnhancedSnapshot {
  return { session_id: 's', meeting_id: 'm', sections: [], tail: '', ...partial };
}

let renderer: ReactTestRenderer | undefined;
afterEach(() => {
  act(() => renderer?.unmount());
  renderer = undefined;
});

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function text(): string {
  const json = JSON.stringify(renderer?.toJSON());
  return json;
}

describe('LiveEnhancedNotes', () => {
  test('says where the notes will appear before any topic closes', async () => {
    const { source } = fakeSource(null);
    await act(async () => {
      renderer = create(<LiveSourceProvider source={source}><LiveEnhancedNotes /></LiveSourceProvider>);
    });
    await flush();
    expect(text()).toContain('Notes appear here as the meeting moves from topic to topic.');
  });

  test('closed topics show their notes and the current one is writing', async () => {
    const { source, emit } = fakeSource(snapshot({
      sections: [{ title: 'Pricing', body: '', writing: true }],
    }));
    await act(async () => {
      renderer = create(<LiveSourceProvider source={source}><LiveEnhancedNotes /></LiveSourceProvider>);
    });
    await flush();
    expect(text()).toContain('Pricing');
    expect(text()).toContain('Writing…');

    await act(async () => {
      emit(snapshot({
        sections: [
          { title: 'Pricing', body: '- Forty per seat [02:05]', writing: false },
          { title: 'Hiring', body: '', writing: true },
        ],
        tail: '## Decisions\n\n- Annual plan first [03:00]',
      }));
    });
    const rendered = text();
    expect(rendered).toContain('Forty per seat');
    expect(rendered).toContain('Hiring');
    expect(rendered).toContain('Decisions');
    // The [MM:SS] link is a button that jumps the transcript.
    const buttons = renderer!.root.findAll((node) => node.type === 'button');
    expect(buttons.map((button) => button.props.title)).toContain('Show this moment in the transcript');
    expect(rendered.match(/Writing…/g)?.length).toBe(1);
  });
});
