import { describe, expect, test } from 'bun:test';
import { liveEngineValue } from './LiveTranscriptionModelSettings';
import { LIVE_ENGINE_OPTIONS, type LiveEngineChoice } from '@/lib/speechModels';

describe('live engine choice', () => {
  test('offers Auto, GPU and the Neural Engine only', () => {
    expect(LIVE_ENGINE_OPTIONS.map((option) => option.value)).toEqual(['auto', 'gpu', 'neural-engine']);
  });

  test('shows the stored engine, and Auto for anything the select does not offer', () => {
    expect(liveEngineValue(null)).toBe('auto');
    expect(liveEngineValue({ liveEngine: 'gpu' })).toBe('gpu');
    expect(liveEngineValue({ liveEngine: 'neural-engine' })).toBe('neural-engine');
    // The CPU engine is gone; an older backend may still report it.
    expect(liveEngineValue({ liveEngine: 'cpu' as LiveEngineChoice })).toBe('auto');
  });
});
