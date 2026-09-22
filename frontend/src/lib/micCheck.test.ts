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

// The settings fixes are macOS-only, so the tests that exercise them need the
// platform to say so.
let platformName = 'macos';
mock.module('@tauri-apps/plugin-os', () => ({ platform: () => platformName }));

// The setup check module must load after the Tauri mocks are registered.
const {
  CHANNEL_OUTCOMES,
  MODEL_CHECK_OUTCOMES,
  SETUP_CHECK_CHANNELS,
  SETUP_CHECK_DURATION_SECONDS,
  applySetupCheckFix,
  cancelSetupCheck,
  describeChannelOutcome,
  describeModelOutcome,
  skippedSetupCheckRecord,
  startSetupCheck,
  summariseSetupCheck,
} = await import('./micCheck');

type ChannelOutcome = (typeof CHANNEL_OUTCOMES)[number];
type ModelCheckOutcome = (typeof MODEL_CHECK_OUTCOMES)[number];
type SetupCheckChannel = (typeof SETUP_CHECK_CHANNELS)[number];
type SetupCheckResult = Awaited<ReturnType<typeof startSetupCheck>>;
type ChannelReport = SetupCheckResult['microphone'];
type ModelReport = SetupCheckResult['model'];

const DEFAULT_DEVICE: Record<SetupCheckChannel, string> = {
  microphone: 'Scarlett Solo USB',
  system_audio: 'BlackHole 2ch',
};

function channelReport(
  channel: SetupCheckChannel,
  outcome: ChannelOutcome,
  deviceName = DEFAULT_DEVICE[channel],
): ChannelReport {
  return {
    channel,
    outcome,
    transcript: outcome === 'transcribed' ? 'testing one two three' : null,
    deviceName,
    peakLevel: 0.4,
    durationMs: 8000,
    detail: null,
  };
}

function modelReport(outcome: ModelCheckOutcome): ModelReport {
  return {
    outcome,
    modelName: outcome === 'loaded' ? 'parakeet-tdt-0.6b-v3' : null,
    detail: null,
  };
}

function resultFor(
  model: ModelCheckOutcome,
  microphone: ChannelOutcome,
  systemAudio: ChannelOutcome,
): SetupCheckResult {
  return {
    model: modelReport(model),
    microphone: channelReport('microphone', microphone),
    systemAudio: channelReport('system_audio', systemAudio),
    durationMs: 8000,
    cancelled: microphone === 'cancelled',
  };
}

/** The outcomes whose cause is the device, and so must say which device. */
const NAMES_THE_DEVICE: ChannelOutcome[] = [
  'transcribed',
  'no_speech_detected',
  'no_audio_detected',
  'permission_denied',
  'device_unavailable',
  'transcription_failed',
];

