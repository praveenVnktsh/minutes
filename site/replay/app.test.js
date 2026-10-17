// Run: node --test site/replay/app.test.js
//
// Renders the viewer (app.js) against a minimal fake DOM with hostile runs:
// every payload number and string, the runs list, the report and the saved
// live asks carry markup. Whatever app.js writes as HTML must not contain an
// element or an event-handler attribute that came from the data.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const DIR = __dirname;
const EVIL = '1"><img src=x onerror=alert(1)><script>alert(2)</script>\' onmouseover=\'alert(3)';

/** Every HTML string app.js wrote, in order. */
function fakeDom(written) {
  const METHODS = new Set([
    'addEventListener', 'removeEventListener', 'setAttribute', 'removeAttribute', 'getAttribute',
    'appendChild', 'scrollIntoView', 'setPointerCapture', 'pause', 'load', 'requestSubmit', 'focus',
    'fillRect', 'strokeRect', 'clearRect', 'fillText', 'beginPath', 'moveTo', 'lineTo', 'stroke',
    'setTransform', 'drawImage', 'setLineDash', 'closest',
  ]);
  const elements = new Map();
  function element(id) {
    const own = {
      id,
      children: [],
      style: {},
      dataset: {},
      value: '',
      classList: { add() {}, remove() {}, toggle() {}, contains: () => false },
      querySelector: () => element(`${id} child`),
      querySelectorAll: () => [],
      getContext: () => element(`${id} context`),
      play: () => Promise.resolve(),
      insertAdjacentHTML: (_where, html) => written.push(String(html)),
    };
    return new Proxy(own, {
      get(target, prop) {
        if (prop in target) return target[prop];
        if (METHODS.has(prop)) return () => undefined;
        return undefined;
      },
      set(target, prop, value) {
        if (prop === 'innerHTML' || prop === 'outerHTML') written.push(String(value));
        target[prop] = value;
        return true;
      },
    });
  }
  const byId = (id) => {
    if (!elements.has(id)) elements.set(id, element(id));
    return elements.get(id);
  };
  // The live summary panel needs bars to animate.
  byId('rec-wave').children.push(element('bar'), element('bar'));
  const document = {
    getElementById: byId,
    createElement: (tag) => element(tag),
    querySelector: () => null,
    querySelectorAll: () => [],
    addEventListener() {},
    documentElement: element('html'),
    body: element('body'),
  };
  return document;
}

function run({ events, report, runs, interactive, served }) {
  const written = [];
  const document = fakeDom(written);
  const responses = {
    '/api/runs': () => JSON.stringify({ runs }),
    '/api/runs/evil/events': () => events,
    '/api/runs/evil/report': () => (report ? JSON.stringify(report) : null),
    '/api/runs/evil/interactive': () => interactive || '',
    '/api/runs/evil/audio': () => '',
  };
  const fetch = async (url) => {
    const make = responses[url];
    const body = make ? make() : null;
    return {
      ok: body !== null,
      status: body === null ? 404 : 200,
      text: async () => body || '',
      json: async () => JSON.parse(body),
    };
  };
  const window = {
    REPLAY_SAMPLE: served ? undefined : { events, report },
    devicePixelRatio: 1,
    addEventListener() {},
    matchMedia: () => ({ addEventListener() {} }),
  };
  const context = {
    window,
    document,
    location: { protocol: served ? 'http:' : 'file:', hash: served ? '#run=evil' : '#sample' },
    history: { replaceState() {} },
    localStorage: { getItem: () => null, setItem() {} },
    fetch,
    performance: { now: () => Date.now() },
    requestAnimationFrame: () => 0,
    getComputedStyle: () => ({ getPropertyValue: () => '' }),
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
    TextDecoder,
    TextEncoder,
    Buffer,
    URLSearchParams,
    console,
  };
  window.window = window;
  vm.createContext(context);
  for (const file of ['parser.js', 'app.js']) {
    vm.runInContext(fs.readFileSync(path.join(DIR, file), 'utf8'), context, { filename: file });
  }
  return { written, viewer: () => window.replayViewer };
}

