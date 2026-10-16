import { describe, expect, test } from 'bun:test';
import { liveModelCardStatus } from './LiveTranscriptionModelSettings';
import type {
  LiveModelDownloadProgressEvent,
  LiveTranscriptionSettings,
} from '@/lib/liveTranscriptionModels';

function settings(downloaded: boolean, downloading = false): LiveTranscriptionSettings {
  return {
    liveModel: 'nemotron-streaming',
    autoFinalPass: true,
    liveModelStatus: {
      modelId: 'nemotron',
      downloaded,
      downloading,
      loaded: false,
      sizeBytes: 662 * 1024 * 1024,
    },
  };
}

function progress(
  status: LiveModelDownloadProgressEvent['status'],
  percent = 0,
  error: string | null = null,
): LiveModelDownloadProgressEvent {
  return {
    modelName: 'nemotron',
    progress: percent,
    downloaded_mb: 0,
    total_mb: 662,
    speed_mbps: 0,
    eta_seconds: null,
    status,
    error,
  };
}

describe('liveModelCardStatus', () => {
  test('reflects the stored download state before any event', () => {
    expect(liveModelCardStatus(null, null)).toBe('Missing');
    expect(liveModelCardStatus(settings(false), null)).toBe('Missing');
    expect(liveModelCardStatus(settings(true), null)).toBe('Available');
    expect(liveModelCardStatus(settings(false, true), null)).toEqual({ Downloading: { progress: 0 } });
  });

  test('progress events take precedence while downloading and on failure', () => {
    expect(liveModelCardStatus(settings(false), progress('downloading', 42))).toEqual({
      Downloading: { progress: 42 },
    });
    expect(liveModelCardStatus(settings(false), progress('error', 0, 'network'))).toEqual({
      Error: 'network',
    });
    expect(liveModelCardStatus(settings(false), progress('completed', 100))).toBe('Available');
    expect(liveModelCardStatus(settings(false), progress('cancelled'))).toBe('Missing');
  });
});
