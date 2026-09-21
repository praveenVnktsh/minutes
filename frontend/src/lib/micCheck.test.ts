import { afterAll, describe, expect, mock, test } from 'bun:test';

// Restore the real module afterwards so a mocked `invoke` cannot leak into a
// later file.
const originalCore = { ...(await import('@tauri-apps/api/core')) };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
});

let invokeResult: () => Promise<unknown> = async () => undefined;
const invokeCalls: Array<[string, unknown]> = [];

mock.module('@tauri-apps/api/core', () => ({
  invoke: async (command: string, args?: unknown) => {
    invokeCalls.push([command, args]);
    return await invokeResult();
  },
}));

// The mic check module must load after the Tauri invoke mock is registered.
const {
  MIC_CHECK_DURATION_SECONDS,
  MIC_CHECK_OUTCOMES,
  cancelMicCheck,
  describeMicCheckOutcome,
  startMicCheck,
} = await import('./micCheck');
type MicCheckOutcome = (typeof MIC_CHECK_OUTCOMES)[number];
type MicCheckResult = Awaited<ReturnType<typeof startMicCheck>>;

function resultFor(outcome: MicCheckOutcome, deviceName = 'Scarlett Solo USB'): MicCheckResult {
  return {
    outcome,
    transcript: outcome === 'transcribed' ? 'testing one two three' : null,
    deviceName,
    peakLevel: 0.4,
    durationMs: 8000,
    detail: null,
  };
}

/** The outcomes whose cause is the device, and so must say which device. */
const NAMES_THE_DEVICE: MicCheckOutcome[] = [
  'transcribed',
  'no_speech_detected',
  'no_audio_detected',
  'permission_denied',
  'device_unavailable',
  'transcription_failed',
];

describe('describeMicCheckOutcome', () => {
  test('gives every outcome its own wording', () => {
    const titles = new Set<string>();
    const messages = new Set<string>();

    for (const outcome of MIC_CHECK_OUTCOMES) {
      const { title, message } = describeMicCheckOutcome(resultFor(outcome));

      expect(title.trim().length).toBeGreaterThan(0);
      expect(message.trim().length).toBeGreaterThan(0);
      titles.add(title);
      messages.add(message);
    }

    // A generic failure message shared between outcomes is the thing this step
    // exists to avoid, so no two may collide.
    expect(titles.size).toBe(MIC_CHECK_OUTCOMES.length);
    expect(messages.size).toBe(MIC_CHECK_OUTCOMES.length);
  });

  test('offers a fix for everything except success', () => {
    for (const outcome of MIC_CHECK_OUTCOMES) {
      const { fix } = describeMicCheckOutcome(resultFor(outcome));

      if (outcome === 'transcribed') {
        expect(fix).toBeUndefined();
      } else {
        expect(fix?.kind).toBeTruthy();
        expect(fix?.label.trim().length).toBeGreaterThan(0);
      }
    }
  });

  test('sends a denied microphone to the privacy settings', () => {
    const { message, fix } = describeMicCheckOutcome(resultFor('permission_denied'));

    expect(fix).toEqual({ label: 'Open microphone settings', kind: 'open-mic-settings' });
    expect(message).toContain('Microphone');
  });

  test('offers another device when the chosen one is silent or unopenable', () => {
    expect(describeMicCheckOutcome(resultFor('no_audio_detected')).fix?.kind).toBe('choose-device');
    expect(describeMicCheckOutcome(resultFor('device_unavailable')).fix?.kind).toBe(
      'choose-device',
    );
  });

  test('offers the download again when the model is missing', () => {
    expect(describeMicCheckOutcome(resultFor('model_unavailable')).fix?.kind).toBe(
      'retry-download',
    );
  });

  test('names the device the user chose', () => {
    for (const outcome of NAMES_THE_DEVICE) {
      const { message } = describeMicCheckOutcome(resultFor(outcome, 'Scarlett Solo USB'));

      expect(message).toContain('Scarlett Solo USB');
    }
  });

  test('falls back to a generic device when Rust never got a name', () => {
    const { message } = describeMicCheckOutcome(resultFor('no_audio_detected', '   '));

    expect(message).toContain('your microphone');
  });

  test('keeps the raw error out of the message', () => {
    const result = { ...resultFor('model_failed'), detail: 'onnxruntime: failed to allocate' };

    expect(describeMicCheckOutcome(result).message).not.toContain('onnxruntime');
  });
});

describe('startMicCheck', () => {
  test('passes the device and duration to the command', async () => {
    invokeCalls.length = 0;
    invokeResult = async () => resultFor('transcribed');

    await startMicCheck({ deviceName: 'Scarlett Solo USB (input)', durationSecs: 5 });

    expect(invokeCalls).toEqual([
      ['mic_check_start', { deviceName: 'Scarlett Solo USB (input)', durationSecs: 5 }],
    ]);
  });

  test('defaults to the default device and the step duration', async () => {
    invokeCalls.length = 0;
    invokeResult = async () => resultFor('transcribed');

    await startMicCheck();

    expect(invokeCalls).toEqual([
      ['mic_check_start', { deviceName: null, durationSecs: MIC_CHECK_DURATION_SECONDS }],
    ]);
  });

  test('returns the result the command resolved with', async () => {
    invokeResult = async () => resultFor('transcribed');

    const result = await startMicCheck({ deviceName: 'Scarlett Solo USB' });

    expect(result.outcome).toBe('transcribed');
    expect(result.transcript).toBe('testing one two three');
  });

  test('reports a failed invoke as command_failed rather than rejecting', async () => {
    invokeResult = async () => {
      throw new Error('A microphone check is already running');
    };

    const result = await startMicCheck({ deviceName: 'Scarlett Solo USB' });

    expect(result.outcome).toBe('command_failed');
    expect(result.deviceName).toBe('Scarlett Solo USB');
    expect(result.detail).toContain('already running');
    expect(describeMicCheckOutcome(result).fix?.kind).toBe('retry');
  });
});

describe('cancelMicCheck', () => {
  test('asks the command to stop the capture', async () => {
    invokeCalls.length = 0;
    invokeResult = async () => undefined;

    await cancelMicCheck();

    expect(invokeCalls).toEqual([['mic_check_cancel', undefined]]);
  });

  test('stays quiet when there is nothing left to cancel', async () => {
    invokeResult = async () => {
      throw new Error('no check in flight');
    };

    expect(await cancelMicCheck()).toBeUndefined();
  });
});
