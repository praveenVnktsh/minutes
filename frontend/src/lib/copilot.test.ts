import { describe, expect, test } from 'bun:test';
import {
  applyDelta,
  cardHeader,
  citationHrefSeconds,
  citationSeconds,
  debugLine,
  isQuestionLine,
  linkifyCitations,
  liveMomentLabel,
  questionLineStarts,
  type CopilotMessage,
  type CopilotSnapshot,
} from './copilot';

function message(overrides: Partial<CopilotMessage> = {}): CopilotMessage {
  return {
    id: 'm1',
    kind: 'action',
    action: 'what_to_say',
    prompt: 'What should I say?',
    audioMs: 1_531_000,
    reply: '',
    status: 'pending',
    error: null,
    canUseBuiltin: false,
    cardState: null,
    provider: null,
    model: null,
    stats: null,
    createdAt: '2026-10-16T10:00:00Z',
    ...overrides,
  };
}

function snapshot(messages: CopilotMessage[]): CopilotSnapshot {
  return {
    sessionId: 's1',
    active: true,
    lineCount: 3,
    messages,
    suggestionsEnabled: true,
    debugEnabled: false,
    debug: {
      last: null,
      detector: { rulePass: 0, ruleReject: 0, confirmYes: 0, confirmNo: 0, skipped: 0, falseAlarms: 0, last: null },
      liveBacklogSecs: 0,
    },
  };
}

describe('applyDelta', () => {
  test('appends in order and marks the answer streaming', () => {
    let state: CopilotSnapshot | null = snapshot([message()]);
    state = applyDelta(state, { sessionId: 's1', id: 'm1', offset: 0, delta: 'Say ' });
    state = applyDelta(state, { sessionId: 's1', id: 'm1', offset: 4, delta: 'café' });
    expect(state!.messages[0].reply).toBe('Say café');
    expect(state!.messages[0].status).toBe('streaming');
    // Offsets are UTF-8 bytes, like Rust's String::len.
    state = applyDelta(state, { sessionId: 's1', id: 'm1', offset: 9, delta: '!' });
    expect(state!.messages[0].reply).toBe('Say café!');
  });

  test('skips a chunk the snapshot already holds and one that would leave a gap', () => {
    const state = snapshot([message({ reply: 'Say ' , status: 'streaming' })]);
    expect(applyDelta(state, { sessionId: 's1', id: 'm1', offset: 0, delta: 'Say ' })).toBe(state);
    expect(applyDelta(state, { sessionId: 's1', id: 'm1', offset: 99, delta: 'x' })).toBe(state);
    expect(applyDelta(state, { sessionId: 'other', id: 'm1', offset: 4, delta: 'x' })).toBe(state);
  });
});

describe('citations', () => {
  test('parse minutes, hours and reject nonsense', () => {
    expect(citationSeconds('[25:31]')).toBe(1531);
    expect(citationSeconds('[1:02:05]')).toBe(3725);
    expect(citationSeconds('[12:75]')).toBeNull();
    expect(citationSeconds('[later]')).toBeNull();
  });

  test('become links that keep their text and leave real links alone', () => {
    const md = linkifyCitations('We agreed [03:10] and [1:02:05]. See [docs](https://x.y) [09:99].');
    expect(md).toBe('We agreed [[03:10]](#copilot-t=190) and [[1:02:05]](#copilot-t=3725). See [docs](https://x.y) [09:99].');
    expect(citationHrefSeconds('#copilot-t=190')).toBe(190);
    expect(citationHrefSeconds('https://x.y')).toBeNull();
    expect(citationHrefSeconds(undefined)).toBeNull();
  });
});

describe('labels', () => {
  test('saved moments read LIVE · time · action', () => {
    expect(liveMomentLabel('live_action', 'What should I say?', 1_531_000)).toBe('LIVE · [25:31] · WHAT SHOULD I SAY?');
    expect(liveMomentLabel('live_suggestion', 'Others asked you', 61_000)).toBe('LIVE · [01:01] · OTHERS ASKED YOU');
    expect(liveMomentLabel('live_ask', null, 5_000)).toBe('LIVE · [00:05] · QUESTION');
    expect(liveMomentLabel(null, null, null)).toBeNull();
  });

  test('card header', () => {
    expect(cardHeader({ audioMs: 1_531_000 })).toBe('OTHERS ASKED YOU · [25:31]');
  });
});

describe('question lines', () => {
  test('only Others lines a card answers are tagged', () => {
    const state = snapshot([
      message({ id: 'c1', kind: 'suggestion', audioMs: 61_250, cardState: 'active' }),
      message({ id: 'c2', kind: 'suggestion', audioMs: 90_000, cardState: 'not_for_me' }),
    ]);
    const starts = questionLineStarts(state);
    expect(starts).toEqual([61_250]);
    expect(isQuestionLine(starts, 61.25, 'system')).toBe(true);
    expect(isQuestionLine(starts, 61.25, 'mic')).toBe(false);
    expect(isQuestionLine(starts, 90, 'system')).toBe(false);
  });
});

describe('debugLine', () => {
  test('shows latency, speed, reuse, detector and false alarms', () => {
    const state = snapshot([]);
    state.debug.last = { firstTokenMs: 420, totalMs: 3000, tokensPerSecond: 14.2, promptTokens: 1218, reusedTokens: 1174 };
    state.debug.detector.falseAlarms = 2;
    const line = debugLine(state);
    expect(line).toContain('first word 0.42s');
    expect(line).toContain('14.2 tok/s');
    expect(line).toContain('reused 1174/1218');
    expect(line).toContain('false alarms 2');
  });
});
