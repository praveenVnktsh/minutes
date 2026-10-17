import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create } from 'react-test-renderer';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalEvent = { ...await import('@tauri-apps/api/event') };
const originalConfig = { ...await import('../../src/contexts/ConfigContext') };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
  mock.module('../../src/contexts/ConfigContext', () => originalConfig);
});

let status: { type: string } = { type: 'not_downloaded' };
let choice = 'summary';
const calls: string[] = [];
const invoke = mock(async (command: string, args?: Record<string, unknown>) => {
  calls.push(command);
  switch (command) {
    case 'get_live_notes_settings':
    case 'set_live_notes_model':
      if (command === 'set_live_notes_model') choice = String(args?.model);
      return {
        model: choice,
        live_notes_model: 'gemma4:e2b',
        low_memory: false,
        opt_in_by_default: true,
        download_pending: false,
      };
    case 'builtin_ai_get_model_info':
      return {
        name: 'gemma4:e2b', display_name: 'Gemma 4 E2B (Live notes)', status, path: '',
        size_mb: 2963, size_bytes: 2963 * 1024 * 1024, context_size: 32768, description: '',
        gguf_file: 'gemma-4-E2B-it-Q4_K_M.gguf',
      };
    default:
      return null;
  }
});
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen: async () => () => {} }));
mock.module('../../src/contexts/ConfigContext', () => ({
  useConfig: () => ({ modelConfig: { provider: 'builtin-ai', model: 'qwen3.5:4b' } }),
}));

const { LiveNotesModelSettings } = await import('../../src/components/LiveNotesModelSettings');

type Node = { children: Array<Node | string> };
function textOf(node: Node | string): string {
  return typeof node === 'string' ? node : node.children.map(textOf).join('');
}

function button(renderer: ReturnType<typeof create>, label: string) {
  return renderer.root.findAllByType('button').find((b) => textOf(b as unknown as Node).includes(label));
}

async function render() {
  let renderer: ReturnType<typeof create>;
  await act(async () => { renderer = create(<LiveNotesModelSettings />); });
  await act(async () => { await Promise.resolve(); });
  return renderer!;
}

describe('live notes model settings', () => {
  beforeEach(() => {
    status = { type: 'not_downloaded' };
    choice = 'summary';
    calls.length = 0;
  });

  test('nothing downloads until Download is pressed, and Gemma cannot be picked before', async () => {
    const renderer = await render();
    expect(calls).not.toContain('builtin_ai_download_model');
    const control = renderer.root.findByProps({ 'aria-label': 'Live notes model' });
    const gemma = control.props.options.find((o: { value: string }) => o.value === 'gemma4:e2b');
    expect(gemma.disabled).toBe(true);
    expect(control.props.value).toBe('summary');

    const download = button(renderer, 'Download');
    await act(async () => { download!.props.onClick(); await Promise.resolve(); });
    expect(calls).toContain('builtin_ai_download_model');
    renderer.unmount();
  });

  test('a downloaded model can be chosen and removed, falling back to the summary model', async () => {
    status = { type: 'available' };
    const renderer = await render();
    const control = renderer.root.findByProps({ 'aria-label': 'Live notes model' });
    expect(control.props.options.every((o: { disabled: boolean }) => !o.disabled)).toBe(true);
    await act(async () => { await control.props.onValueChange('gemma4:e2b'); });
    expect(choice).toBe('gemma4:e2b');

    const remove = button(renderer, 'Remove');
    await act(async () => { await remove!.props.onClick(); });
    expect(choice).toBe('summary');
    expect(calls).toContain('builtin_ai_delete_model');
    renderer.unmount();
  });
});
