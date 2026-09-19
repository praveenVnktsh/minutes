import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalConfig = { ...await import('../../src/contexts/ConfigContext') };
const originalModelSettings = { ...await import('../../src/components/ModelSettingsModal') };
const originalToast = { ...await import('sonner') };

let handler: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
const invoke = mock((command: string, args?: Record<string, unknown>) => handler(command, args));
const notify = mock(() => {});
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('../../src/contexts/ConfigContext', () => ({
  useConfig: () => ({ isModelConfigSaving: false }),
}));
mock.module('../../src/components/ModelSettingsModal', () => ({
  ModelSettingsModal: () => null,
}));
mock.module('sonner', () => ({ toast: { success: notify, error: notify } }));

const { MeetingAssistantPanel } = await import('../../src/components/MeetingDetails/MeetingAssistantPanel');

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('../../src/contexts/ConfigContext', () => originalConfig);
  mock.module('../../src/components/ModelSettingsModal', () => originalModelSettings);
  mock.module('sonner', () => originalToast);
});

let renderer: ReactTestRenderer | undefined;
beforeEach(() => {
  invoke.mockClear();
  notify.mockClear();
  handler = async (command) => command === 'get_meeting_chat' ? [] : undefined;
});
afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

const modelConfig = { provider: 'ollama', model: 'test', whisperModel: 'base' } as const;

describe('MeetingAssistantPanel operation recovery', () => {
  test('distinguishes a history read failure from empty and retries it', async () => {
    let fail = true;
    handler = async (command) => {
      if (command === 'get_meeting_chat') {
        if (fail) throw new Error('database busy');
        return [];
      }
      throw new Error(`Unexpected command: ${command}`);
    };
    await act(async () => {
      renderer = create(<MeetingAssistantPanel meetingId="history-error" modelConfig={modelConfig} onNotesUpdated={() => {}} />);
    });
    expect(JSON.stringify(renderer!.toJSON())).toContain('database busy');
    expect(JSON.stringify(renderer!.toJSON())).not.toContain('Ask about this meeting');

    fail = false;
    const retry = renderer!.root.findAllByType('button').find((button) => button.children.includes('Retry'))!;
    await act(async () => retry.props.onClick());
    expect(JSON.stringify(renderer!.toJSON())).toContain('Ask about this meeting');
  });

  test('delivers a pending result once to the current callbacks after remount', async () => {
    let resolveChat!: (value: unknown) => void;
    const chat = new Promise((resolve) => { resolveChat = resolve; });
    handler = async (command) => {
      if (command === 'get_meeting_chat') return [];
      if (command === 'chat_with_meeting') return chat;
      throw new Error(`Unexpected command: ${command}`);
    };
    const oldNotes = mock(() => {});
    const newNotes = mock(() => {});
    const oldTranscript = mock(async () => {});
    const newTranscript = mock(async () => {});
    await act(async () => {
      renderer = create(<MeetingAssistantPanel meetingId="pending-success" modelConfig={modelConfig} onNotesUpdated={oldNotes} onTranscriptUpdated={oldTranscript} />);
    });
    const textarea = renderer!.root.findByType('textarea');
    await act(async () => textarea.props.onChange({ target: { value: 'Revise notes' } }));
    await act(async () => { void renderer!.root.findByType('form').props.onSubmit({ preventDefault() {} }); });
    await act(async () => renderer!.unmount());
    renderer = undefined;
    await act(async () => {
      renderer = create(<MeetingAssistantPanel meetingId="pending-success" modelConfig={modelConfig} onNotesUpdated={newNotes} onTranscriptUpdated={newTranscript} />);
    });
    await act(async () => resolveChat({
      message: { id: 'answer', role: 'assistant', content: 'Updated', createdAt: '2026-09-19' },
      notesMarkdown: 'Current notes',
      transcriptEditsApplied: 1,
    }));

    expect(oldNotes).not.toHaveBeenCalled();
    expect(oldTranscript).not.toHaveBeenCalled();
    expect(newNotes).toHaveBeenCalledTimes(1);
    expect(newNotes).toHaveBeenCalledWith('Current notes');
    expect(newTranscript).toHaveBeenCalledTimes(1);
    expect(renderer!.root.findByType('textarea').props.value).toBe('');
  });

  test('does not clear text typed after restoring a submitted draft', async () => {
    let resolveChat!: (value: unknown) => void;
    const chat = new Promise((resolve) => { resolveChat = resolve; });
    handler = async (command) => {
      if (command === 'get_meeting_chat') return [];
      if (command === 'chat_with_meeting') return chat;
      throw new Error(`Unexpected command: ${command}`);
    };
    await act(async () => {
      renderer = create(<MeetingAssistantPanel meetingId="pending-new-draft" modelConfig={modelConfig} onNotesUpdated={() => {}} />);
    });
    await act(async () => renderer!.root.findByType('textarea').props.onChange({ target: { value: 'Submitted' } }));
    await act(async () => { void renderer!.root.findByType('form').props.onSubmit({ preventDefault() {} }); });
    await act(async () => renderer!.unmount());
    renderer = undefined;
    await act(async () => {
      renderer = create(<MeetingAssistantPanel meetingId="pending-new-draft" modelConfig={modelConfig} onNotesUpdated={() => {}} />);
    });
    await act(async () => renderer!.root.findByType('textarea').props.onChange({ target: { value: 'Next question' } }));
    await act(async () => resolveChat({
      message: { id: 'answer', role: 'assistant', content: 'Done', createdAt: '2026-09-19' },
      transcriptEditsApplied: 0,
    }));
    expect(renderer!.root.findByType('textarea').props.value).toBe('Next question');
  });

  test('keeps a failed pending draft when the panel remounts', async () => {
    let rejectChat!: (error: unknown) => void;
    const chat = new Promise((_resolve, reject) => { rejectChat = reject; });
    handler = async (command) => {
      if (command === 'get_meeting_chat') return [];
      if (command === 'chat_with_meeting') return chat;
      throw new Error(`Unexpected command: ${command}`);
    };
    await act(async () => {
      renderer = create(<MeetingAssistantPanel meetingId="pending-error" modelConfig={modelConfig} onNotesUpdated={() => {}} />);
    });
    await act(async () => renderer!.root.findByType('textarea').props.onChange({ target: { value: 'Do not lose me' } }));
    await act(async () => { void renderer!.root.findByType('form').props.onSubmit({ preventDefault() {} }); });
    await act(async () => renderer!.unmount());
    renderer = undefined;
    await act(async () => {
      renderer = create(<MeetingAssistantPanel meetingId="pending-error" modelConfig={modelConfig} onNotesUpdated={() => {}} />);
    });
    await act(async () => rejectChat(new Error('provider unavailable')));
    expect(renderer!.root.findByType('textarea').props.value).toBe('Do not lose me');
    expect(renderer!.root.findAllByProps({ role: 'alert' }).some((node: any) => node.children.join('').includes('provider unavailable'))).toBe(true);
  });
});
