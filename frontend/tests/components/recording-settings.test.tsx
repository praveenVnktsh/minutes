import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create } from 'react-test-renderer';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalConfig = { ...await import('../../src/contexts/ConfigContext') };
const originalRecording = { ...await import('../../src/contexts/RecordingStateContext') };
const originalAnalytics = { ...await import('../../src/lib/analytics') };
const originalDevices = { ...await import('../../src/components/DeviceSelection') };
const originalStore = { ...await import('@tauri-apps/plugin-store') };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('../../src/contexts/ConfigContext', () => originalConfig);
  mock.module('../../src/contexts/RecordingStateContext', () => originalRecording);
  mock.module('../../src/lib/analytics', () => originalAnalytics);
  mock.module('../../src/components/DeviceSelection', () => originalDevices);
  mock.module('@tauri-apps/plugin-store', () => originalStore);
});

const preferences = {
  save_folder: '/recordings', auto_save: true, file_format: 'wav', automatic_record_prompt: true,
  min_meeting_duration_seconds: 10, preferred_mic_device: null, preferred_system_device: null,
};
const invoke = mock(async (command: string) => {
  if (command === 'get_recording_preferences') return preferences;
  if (command === 'set_recording_preferences') throw new Error('disk unavailable');
  return null;
});
let storeMemory = true;
let storeDisk = true;
let getStored = async () => storeMemory;
let saveStored = async () => { storeDisk = storeMemory; };
const store = {
  get: async () => getStored(),
  set: async (_key: string, value: unknown) => { storeMemory = Boolean(value); },
  save: async () => saveStored(),
};
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('../../src/contexts/ConfigContext', () => ({ useConfig: () => ({ setSelectedDevices() {} }) }));
mock.module('../../src/contexts/RecordingStateContext', () => ({ useRecordingState: () => ({ isRecording: false }) }));
mock.module('../../src/lib/analytics', () => ({ default: { track: async () => {} } }));
mock.module('../../src/components/DeviceSelection', () => ({ DeviceSelection: () => null }));
mock.module('@tauri-apps/plugin-store', () => ({ Store: { load: async () => store } }));

const { RecordingSettings } = await import('../../src/components/RecordingSettings');

describe('recording preference feedback', () => {
  beforeEach(() => {
    storeMemory = true;
    storeDisk = true;
    getStored = async () => storeMemory;
    saveStored = async () => { storeDisk = storeMemory; };
  });

  test('rolls back a rejected recording policy and names the attempted operation', async () => {
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<RecordingSettings />); });
    await act(async () => { await Promise.resolve(); });
    const toggle = renderer!.root.findByProps({ 'aria-label': 'Save audio recordings' });

    await act(async () => { await toggle.props.onCheckedChange(false); });

    expect(toggle.props.checked).toBe(true);
    expect(JSON.stringify(renderer!.toJSON())).toContain('Could not save audio recording preference; previous setting restored');
    renderer!.unmount();
  });

  test('restores the participant reminder cache and UI after a rejected save', async () => {
    let saves = 0;
    saveStored = async () => {
      saves += 1;
      if (saves === 1) throw new Error('store unavailable');
      storeDisk = storeMemory;
    };
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<RecordingSettings />); await Promise.resolve(); });
    const toggle = renderer!.root.findByProps({ 'aria-label': 'Remind me to inform participants when recording starts' });
    await act(async () => { await toggle.props.onCheckedChange(false); });

    expect(toggle.props.checked).toBe(true);
    expect(storeMemory).toBe(true);
    expect(storeDisk).toBe(true);
    expect(JSON.stringify(renderer!.toJSON())).toContain('previous setting restored');
    renderer!.unmount();
  });

  test('ignores a delayed initial reminder read after the user changes the setting', async () => {
    let resolveGet!: (value: boolean) => void;
    getStored = () => new Promise<boolean>(resolve => { resolveGet = resolve; });
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<RecordingSettings />); await Promise.resolve(); });
    const toggle = renderer!.root.findByProps({ 'aria-label': 'Remind me to inform participants when recording starts' });
    await act(async () => { await toggle.props.onCheckedChange(false); });
    await act(async () => resolveGet(true));

    expect(toggle.props.checked).toBe(false);
    expect(storeDisk).toBe(false);
    renderer!.unmount();
  });

  test('reports when restoring the participant reminder cache also fails', async () => {
    saveStored = async () => { throw new Error('store unavailable'); };
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<RecordingSettings />); await Promise.resolve(); });
    const toggle = renderer!.root.findByProps({ 'aria-label': 'Remind me to inform participants when recording starts' });
    await act(async () => { await toggle.props.onCheckedChange(false); });

    expect(toggle.props.checked).toBe(true);
    expect(storeMemory).toBe(true);
    expect(JSON.stringify(renderer!.toJSON())).toContain('Could not save or restore the participant reminder');
    renderer!.unmount();
  });
});