describe('describeChannelOutcome', () => {
  for (const channel of SETUP_CHECK_CHANNELS) {
    test(`gives every ${channel} outcome its own wording`, () => {
      const titles = new Set<string>();
      const messages = new Set<string>();

      for (const outcome of CHANNEL_OUTCOMES) {
        const { title, message } = describeChannelOutcome(channelReport(channel, outcome));

        expect(title.trim().length).toBeGreaterThan(0);
        expect(message.trim().length).toBeGreaterThan(0);
        titles.add(title);
        messages.add(message);
      }

      // A generic failure message shared between outcomes is the thing this step
      // exists to avoid, so no two may collide.
      expect(titles.size).toBe(CHANNEL_OUTCOMES.length);
      expect(messages.size).toBe(CHANNEL_OUTCOMES.length);
    });

    test(`names the ${channel} device the check opened`, () => {
      for (const outcome of NAMES_THE_DEVICE) {
        const { message } = describeChannelOutcome(channelReport(channel, outcome));

        expect(message).toContain(DEFAULT_DEVICE[channel]);
      }
    });
  }

  test('falls back to a generic name for each channel when Rust never got one', () => {
    expect(
      describeChannelOutcome(channelReport('microphone', 'no_audio_detected', '   ')).message,
    ).toContain('your microphone');
    expect(
      describeChannelOutcome(channelReport('system_audio', 'device_unavailable', '   ')).message,
    ).toContain('your system audio');
  });

  // The whole reason this ticket split one check into two channels: the same
  // outcome means a different thing depending on which side it came from.
  test('words silence differently for the microphone and for system audio', () => {
    const mic = describeChannelOutcome(channelReport('microphone', 'no_audio_detected'));
    const system = describeChannelOutcome(channelReport('system_audio', 'no_audio_detected'));

    expect(mic.title).not.toBe(system.title);
    expect(mic.message).not.toBe(system.message);
  });

  test('words a withheld permission differently for the microphone and for system audio', () => {
    const mic = describeChannelOutcome(channelReport('microphone', 'permission_denied'));
    const system = describeChannelOutcome(channelReport('system_audio', 'permission_denied'));

    expect(mic.title).not.toBe(system.title);
    expect(mic.message).not.toBe(system.message);
    expect(mic.message).toContain('Microphone');
    expect(mic.fix).toEqual({ label: 'Open microphone settings', kind: 'open-mic-settings' });
    expect(system.message).toContain('Screen Recording');
    expect(system.fix?.kind).toBe('open-screen-recording-settings');
  });

  test('treats a quiet computer as a result rather than a failure', () => {
    const { message, fix } = describeChannelOutcome(
      channelReport('system_audio', 'no_audio_detected'),
    );

    // No fix button, because there is nothing to fix; instead the message points
    // at the system-audio route for the user who did expect to hear something.
    expect(fix).toBeUndefined();
    expect(message).toMatch(/screen recording/i);
    expect(message).toMatch(/loopback/i);
    expect(message).not.toMatch(/fail|error|went wrong/i);
  });

  test('offers another device when a channel is silent or unopenable', () => {
    expect(
      describeChannelOutcome(channelReport('microphone', 'no_audio_detected')).fix?.kind,
    ).toBe('choose-device');
    expect(
      describeChannelOutcome(channelReport('microphone', 'device_unavailable')).fix?.kind,
    ).toBe('choose-device');
    expect(
      describeChannelOutcome(channelReport('system_audio', 'device_unavailable')).fix?.kind,
    ).toBe('choose-device');
  });

  test('offers no button for something the user cannot fix', () => {
    for (const channel of SETUP_CHECK_CHANNELS) {
      expect(describeChannelOutcome(channelReport(channel, 'transcribed')).fix).toBeUndefined();
      expect(describeChannelOutcome(channelReport(channel, 'unsupported')).fix).toBeUndefined();
      // `not_run` is the model's problem, and the model card carries its fix.
      expect(describeChannelOutcome(channelReport(channel, 'not_run')).fix).toBeUndefined();
    }
  });

  test('sends an untested channel back to the model', () => {
    for (const channel of SETUP_CHECK_CHANNELS) {
      expect(describeChannelOutcome(channelReport(channel, 'not_run')).message).toContain('model');
    }
  });

  test('keeps the raw error out of the message', () => {
    const report = {
      ...channelReport('microphone', 'transcription_failed'),
      detail: 'onnxruntime: failed to allocate',
    };

    expect(describeChannelOutcome(report).message).not.toContain('onnxruntime');
  });
});

