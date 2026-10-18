import { describe, expect, test } from 'bun:test';
import { act, create } from 'react-test-renderer';
import { LiveSourceProvider } from '@/lib/liveSource';
import { setShowAiNotes } from '@/lib/aiNotes';
import { buildIndex, parseJsonl } from '@/lib/replay/reducer';
import { ReplaySource } from '@/lib/replay/replaySource';
import { ReplayNotes } from './ReplayWorkspace';

const records = parseJsonl([
  { seq: 0, t_ms: 0, audio_ms: 0, type: 'meta', name: 'run-start', payload: { schema_version: 1 } },
  {
    seq: 1, t_ms: 10, audio_ms: 64_000, type: 'event', name: 'live-timeline-update',
    payload: {
      session_id: 's', meeting_id: 'm', status: 'listening', error: null,
      entries: [
        { id: 'a', start_ms: 5_000, end_ms: 9_000, kind: 'point', text: 'Sync bug is the last blocker', speaker: null, provider: 'p', model: 'm', created_at: '', topic: 'Launch Plan' },
        { id: 'b', start_ms: 62_000, end_ms: 70_000, kind: 'decision', text: 'Free tier held back', speaker: null, provider: 'p', model: 'm', created_at: '', topic: 'Pricing' },
      ],
    },
  },
].map((record) => JSON.stringify(record)).join('\n'));

const backend = { ask: async () => {}, stop: async () => {} };

describe('the replay renders the one notes document', () => {
  test('typed notes, then the live notes as AI lines under their topics; hidden with Show AI notes off', async () => {
    const source = new ReplaySource(buildIndex(records, 50), backend);
    source.seek(records.length);
    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(
        <LiveSourceProvider source={source}>
          <ReplayNotes notes="- My own line" />
        </LiveSourceProvider>,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const text = () => JSON.stringify(renderer.toJSON());
    expect(text()).toContain('- My own line');
    expect(renderer.root.findAll((node) => node.props['data-ai-topic'] !== undefined).map((node) => node.props['data-ai-topic']))
      .toEqual(['Launch Plan', 'Pricing']);
    expect(text()).toContain('Decision: ');
    expect(text()).toContain('Free tier held back');

    await act(async () => setShowAiNotes(false));
    expect(text()).toContain('- My own line');
    expect(text()).not.toContain('Sync bug is the last blocker');
    expect(text()).not.toContain('Launch Plan');

    await act(async () => setShowAiNotes(true));
    await act(async () => renderer.unmount());
  });
});