/** Elements and attributes in `html` (a naive tag scan is enough here). */
function tags(html) {
  return [...html.matchAll(/<([a-zA-Z][\w-]*)((?:[^>"']|"[^"]*"|'[^']*')*)>/g)].map((m) => ({
    name: m[1].toLowerCase(),
    attrs: [...m[2].matchAll(/([^\s=/"']+)(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+))?/g)].map((a) => a[1].toLowerCase()),
  }));
}

/** The hostile values reached these templates. */
function assertRendered(written, parts) {
  assert.ok(written.length > 10, 'the viewer rendered');
  for (const part of parts) assert.ok(written.some((h) => h.includes(part)), `rendered ${part}`);
}

function assertClean(written) {
  for (const html of written) {
    for (const tag of tags(html)) {
      assert.ok(!['script', 'img', 'iframe', 'object', 'embed', 'svg', 'math', 'style', 'link'].includes(tag.name),
        `no <${tag.name}> from the data in: ${html.slice(0, 300)}`);
      for (const attr of tag.attrs) {
        assert.ok(!/^on/.test(attr), `no ${attr} attribute in: ${html.slice(0, 300)}`);
      }
    }
    assert.ok(!/javascript:/i.test(html), `no javascript: URL in: ${html.slice(0, 300)}`);
  }
}

/** A copy of `value` with every number (or string) replaced by markup. */
function poison(value, what) {
  if (Array.isArray(value)) return value.map((v) => poison(v, what));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, poison(v, what)]));
  }
  if (what === 'numbers' && typeof value === 'number') return EVIL;
  if (what === 'strings' && typeof value === 'string') return value + EVIL;
  return value;
}

function poisonEvents(what) {
  const text = fs.readFileSync(path.join(DIR, 'sample-events.jsonl'), 'utf8');
  return text.split('\n').filter((l) => l.trim()).map((line) => {
    const r = JSON.parse(line);
    r.payload = poison(r.payload, what);
    return JSON.stringify(r);
  }).join('\n');
}

const evilRun = {
  name: 'evil', recording: EVIL, audio_name: EVIL, status: EVIL, passed: EVIL, failed: EVIL,
  started_at: EVIL, duration_s: EVIL, interactive: EVIL,
};
const evilReport = {
  criteria: [
    { label: EVIL, status: EVIL, value: EVIL, detail: [EVIL] },
    { label: 'x', status: 'pass', value: 1, detail: EVIL },
  ],
};
const evilAsk = JSON.stringify({
  id: EVIL, audio_ms: EVIL, seq: EVIL, as_of_ms: EVIL, kind: 'ask', action: EVIL, prompt: EVIL, reply: `${EVIL}\n- ${EVIL} [00:05]`,
  status: EVIL, error: EVIL, model: EVIL, stats: { firstTokenMs: EVIL, totalMs: EVIL, promptTokens: EVIL, reusedTokens: EVIL, tokensPerSecond: EVIL },
  warm_ms: EVIL, context: { lines: EVIL, timeline: EVIL, notes_chars: EVIL }, key: EVIL,
});

async function settle(viewer) {
  for (let k = 0; k < 20 && !viewer(); k += 1) await new Promise((r) => setTimeout(r, 5));
  await new Promise((r) => setTimeout(r, 30));
  const v = viewer();
  v.play(false);
  v.seekAudio(1e12);
  v.play(false);
  await new Promise((r) => setTimeout(r, 30));
}

// Poisoned strings no longer match the phase names, so no request finishes.
const rendered = {
  numbers: ['data-row-i=', 'class="row', 'Pass criteria', 'data-at=', 'data-start='],
  strings: ['class="row', 'Pass criteria', 'data-at=', 'data-start='],
};

for (const what of ['numbers', 'strings']) {
  test(`hostile ${what} in a file-mode run render as text`, async () => {
    const { written, viewer } = run({ events: poisonEvents(what), report: evilReport, served: false });
    await settle(viewer);
    assertRendered(written, rendered[what]);
    assertClean(written);
  });

  test(`hostile ${what} in a served run, its runs list and live asks render as text`, async () => {
    const { written, viewer } = run({
      events: poisonEvents(what), report: evilReport, runs: [evilRun], interactive: `${evilAsk}\n`, served: true,
    });
    await settle(viewer);
    assertRendered(written, [...rendered[what], 'live-ask', 'runs-table', 'data-seek-audio=']);
    assertClean(written);
  });
}

test('the page runs no inline script, so the served CSP needs no unsafe-inline', () => {
  const html = fs.readFileSync(path.join(DIR, 'index.html'), 'utf8');
  const scripts = [...html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/gi)];
  assert.ok(scripts.length >= 4);
  for (const [, attrs, body] of scripts) {
    const src = /\bsrc="([^"]+)"/.exec(attrs);
    assert.ok(src, `inline script: ${body.slice(0, 80)}`);
    assert.equal(body.trim(), '');
    assert.ok(fs.existsSync(path.join(DIR, src[1])), `${src[1]} exists`);
  }
  for (const tag of tags(html)) {
    for (const attr of tag.attrs) assert.ok(!/^on/.test(attr), `inline handler ${attr} on <${tag.name}>`);
  }
  assert.ok(!/javascript:/i.test(html));
  const serve = fs.readFileSync(path.join(DIR, '../../frontend/src-tauri/src/replay/serve.rs'), 'utf8');
  const csp = /Content-Security-Policy: ([^\\\r\n]+)/.exec(serve)[1];
  const scriptSrc = csp.split(';').map((d) => d.trim()).find((d) => d.startsWith('script-src'));
  assert.equal(scriptSrc, "script-src 'self'");
  for (const [, attrs] of scripts) {
    const src = /\bsrc="([^"]+)"/.exec(attrs)[1];
    assert.ok(serve.includes(`site/replay/${src}`), `${src} is embedded by replay serve`);
  }
});

test('the harness catches injected markup', () => {
  assert.throws(() => assertClean([...Array(11)].map(() => `<td data-at="${EVIL}">x</td>`)));
});