describe('describeModelOutcome', () => {
  test('gives every model outcome its own wording', () => {
    const titles = new Set<string>();
    const messages = new Set<string>();

    for (const outcome of MODEL_CHECK_OUTCOMES) {
      const { title, message } = describeModelOutcome(modelReport(outcome));

      expect(title.trim().length).toBeGreaterThan(0);
      expect(message.trim().length).toBeGreaterThan(0);
      titles.add(title);
      messages.add(message);
    }

    expect(titles.size).toBe(MODEL_CHECK_OUTCOMES.length);
    expect(messages.size).toBe(MODEL_CHECK_OUTCOMES.length);
  });

  test('names the model it loaded and asks for nothing', () => {
    const { message, fix } = describeModelOutcome(modelReport('loaded'));

    expect(message).toContain('parakeet-tdt-0.6b-v3');
    expect(fix).toBeUndefined();
  });

  test('offers the download again when the model is missing', () => {
    expect(describeModelOutcome(modelReport('unavailable')).fix).toEqual({
      label: 'Download the model again',
      kind: 'retry-download',
    });
  });

  test('names both real causes when the model would not load', () => {
    const { message, fix } = describeModelOutcome(modelReport('failed'));

    expect(message).toMatch(/damaged/i);
    expect(message).toMatch(/memory/i);
    expect(fix?.kind).toBe('retry');
  });

  test('offers a retry when the command itself never ran', () => {
    expect(describeModelOutcome(modelReport('command_failed')).fix?.kind).toBe('retry');
  });

  test('keeps the raw error out of the message', () => {
    const report = { ...modelReport('failed'), detail: 'onnxruntime: failed to allocate' };

    expect(describeModelOutcome(report).message).not.toContain('onnxruntime');
  });
});

describe('startSetupCheck', () => {
  test('passes both devices and the duration to the command', async () => {
    invokeCalls.length = 0;
    invokeResult = async () => resultFor('loaded', 'transcribed', 'no_audio_detected');

    await startSetupCheck({
      micDeviceName: 'Scarlett Solo USB',
      systemDeviceName: 'BlackHole 2ch',
      durationSecs: 5,
    });

    expect(invokeCalls).toEqual([
      [
        'mic_check_start',
        {
          micDeviceName: 'Scarlett Solo USB',
          systemDeviceName: 'BlackHole 2ch',
          durationSecs: 5,
        },
      ],
    ]);
  });

  test('defaults to the default devices and the step duration', async () => {
    invokeCalls.length = 0;
    invokeResult = async () => resultFor('loaded', 'transcribed', 'no_audio_detected');

    await startSetupCheck();

    expect(invokeCalls).toEqual([
      [
        'mic_check_start',
        {
          micDeviceName: null,
          systemDeviceName: null,
          durationSecs: SETUP_CHECK_DURATION_SECONDS,
        },
      ],
    ]);
  });

  test('returns the result the command resolved with', async () => {
    invokeResult = async () => resultFor('loaded', 'transcribed', 'no_audio_detected');

    const result = await startSetupCheck({ micDeviceName: 'Scarlett Solo USB' });

    expect(result.microphone.outcome).toBe('transcribed');
    expect(result.microphone.transcript).toBe('testing one two three');
    expect(result.systemAudio.outcome).toBe('no_audio_detected');
  });

  test('reports a failed invoke as command_failed rather than rejecting', async () => {
    invokeResult = async () => {
      throw new Error('A setup check is already running');
    };

    const result = await startSetupCheck({
      micDeviceName: 'Scarlett Solo USB',
      systemDeviceName: 'BlackHole 2ch',
    });

    expect(result.model.outcome).toBe('command_failed');
    expect(result.microphone.outcome).toBe('command_failed');
    expect(result.systemAudio.outcome).toBe('command_failed');
    expect(result.microphone.deviceName).toBe('Scarlett Solo USB');
    expect(result.systemAudio.deviceName).toBe('BlackHole 2ch');
    expect(result.model.detail).toContain('already running');
    expect(describeModelOutcome(result.model).fix?.kind).toBe('retry');
    expect(describeChannelOutcome(result.microphone).fix?.kind).toBe('retry');
    expect(describeChannelOutcome(result.systemAudio).fix?.kind).toBe('retry');
  });
});

