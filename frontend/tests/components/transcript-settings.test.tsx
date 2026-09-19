import { afterAll, describe, expect, mock, test } from 'bun:test';
import type { ReactNode } from 'react';
import { act, create } from 'react-test-renderer';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalWhisper = { ...await import('../../src/components/WhisperModelManager') };
const originalParakeet = { ...await import('../../src/components/ParakeetModelManager') };
const originalSelect = { ...await import('../../src/components/ui/select') };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('../../src/components/WhisperModelManager', () => originalWhisper);
  mock.module('../../src/components/ParakeetModelManager', () => originalParakeet);
  mock.module('../../src/components/ui/select', () => originalSelect);
});

const invoke = mock(async (command: string) => {
  if (command === 'api_get_transcription_vocabulary') return 'Minutes';
  if (command === 'api_set_transcription_vocabulary') return null;
  throw new Error(`Unexpected command: ${command}`);
});
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('../../src/components/WhisperModelManager', () => ({ ModelManager: () => null }));
mock.module('../../src/components/ParakeetModelManager', () => ({ ParakeetModelManager: () => null }));
mock.module('../../src/components/ui/select', () => ({
  Select: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  SelectTrigger: ({ children }: { children?: ReactNode }) => <button type="button">{children}</button>,
  SelectValue: () => null,
  SelectContent: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  SelectItem: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
}));

const { TranscriptSettings } = await import('../../src/components/TranscriptSettings');

describe('transcription vocabulary feedback', () => {
  test('returns to unsaved after editing a successfully saved vocabulary', async () => {
    let renderer: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(
        <TranscriptSettings
          transcriptModelConfig={{ provider: 'localWhisper', model: 'large-v3', apiKey: null }}
          setTranscriptModelConfig={() => {}}
        />,
      );
      await Promise.resolve();
    });
    const textarea = renderer!.root.findByProps({ id: 'transcription-vocabulary' });
    act(() => textarea.props.onChange({ target: { value: 'Minutes, Acme' } }));
    const save = renderer!.root.findAllByType('button').find(button => button.children.includes('Save vocabulary'))!;
    await act(async () => { await save.props.onClick(); });
    expect(JSON.stringify(renderer!.toJSON())).toContain('Vocabulary saved');

    act(() => textarea.props.onChange({ target: { value: 'Minutes, Acme, Kubernetes' } }));
    const rendered = JSON.stringify(renderer!.toJSON());
    expect(rendered).toContain('Vocabulary changes not saved');
    expect(rendered).not.toContain('Vocabulary saved');
    renderer!.unmount();
  });
});
