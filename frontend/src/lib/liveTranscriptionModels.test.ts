import { describe, expect, test } from 'bun:test';
import {
  applyPartial,
  finalPassNotice,
  finalPassTookOver,
  orderedPartials,
  type FinalPassStarted,
} from './liveTranscriptionModels';

const queued: FinalPassStarted = {
  status: 'queued',
  taskId: 'task-1',
  skipReason: null,
  message: null,
  provider: 'localWhisper',
  model: 'large-v3-turbo-q5_0',
  fallbackNotice: null,
};

describe('finalPassTookOver', () => {
  test('only a queued pass takes over post-processing', () => {
    expect(finalPassTookOver(queued)).toBe(true);
    expect(finalPassTookOver({ ...queued, status: 'skipped' })).toBe(false);
    expect(finalPassTookOver(undefined)).toBe(false);
    expect(finalPassTookOver(null)).toBe(false);
    expect(finalPassTookOver('queued')).toBe(false);
  });
});

describe('finalPassNotice', () => {
  test('reports a fallback engine and meaningful skips, but not a disabled pass', () => {
    expect(finalPassNotice(queued)).toBeNull();
    expect(finalPassNotice({ ...queued, fallbackNotice: 'Whisper is not downloaded' })).toBe(
      'Whisper is not downloaded',
    );
    expect(
      finalPassNotice({ ...queued, status: 'skipped', skipReason: 'no_engine', message: 'No model' }),
    ).toBe('No model');
    expect(
      finalPassNotice({ ...queued, status: 'skipped', skipReason: 'disabled', message: 'Off' }),
    ).toBeNull();
    expect(finalPassNotice(undefined)).toBeNull();
  });
});

describe('applyPartial', () => {
  test('tracks one in-progress line per source and clears it on empty text', () => {
    let partials = applyPartial({}, { source: 'mic', text: 'hello', audio_start_time: 1 });
    partials = applyPartial(partials, { source: 'system', text: 'hi there', audio_start_time: 0.5 });
    partials = applyPartial(partials, { source: 'mic', text: 'hello world', audio_start_time: 1 });
    expect(orderedPartials(partials)).toEqual([
      { source: 'system', text: 'hi there', start: 0.5 },
      { source: 'mic', text: 'hello world', start: 1 },
    ]);
    partials = applyPartial(partials, { source: 'mic', text: '  ', audio_start_time: 3 });
    expect(Object.keys(partials)).toEqual(['system']);
  });

  test('returns the same object when nothing changed', () => {
    const partials = applyPartial({}, { source: 'mic', text: 'same', audio_start_time: 2 });
    expect(applyPartial(partials, { source: 'mic', text: 'same', audio_start_time: 2 })).toBe(partials);
    const empty = {};
    expect(applyPartial(empty, { source: 'mic', text: '', audio_start_time: 0 })).toBe(empty);
  });
});