describe('cancelSetupCheck', () => {
  test('asks the command to stop the capture', async () => {
    invokeCalls.length = 0;
    invokeResult = async () => undefined;

    await cancelSetupCheck();

    expect(invokeCalls).toEqual([['mic_check_cancel', undefined]]);
  });

  test('stays quiet when there is nothing left to cancel', async () => {
    invokeResult = async () => {
      throw new Error('no check in flight');
    };

    expect(await cancelSetupCheck()).toBeUndefined();
  });
});

describe('summariseSetupCheck', () => {
  test('passes a working microphone on a quiet computer', () => {
    const record = summariseSetupCheck(resultFor('loaded', 'transcribed', 'no_audio_detected'));

    expect(record.status).toBe('passed');
    expect(record.model).toBe('loaded');
    expect(record.microphone).toBe('transcribed');
    expect(record.system_audio).toBe('no_audio_detected');
    expect(Number.isNaN(Date.parse(record.checked_at))).toBe(false);
  });

  test('passes even when this computer cannot capture system audio at all', () => {
    expect(summariseSetupCheck(resultFor('loaded', 'transcribed', 'unsupported')).status).toBe(
      'passed',
    );
  });

  test('records issues when the model never loaded', () => {
    const record = summariseSetupCheck(resultFor('failed', 'not_run', 'not_run'));

    expect(record.status).toBe('issues');
    expect(record.model).toBe('failed');
    expect(record.microphone).toBe('not_run');
  });

  test('records issues when the microphone sent silence', () => {
    expect(
      summariseSetupCheck(resultFor('loaded', 'no_audio_detected', 'transcribed')).status,
    ).toBe('issues');
  });
});

describe('skippedSetupCheckRecord', () => {
  test('keeps whatever the run had already found out', () => {
    const record = skippedSetupCheckRecord(
      resultFor('loaded', 'permission_denied', 'no_audio_detected'),
    );

    expect(record.status).toBe('skipped');
    expect(record.model).toBe('loaded');
    expect(record.microphone).toBe('permission_denied');
    expect(record.system_audio).toBe('no_audio_detected');
  });

  test('records not_run when the user walked straight past the check', () => {
    const record = skippedSetupCheckRecord();

    expect(record.status).toBe('skipped');
    expect(record.model).toBe('not_run');
    expect(record.microphone).toBe('not_run');
    expect(record.system_audio).toBe('not_run');
  });
});

describe('applySetupCheckFix', () => {
  test('opens the microphone pane and the screen recording pane', async () => {
    platformName = 'macos';
    invokeCalls.length = 0;
    invokeResult = async () => undefined;

    expect(await applySetupCheckFix('open-mic-settings')).toBe(true);
    expect(await applySetupCheckFix('open-screen-recording-settings')).toBe(true);

    expect(invokeCalls).toEqual([
      ['open_system_settings', { preferencePane: 'Privacy_Microphone' }],
      ['open_system_settings', { preferencePane: 'Privacy_ScreenCapture' }],
    ]);
  });

  test('leaves the step to its own buttons for the other fixes', async () => {
    platformName = 'macos';
    invokeCalls.length = 0;

    expect(await applySetupCheckFix('retry')).toBe(false);
    expect(await applySetupCheckFix('choose-device')).toBe(false);
    expect(await applySetupCheckFix('retry-download')).toBe(false);
    expect(invokeCalls).toEqual([]);
  });

  test('reports back when the settings pane could not be opened', async () => {
    platformName = 'macos';
    invokeResult = async () => {
      throw new Error('open_system_settings is not registered');
    };

    expect(await applySetupCheckFix('open-mic-settings')).toBe(false);
  });

  test('does nothing off macOS, where there is no pane to open', async () => {
    platformName = 'windows';
    invokeCalls.length = 0;
    invokeResult = async () => undefined;

    expect(await applySetupCheckFix('open-screen-recording-settings')).toBe(false);
    expect(invokeCalls).toEqual([]);
  });
});
