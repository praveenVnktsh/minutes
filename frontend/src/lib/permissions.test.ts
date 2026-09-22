import { afterAll, describe, expect, mock, test } from 'bun:test';

// Restore the real modules afterwards so a mocked `invoke` or `platform` cannot
// leak into a later file.
const originalCore = { ...(await import('@tauri-apps/api/core')) };
const originalOs = { ...(await import('@tauri-apps/plugin-os')) };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/plugin-os', () => originalOs);
});

let invokeResult: () => Promise<unknown> = async () => undefined;
const invokeCalls: Array<[string, unknown]> = [];

mock.module('@tauri-apps/api/core', () => ({
  invoke: async (command: string, args?: unknown) => {
    invokeCalls.push([command, args]);
    return await invokeResult();
  },
}));

// openPermissionSettings is macOS-only, so the tests that exercise it need the
// platform to say so.
let platformName = 'macos';
mock.module('@tauri-apps/plugin-os', () => ({ platform: () => platformName }));

// The permissions module must load after the Tauri mocks are registered.
const {
  PERMISSION_PANES,
  checkMicrophonePermission,
  checkSystemAudioPermission,
  isMacOS,
  openPermissionSettings,
  verdictToStatus,
} = await import('./permissions');

describe('openPermissionSettings', () => {
  test('invokes open_system_settings with the microphone pane', async () => {
    platformName = 'macos';
    invokeCalls.length = 0;
    invokeResult = async () => undefined;

    expect(await openPermissionSettings(PERMISSION_PANES.microphone)).toBe(true);

    expect(invokeCalls).toEqual([
      ['open_system_settings', { preferencePane: 'Privacy_Microphone' }],
    ]);
  });

  test('invokes open_system_settings with the system audio pane', async () => {
    platformName = 'macos';
    invokeCalls.length = 0;
    invokeResult = async () => undefined;

    expect(await openPermissionSettings(PERMISSION_PANES.systemAudio)).toBe(true);

    expect(invokeCalls).toEqual([
      ['open_system_settings', { preferencePane: 'Privacy_ScreenCapture' }],
    ]);
  });

  // FR-04's regression test: both onboarding call sites used to invoke
  // open_system_settings with no argument object at all, so deserialisation
  // failed on the Rust side. Asserting the exact payload shape for every pane
  // is what would have caught that divergence.
  test('always sends a preferencePane argument object, for every pane', async () => {
    platformName = 'macos';
    invokeCalls.length = 0;
    invokeResult = async () => undefined;

    for (const pane of Object.values(PERMISSION_PANES)) {
      await openPermissionSettings(pane);
    }

    expect(invokeCalls).toEqual(
      Object.values(PERMISSION_PANES).map((pane) => ['open_system_settings', { preferencePane: pane }]),
    );
  });

  test('resolves false rather than throwing when the invoke rejects', async () => {
    platformName = 'macos';
    invokeResult = async () => {
      throw new Error('open_system_settings is not registered');
    };

    expect(await openPermissionSettings(PERMISSION_PANES.microphone)).toBe(false);
  });

  test('does nothing off macOS, where there is no pane to open', async () => {
    platformName = 'windows';
    invokeCalls.length = 0;
    invokeResult = async () => undefined;

    expect(await openPermissionSettings(PERMISSION_PANES.systemAudio)).toBe(false);
    expect(invokeCalls).toEqual([]);
  });
});

describe('checkMicrophonePermission', () => {
  test('returns the report the backend gave', async () => {
    platformName = 'macos';
    invokeCalls.length = 0;
    invokeResult = async () => ({ verdict: 'authorized', detail: null });

    expect(await checkMicrophonePermission()).toEqual({ verdict: 'authorized', detail: null });
    expect(invokeCalls).toEqual([['trigger_microphone_permission', undefined]]);
  });

  test('returns undetermined, not denied, when the invoke rejects', async () => {
    invokeResult = async () => {
      throw new Error('trigger_microphone_permission is not registered');
    };

    const report = await checkMicrophonePermission();
    expect(report.verdict).toBe('undetermined');
    expect(report.detail).toBe('trigger_microphone_permission is not registered');
  });
});

describe('checkSystemAudioPermission', () => {
  test('returns the report the backend gave', async () => {
    invokeCalls.length = 0;
    invokeResult = async () => ({ verdict: 'denied', detail: 'no audio callback observed' });

    expect(await checkSystemAudioPermission()).toEqual({
      verdict: 'denied',
      detail: 'no audio callback observed',
    });
    expect(invokeCalls).toEqual([['trigger_system_audio_permission_command', undefined]]);
  });

  test('returns undetermined, not denied, when the invoke rejects', async () => {
    invokeResult = async () => {
      throw new Error('command failed');
    };

    const report = await checkSystemAudioPermission();
    expect(report.verdict).toBe('undetermined');
    expect(report.detail).toBe('command failed');
  });
});

describe('verdictToStatus', () => {
  test('maps every wire verdict to its row status', () => {
    expect(verdictToStatus('authorized')).toBe('authorized');
    expect(verdictToStatus('denied')).toBe('denied');
    expect(verdictToStatus('undetermined')).toBe('undetermined');
  });
});

describe('isMacOS', () => {
  test('reads the platform from @tauri-apps/plugin-os', async () => {
    platformName = 'macos';
    expect(await isMacOS()).toBe(true);

    platformName = 'windows';
    expect(await isMacOS()).toBe(false);
  });
});
