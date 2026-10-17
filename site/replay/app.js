/*
 * Replay viewer UI. Everything shown is derived from the loaded records:
 * the cursor is a record count `n`, and the screen renders stateAt(n) (see
 * parser.js). Playing advances a time cursor on the chosen axis and maps it
 * to `n`.
 *
 * Opened from files (or the sample), nothing leaves the browser. Served by
 * `replay serve`, it also lists the local runs, follows a run still being
 * recorded, and asks the real Copilot about the meeting at the cursor.
 */
(function () {
  'use strict';
  const R = window.ReplayParser;
  const $ = (id) => document.getElementById(id);

  const view = {
    records: [],
    index: null,
    lanes: null,
    lanesVersion: 0,
    info: null,
    report: null,
    state: null,
    n: 0,
    cursor: 0, // ms on the current axis
    axis: 'audio',
    speed: 64,
    playing: false,
    selected: null,
    tl: { v0: 0, v1: 1 },
    renderedLines: 0,
    // Served by `replay serve`.
    served: false,
    runs: [],
    run: null, // the open run's name
    tail: null,
    asks: [], // live Copilot asks: saved ones and the one running
    asking: null,
    audio: { available: false, blocked: false, message: '' },
  };

  function showError(message) {
    if (window.replayShowError) window.replayShowError(message);
  }

  // ---------------------------------------------------------------------
  // Loading
  // ---------------------------------------------------------------------

  function load(eventsText, reportJson, options = {}) {
    let records;
    try {
      records = R.parseJsonl(eventsText);
    } catch (error) {
      showError(`Could not read the events: ${error.message}`);
      return false;
    }
    setPlaying(false);
    view.index = R.buildIndex(records);
    view.records = view.index.records;
    rebuildDerived();
    view.report = reportJson === undefined ? view.report : reportJson;
    view.run = options.run || null;
    view.asks = options.asks || [];
    setupAudio(view.run);
    view.n = 0;
    view.cursor = 0;
    view.state = R.stateAt(view.index, 0);
    view.renderedLines = -1;
    view.selected = null;
    keys.summary = keys.copilot = keys.now = null;
    $('drop').hidden = true;
    $('rec-pill').hidden = false;
    for (const id of ['play', 'prev', 'next', 'jump-name']) $(id).disabled = false;
    const select = $('jump-name');
    select.innerHTML = view.info.names.map((n) => `<option ${n === 'copilot-state' ? 'selected' : ''}>${esc(n)}</option>`).join('');
    const steps = [...new Set(view.info.detector.map((d) => d.step))].sort();
    $('detector-filter').innerHTML = '<option value="">All</option>' + steps.map((s) => `<option>${esc(s)}</option>`).join('');
    $('run-name').textContent = view.run || '';
    fitTimeline();
    renderStatic();
    seekCount(0);
    renderAsks();
    return true;
  }

  /** Lanes and the run summary: recomputed on load and as a live run grows. */
  function rebuildDerived() {
    view.lanes = R.buildLanes(view.records);
    view.info = R.summarize(view.records);
    view.lanesVersion += 1;
    finalsIndex = null;
  }

  async function readFiles(files) {
    let events = null;
    let report;
    for (const file of files) {
      const text = await file.text();
      if (file.name.endsWith('.jsonl')) events = text;
      else {
        try {
          const json = JSON.parse(text);
          if (Array.isArray(json.criteria)) report = json;
        } catch (_) {
          /* not a report */
        }
      }
    }
    if (events) {
      stopTail();
      if (load(events, report === undefined ? null : report)) setPlaying(true);
    } else if (report && view.records.length) {
      view.report = report;
      renderStatic();
    } else showError('Choose an events.jsonl (and optionally its report.json).');
  }

  $('files').addEventListener('change', (e) => readFiles([...e.target.files]));
  document.addEventListener('dragover', (e) => {
    e.preventDefault();
    $('drop').classList.add('over');
  });
  document.addEventListener('dragleave', () => $('drop').classList.remove('over'));
  document.addEventListener('drop', (e) => {
    e.preventDefault();
    $('drop').classList.remove('over');
    readFiles([...e.dataTransfer.files]);
  });
  const loadSample = () => {
    if (!window.REPLAY_SAMPLE) return showError('The sample is missing (sample-data.js).');
    stopTail();
    if (load(window.REPLAY_SAMPLE.events, window.REPLAY_SAMPLE.report || null)) setPlaying(true);
  };
  $('load-sample').addEventListener('click', loadSample);
  document.addEventListener('click', (e) => {
    if (e.target.closest('[data-load-sample]')) {
      $('runs-pop').hidden = true;
      loadSample();
    }
  });

  // ---------------------------------------------------------------------
  // Runs (served by `replay serve`)
  // ---------------------------------------------------------------------

  const api = (path) => `/api/runs${path}`;

  async function detectServer() {
    if (!/^https?:$/.test(location.protocol)) return false;
    try {
      const res = await fetch(api(''), { cache: 'no-store' });
      if (!res.ok) return false;
      const json = await res.json();
      if (!Array.isArray(json.runs)) return false;
      view.runs = json.runs;
      return true;
    } catch (_) {
      return false;
    }
  }

  function runRows(runs) {
    if (!runs.length) return '<p class="subtle" style="padding:10px">No runs yet under the replays folder.</p>';
    const rows = runs.map((r) => {
      const tone = r.status === 'done' ? 'success' : r.status === 'running' ? 'info' : 'warning';
      const score = r.passed == null ? '–' : `${r.passed}✓ ${r.failed}✗`;
      const created = r.started_at ? new Date(r.started_at).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }) : '';
      return `<tr data-run="${esc(r.name)}" class="${r.name === view.run ? 'current' : ''}">
        <td><b>${esc(r.name)}</b></td>
        <td class="rec">${esc(r.recording || r.audio_name || '')}</td>
        <td class="mono">${r.duration_s ? `${(r.duration_s / 60).toFixed(1)} min` : '–'}</td>
        <td>${badge(tone, r.status)}</td>
        <td class="mono">${esc(score)}</td>
        <td class="subtle">${esc(created)}</td>
        <td class="subtle">${r.interactive ? `${r.interactive} asked` : ''}</td></tr>`;
    }).join('');
    return `<table class="runs-table"><thead><tr><th>Run</th><th>Recording</th><th>Length</th><th>Status</th><th>Criteria</th><th>Created</th><th></th></tr></thead><tbody>${rows}</tbody></table>`;
  }

  async function refreshRuns() {
    try {
      const res = await fetch(api(''), { cache: 'no-store' });
      view.runs = (await res.json()).runs || [];
    } catch (error) {
      showError(`Could not list the runs: ${error.message}`);
    }
    $('runs-pop').innerHTML = runRows(view.runs) + RUNS_FOOT;
    $('runs-inline').innerHTML = runRows(view.runs);
  }

  const RUNS_FOOT = '<div class="runs-foot"><label class="pill-btn" for="files">Open events.jsonl…</label><button class="pill-btn" type="button" data-load-sample>Load sample</button></div>';

  async function openRun(name) {
    $('runs-pop').hidden = true;
    $('runs-btn').setAttribute('aria-expanded', 'false');
    stopTail();
    const enc = encodeURIComponent(name);
    $('run-name').textContent = `Loading ${name}…`;
    try {
      const [events, report, interactive] = await Promise.all([
        fetch(api(`/${enc}/events`), { cache: 'no-store' }).then((r) => {
          if (!r.ok) throw new Error(`events: ${r.status}`);
          return r.text();
        }),
        fetch(api(`/${enc}/report`), { cache: 'no-store' }).then((r) => (r.ok ? r.json() : null)),
        fetch(api(`/${enc}/interactive`), { cache: 'no-store' }).then((r) => (r.ok ? r.text() : '')),
      ]);
      if (!load(events, report, { run: name, asks: R.parseInteractive(interactive).map(savedAsk) })) return;
      history.replaceState(null, '', `#run=${enc}`);
      const meta = view.runs.find((r) => r.name === name);
      if (!report && meta && meta.status === 'running' && $('follow').checked) startTail();
      setPlaying(true);
    } catch (error) {
      $('run-name').textContent = '';
      showError(`Could not open ${name}: ${error.message}`);
    }
  }

  document.addEventListener('click', (e) => {
    const row = e.target.closest('[data-run]');
    if (row) openRun(row.dataset.run);
  });
  $('runs-btn').addEventListener('click', async () => {
    const pop = $('runs-pop');
    pop.hidden = !pop.hidden;
    $('runs-btn').setAttribute('aria-expanded', String(!pop.hidden));
    if (!pop.hidden) await refreshRuns();
  });
  document.addEventListener('pointerdown', (e) => {
    if (!$('runs-pop').hidden && !e.target.closest('#runs-pop, #runs-btn')) {
      $('runs-pop').hidden = true;
      $('runs-btn').setAttribute('aria-expanded', 'false');
    }
  });

  /** Follows a run that is still being recorded. */
  function startTail() {
    if (!view.run || view.tail) return;
    const source = new EventSource(api(`/${encodeURIComponent(view.run)}/tail?from=${view.records.length}`));
    const pending = [];
    let lastDerive = 0;
    const flush = () => {
      if (!pending.length) return;
      const atEnd = view.n >= view.records.length;
      R.extendIndex(view.index, pending.splice(0));
      const now = performance.now();
      if (now - lastDerive > 2000) {
        lastDerive = now;
        rebuildDerived();
        renderStatic();
      }
      if (atEnd && !view.playing) seekCount(view.records.length);
      else drawTimeline();
    };
    const timer = setInterval(flush, 1000);
    source.onmessage = (e) => {
      try {
        const record = R.parseLine(e.data);
        if (record) pending.push(record);
      } catch (_) {
        /* a torn line */
      }
    };
    source.addEventListener('end', async () => {
      stopTail();
      flush();
      rebuildDerived();
      try {
        const res = await fetch(api(`/${encodeURIComponent(view.run)}/report`), { cache: 'no-store' });
        if (res.ok) view.report = await res.json();
      } catch (_) {
        /* not written yet */
      }
      renderStatic();
    });
    source.onerror = () => {
      if (source.readyState === EventSource.CLOSED) stopTail();
    };
    view.tail = { source, timer, flush };
  }

  function stopTail() {
    if (!view.tail) return;
    view.tail.source.close();
    clearInterval(view.tail.timer);
    view.tail = null;
  }

  $('follow').addEventListener('change', () => {
    if ($('follow').checked) {
      const meta = view.runs.find((r) => r.name === view.run);
      if (meta && meta.status === 'running') startTail();
    } else stopTail();
  });

  // ---------------------------------------------------------------------
  // Cursor
  // ---------------------------------------------------------------------

  const axisOf = (r) => (view.axis === 'audio' ? r.audio_ms : r.t_ms);
  const endOfAxis = () => (view.axis === 'audio' ? view.info.endAudioMs : view.info.endMs);

  function countAt(ms) {
    return view.axis === 'audio' ? R.countAtAudio(view.records, ms) : R.countAtTime(view.records, ms);
  }

  function seekCount(n) {
    view.n = Math.max(0, Math.min(n, view.records.length));
    view.state = R.stateAt(view.index, view.n, view.state);
    view.cursor = view.n > 0 ? axisOf(view.records[view.n - 1]) : 0;
    renderState();
    syncAudio();
  }

  function seekTime(ms) {
    view.cursor = Math.max(0, Math.min(ms, endOfAxis()));
    const n = countAt(view.cursor);
    if (n === view.n && view.state) {
      // Nothing new happened: only the cursor moved.
      drawTimeline();
      return;
    }
    view.n = n;
    view.state = R.stateAt(view.index, view.n, view.state);
    renderState();
  }

  let lastFrame = 0;
  let lastAudioSync = 0;
  const fps = { frames: 0, since: 0, value: 0 };
  function tick(now) {
    if (!view.playing) return;
    const dt = lastFrame ? Math.min(now - lastFrame, 100) : 0;
    lastFrame = now;
    fps.frames += 1;
    if (now - fps.since >= 1000) {
      fps.value = (fps.frames * 1000) / (now - fps.since || 1);
      fps.frames = 0;
      fps.since = now;
    }
    // While the recording plays, it is the clock (no drift at 1×); otherwise
    // the frame time times the speed.
    if (now - lastAudioSync > 500) {
      lastAudioSync = now;
      syncAudio();
    }
    const next = audioClocked() ? audio.currentTime * 1000 : view.cursor + dt * view.speed;
    if (next >= endOfAxis()) {
      seekTime(endOfAxis());
      if (!view.tail) setPlaying(false);
      else requestAnimationFrame(tick);
      return;
    }
    seekTime(next);
    requestAnimationFrame(tick);
  }
  // For scripting and measuring (e.g. over the DevTools protocol).
  window.replayViewer = {
    fps: () => fps.value,
    seekAudio: (ms) => seekAudio(ms),
    play: (on) => setPlaying(on !== false),
    setSpeed: (x) => document.querySelector(`[data-speed="${x}"]`)?.click(),
    audio: () => ({ currentTime: audio.currentTime, paused: audio.paused, rate: audio.playbackRate, src: audio.currentSrc, cursorMs: view.cursor, note: $('audio-note').textContent }),
    cursor: () => ({ n: view.n, audioMs: view.state ? view.state.audioMs : 0, records: view.records.length, run: view.run, playing: view.playing }),
    asks: () => view.asks.map((a) => ({ prompt: a.prompt, status: a.status, reply: a.reply, stats: a.stats, firstTokenMs: a.firstTokenMs, error: a.error })),
  };

  function setPlaying(on) {
    view.playing = on && view.records.length > 0;
    $('play-icon').innerHTML = view.playing ? '<rect x="3.5" y="2.5" width="3" height="11" rx="1"/><rect x="9.5" y="2.5" width="3" height="11" rx="1"/>' : '<path d="M4 2.5v11l9-5.5z"/>';
    $('play').setAttribute('aria-label', view.playing ? 'Pause' : 'Play');
    if (view.playing) {
      if (view.cursor >= endOfAxis()) seekTime(0);
      lastFrame = 0;
      fps.since = performance.now();
      fps.frames = 0;
      requestAnimationFrame(tick);
    } else if (view.state) {
      // Settle the throttled panels on the paused moment.
      renderPanels(true);
    }
    syncAudio();
  }

  $('play').addEventListener('click', () => setPlaying(!view.playing));
  document.querySelectorAll('[data-speed]').forEach((b) => b.addEventListener('click', () => {
    view.speed = Number(b.dataset.speed);
    document.querySelectorAll('[data-speed]').forEach((o) => o.setAttribute('aria-pressed', String(o === b)));
    syncAudio();
  }));
  document.querySelectorAll('[data-axis]').forEach((b) => b.addEventListener('click', () => {
    if (view.axis === b.dataset.axis) return;
    view.axis = b.dataset.axis;
    document.querySelectorAll('[data-axis]').forEach((o) => o.setAttribute('aria-pressed', String(o === b)));
    if (view.records.length) {
      fitTimeline();
      seekCount(view.n);
    }
    syncAudio();
  }));
  const jump = (direction) => {
    const n = R.nextOfName(view.records, view.n, $('jump-name').value, direction);
    if (n !== null) {
      seekCount(n);
      select(view.records[n - 1].i, null);
    }
  };
  $('prev').addEventListener('click', () => jump(-1));
  $('next').addEventListener('click', () => jump(1));
  document.addEventListener('keydown', (e) => {
    if (e.target.closest('input, select, textarea')) return;
    if (e.key === ' ') {
      e.preventDefault();
      setPlaying(!view.playing);
    } else if (e.key === 'ArrowRight') jump(1);
    else if (e.key === 'ArrowLeft') jump(-1);
  });
  $('theme').addEventListener('click', () => {
    const next = document.documentElement.dataset.theme === 'light' ? 'dark' : 'light';
    document.documentElement.dataset.theme = next;
    try { localStorage.setItem('minutes-replay-theme', next); } catch (_) { /* private mode */ }
    colors = null;
    drawTimeline();
  });

  // ---------------------------------------------------------------------
  // Rendering helpers
  // ---------------------------------------------------------------------

  function esc(text) {
    return String(text ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  }

  const CITE = /\[(?:(\d{1,2}):)?(\d{1,2}):(\d{2})\]/g;

  function citeValid(seconds) {
    const finals = view.state && view.state.finalLines;
    if (!finals) return true;
    return finals.some((l) => seconds >= (l.start ?? 0) - 2 && seconds <= (l.end ?? l.start ?? 0) + 2);
  }

  /** The tiny Markdown the replies use: paragraphs, "- " lists, **bold**, citations. */
  function markdown(text) {
    const inline = (s) => esc(s)
      .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
      .replace(CITE, (whole, h, m, sec) => {
        const seconds = Number(h || 0) * 3600 + Number(m) * 60 + Number(sec);
        return `<button type="button" class="cite${citeValid(seconds) ? '' : ' bad'}" data-seconds="${seconds}" title="${citeValid(seconds) ? 'Go to this moment' : 'No final-transcript line at this time'}">${whole}</button>`;
      });
    const out = [];
    let list = false;
    for (const line of String(text || '').split('\n')) {
      const item = /^\s*[-*]\s+(.*)$/.exec(line);
      if (item) {
        if (!list) out.push('<ul>');
        list = true;
        out.push(`<li>${inline(item[1])}</li>`);
        continue;
      }
      if (list) out.push('</ul>');
      list = false;
      if (line.trim()) out.push(`<p>${inline(line)}</p>`);
    }
    if (list) out.push('</ul>');
    return out.join('');
  }

  document.addEventListener('click', (e) => {
    const cite = e.target.closest('.cite');
    if (cite) seekToLine(Number(cite.dataset.seconds));
    const at = e.target.closest('[data-at]');
    if (at) {
      seekCount(Number(at.dataset.at) + 1);
      select(Number(at.dataset.at), null);
    }
    const seek = e.target.closest('[data-seek-audio]');
    if (seek) seekAudio(Number(seek.dataset.seekAudio));
  });

  /** Finalized lines, `[startSeconds, record index]`, for citations. */
  let finalsIndex = null;
  function finals() {
    if (!finalsIndex) {
      finalsIndex = [];
      for (const r of view.records) {
        const p = r.payload || {};
        if (r.type === 'event' && r.name === 'transcript-update' && !p.is_partial && String(p.text || '').trim()) {
          finalsIndex.push([Number(p.audio_start_time) || 0, r.i]);
        }
      }
    }
    return finalsIndex;
  }

  /** A [MM:SS] citation: move the cursor to when that line was transcribed
   * (or keep it, if later) and show the line. */
  function seekToLine(seconds) {
    if (!view.records.length) return;
    let best = null;
    for (const [start, i] of finals()) {
      if (start <= seconds + 0.5) best = i;
      else break;
    }
    if (best !== null && best + 1 > view.n) seekCount(best + 1);
    else if (best === null) seekAudio(seconds * 1000);
    setPlaying(false);
    requestAnimationFrame(() => jumpTranscript(seconds));
  }

  /** Moves the cursor to meeting time `ms`. */
  function seekAudio(ms) {
    if (!view.records.length) return;
    if (view.axis === 'audio') {
      seekTime(ms);
      syncAudio();
    } else seekCount(R.countAtAudio(view.records, ms));
  }

  function jumpTranscript(seconds) {
    const rows = [...$('transcript').querySelectorAll('.row')];
    let best = null;
    for (const row of rows) {
      if (Number(row.dataset.start) <= seconds + 0.5) best = row;
    }
    if (!best) return;
    best.scrollIntoView({ block: 'center', behavior: 'smooth' });
    best.classList.add('flash');
    setTimeout(() => best.classList.remove('flash'), 1400);
  }

  const badge = (tone, text) => `<span class="badge ${tone}">${esc(text)}</span>`;

  // ---------------------------------------------------------------------
  // The run panel (static per load)
  // ---------------------------------------------------------------------

  function renderStatic() {
    const meta = view.info.meta;
    const limits = (meta.limitations || []).map((l) => `<p class="limit">${esc(l)}</p>`).join('');
    const report = view.report;
    let score = `<p class="subtle">${view.served ? 'No report.json yet (the run has not finished).' : 'Open the run\'s <b>report.json</b> too to see the pass criteria.'}</p>`;
    if (report && Array.isArray(report.criteria)) {
      const tone = { pass: 'success', fail: 'error', not_applicable: 'neutral', info: 'info' };
      const label = { pass: 'Pass', fail: 'Fail', not_applicable: 'n/a', info: 'Info' };
      const rows = report.criteria.map((c) => `
        <tr><td>${esc(c.label)}${(c.detail || []).length ? `<details class="detail"><summary>${c.detail.length} details</summary><ul>${c.detail.map((d) => `<li>${esc(d)}</li>`).join('')}</ul></details>` : ''}</td>
        <td>${badge(tone[c.status] || 'neutral', label[c.status] || c.status)}</td><td class="muted">${esc(c.value)}</td></tr>`).join('');
      const passed = report.criteria.filter((c) => c.status === 'pass').length;
      const failed = report.criteria.filter((c) => c.status === 'fail').length;
      score = `<p class="subtle">${passed} passed · ${failed} failed</p><table><thead><tr><th>Criterion</th><th>Result</th><th>Value</th></tr></thead><tbody>${rows}</tbody></table>`;
    }
    const duration = meta.duration_s ? `${(meta.duration_s / 60).toFixed(1)} min` : '';
    const mode = meta.realtime ? 'real time' : meta.speed === 0 || meta.realtime === false ? 'unpaced (simulated meeting clock)' : `${meta.speed ?? 1}×`;
    const runMeta = view.runs.find((r) => r.name === view.run);
    $('run').innerHTML = `
      <div class="side-card run-head">
        <h3>Run</h3>
        <h2>${esc((runMeta && runMeta.recording) || view.run || meta.audio_name || 'Replay')}</h2>
        <p>${esc(duration)} · ${esc(mode)} · live: ${esc(meta.live_engine || '?')} · model: ${esc(meta.summary_model || '?')} · ${view.records.length} events · replay took ${esc(R.clock(view.info.endMs))}${view.tail ? ' · <b>following live</b>' : ''}</p>
        ${limits}
      </div>
      <div class="card"><h3>Pass criteria</h3>${score}</div>`;
    renderRequests();
    renderLogs();
  }

  function renderRequests() {
    const rows = view.info.requests.map((r) => {
      const s = r.stats || {};
      return `<tr data-row-i="${r.i}"><td><button type="button" data-at="${r.i}">${esc(R.clock(r.audioMs))}</button></td><td>${esc(r.kind || '')}</td>
        <td>${badge(r.status === 'done' ? 'success' : r.status === 'error' ? 'error' : 'neutral', r.status || '')}</td>
        <td class="mono">${s.firstTokenMs ?? '–'}</td><td class="mono">${s.totalMs ?? '–'}</td>
        <td class="mono">${s.tokensPerSecond != null ? Number(s.tokensPerSecond).toFixed(1) : '–'}</td>
        <td class="mono">${s.reusedTokens ?? '–'}/${s.promptTokens ?? '–'}</td></tr>`;
    }).join('');
    $('requests').innerHTML = rows
      ? `<table><thead><tr><th>At</th><th>Kind</th><th>Status</th><th>First ms</th><th>Total ms</th><th>tok/s</th><th>Reused</th></tr></thead><tbody>${rows}</tbody></table>`
      : '<p class="subtle">No Copilot requests.</p>';
  }

  function renderLogs() {
    const filter = $('detector-filter').value;
    const cls = () => 'future';
    const det = view.info.detector.filter((d) => !filter || d.step === filter);
    $('detector').innerHTML = det.length
      ? `<table><tbody>${det.map((d) => `<tr class="${cls(d.i)}"><td><button type="button" data-at="${d.i}">${esc(R.clock(d.audioMs))}</button></td><td>${badge(d.step === 'confirm_yes' ? 'warning' : d.step === 'skipped' ? 'neutral' : d.step && d.step.startsWith('confirm_') && d.step !== 'confirm_no' ? 'error' : 'info', d.step)}</td><td>${esc(d.decision)}</td><td class="subtle">line ${esc(R.clock(d.lineMs))}</td></tr>`).join('')}</tbody></table>`
      : '<p class="subtle">No detector decisions.</p>';
    const side = view.info.sidecar;
    $('sidecar').innerHTML = side.length
      ? `<table><tbody>${side.map((s) => `<tr class="${cls(s.i)}"><td><button type="button" data-at="${s.i}">${esc(R.clock(s.audioMs))}</button></td><td>${badge(s.what === 'preempt' || s.what === 'timeline-preempted' ? 'warning' : s.what === 'timeout' ? 'error' : 'neutral', s.what)}</td><td class="mono">${esc(s.id ?? '')}</td><td>${esc(s.priority || '')}</td><td class="subtle">${esc(s.detail || '')}</td></tr>`).join('')}</tbody></table>`
      : '<p class="subtle">No llama-helper activity.</p>';
    logRows = [...document.querySelectorAll('#detector tr, #sidecar tr, #requests tr[data-row-i]')].map((tr) => {
      const button = tr.querySelector('[data-at]');
      return { tr, i: button ? Number(button.dataset.at) : 0 };
    });
    logCursor = -1;
    updateLogCursor();
  }
  $('detector-filter').addEventListener('change', renderLogs);

  let logRows = [];
  let logCursor = -1;
  let logTimer = null;
  /** Rows before the cursor are "past"; throttled while playing. */
  function updateLogCursor() {
    if (logTimer) return;
    logTimer = setTimeout(() => {
      logTimer = null;
      if (logCursor === view.n) return;
      logCursor = view.n;
      for (const row of logRows) {
        const cls = row.i < view.n ? 'past' : 'future';
        if (row.tr.className !== cls) row.tr.className = cls;
      }
    }, view.playing ? 250 : 0);
  }

  // ---------------------------------------------------------------------
  // The recording screen at the cursor
  // ---------------------------------------------------------------------

  const PHASE_LABEL = {
    idle: 'Starting', 'live-start': 'Recording', 'audio-end': 'Stopping…', stop: 'Saving…', stopped: 'Saved',
    'final-pass-start': 'Final transcript…', 'final-pass-done': 'Final transcript ready',
    'summary-start': 'Summarizing…', 'summary-done': 'Summary ready', 'run-end': 'Done',
  };

  /** What each throttled panel last rendered, so unchanged ones are skipped. */
  const keys = { summary: null, copilot: null, now: null };
  let lastPanels = 0;

  function renderState() {
    if (!view.state) return;
    const s = view.state;
    const live = s.phase === 'live-start' || s.phase === 'idle';
    const meta = view.info.meta;
    const recorded = Math.min(s.audioMs, (meta.fed_s || meta.duration_s || 1e9) * 1000);
    $('clock').textContent = `${R.clock(s.audioMs)} · wall ${R.clock(s.tMs)} · #${view.n}`;
    $('rec-time').textContent = R.clock(recorded);
    $('rec-dot').className = `dot ${live ? 'live' : 'done'}`;
    $('rec-state').textContent = PHASE_LABEL[s.phase] || s.phase;
    const partial = Object.keys(s.partials).length > 0;
    [...$('rec-wave').children].forEach((bar, k) => {
      const h = live ? (partial ? 6 + ((k * 7 + Math.floor(s.audioMs / 150)) % 9) * 2.2 : 4) : 3;
      bar.style.height = `${h}px`;
    });
    const label = $('live-label');
    label.className = `live-label${live ? '' : ' ended'}`;
    label.querySelector('span').textContent = live ? 'Live' : 'Ended';

    renderTranscript(s);
    renderPanels(false);
    updateLogCursor();
    drawTimeline();
  }

  /** The live summary, Copilot and "Now" panels: re-rendered only when what
   * they show changed, and at most every 100 ms while playing. */
  function renderPanels(force) {
    const now = performance.now();
    if (!force && view.playing && now - lastPanels < 100) return;
    lastPanels = now;
    const s = view.state;
    const live = s.phase === 'live-start' || s.phase === 'idle';
    renderLiveSummary(s, live);
    renderCopilot(s);
    if (force || !view.playing || now - (keys.nowAt || 0) > 250) {
      keys.nowAt = now;
      renderNow(s);
    }
  }

  function renderLiveSummary(s, live) {
    const t = s.timeline;
    const entries = (t && t.entries) || [];
    const key = `${entries.length}|${t && t.status}|${entries.length ? entries[entries.length - 1].id : ''}|${live}`;
    if (key === keys.summary) return;
    const grew = keys.summaryCount !== undefined && entries.length > keys.summaryCount;
    keys.summary = key;
    keys.summaryCount = entries.length;
    let notice = '';
    if (t && t.status === 'failed') notice = '<p class="notice warning">Live summary paused. It will try again with the next minute of conversation.</p>';
    else if (t && t.status === 'unavailable') notice = `<p class="notice ${t.error ? 'warning' : 'neutral'}">${esc(t.error || 'Live summary needs a summary model. Choose one in Settings → Summary.')}</p>`;
    else if (t && t.status === 'disabled') notice = '<p class="notice neutral">Live summary is off. Turn it on in Settings → Summary.</p>';
    const marker = (kind) => (kind === 'decision' ? badge('success', 'Decision') : kind === 'action' ? badge('info', 'Action') : '');
    const list = entries.length
      ? `<ol>${entries.map((e, k) => `<li class="${grew && k === entries.length - 1 ? 'fresh' : ''}"><span class="ls-time"><button type="button" data-seconds="${Math.floor(e.start_ms / 1000)}" class="cite-plain">${esc(R.clock(e.start_ms))}</button></span><span class="ls-text">${marker(e.kind)}${esc(e.text)}${e.speaker ? `<span class="ls-speaker">· ${esc(e.speaker)}</span>` : ''}</span></li>`).join('')}</ol>`
      : '';
    const listening = live && (!t || (t.status !== 'disabled' && t.status !== 'unavailable'));
    const tail = listening
      ? `<p class="ls-listening"><i class="${t && t.status === 'generating' ? 'pulse' : ''}"></i>${t && t.status === 'generating' ? 'Summarizing the last minute…' : entries.length === 0 ? 'Listening… key points will appear here every minute or so.' : 'Listening…'}</p>`
      : '';
    const box = $('live-summary');
    const nearBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 80;
    box.innerHTML = `${notice}${list}${tail}`;
    $('ls-count').textContent = entries.length ? `${entries.length} entries` : '';
    // Follows the newest entry.
    if (nearBottom || view.playing || grew) box.scrollTop = box.scrollHeight;
  }
  $('live-summary').addEventListener('click', (e) => {
    const b = e.target.closest('.cite-plain');
    if (b) {
      setPlaying(false);
      jumpTranscript(Number(b.dataset.seconds));
    }
  });

  function lineHtml(line, tag) {
    return `<div class="row${view.playing ? ' rise' : ''}" data-start="${line.start}" data-i="${line.i}">
      <span class="row-time">[${esc(R.clock(line.start * 1000))}]</span>
      <div class="row-body"><span class="chip ${line.source === 'mic' ? 'mic' : 'system'}">${line.source === 'mic' ? 'You' : 'Others'}</span>${tag ? `<span class="row-tag">${esc(tag)}</span>` : ''}
      <p class="row-text">${esc(line.text)}</p></div></div>`;
  }

  let renderedTags = '';
  let renderedPartials = '';
  function renderTranscript(s) {
    const box = $('transcript');
    const starts = R.questionLineStarts(s.copilot);
    const tagKey = starts.join(',');
    const tagOf = (line) => (R.isQuestionLine(starts, line.start, line.source) ? 'Question for you' : '');
    const nearBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 80;
    let changed = true;
    if (s.lines.length === 0) {
      if (view.renderedLines !== 0) box.innerHTML = '<p class="empty">Listening… live transcript will appear here as people speak.</p>';
      else changed = false;
      view.renderedLines = 0;
    } else if (view.renderedLines > 0 && view.renderedLines <= s.lines.length && tagKey === renderedTags) {
      if (s.lines.length > view.renderedLines) {
        box.insertAdjacentHTML('beforeend', s.lines.slice(view.renderedLines).map((l) => lineHtml(l, tagOf(l))).join(''));
      } else changed = false;
    } else {
      box.innerHTML = s.lines.map((l) => lineHtml(l, tagOf(l))).join('');
    }
    view.renderedLines = s.lines.length;
    renderedTags = tagKey;
    if (changed && (nearBottom || view.playing)) box.scrollTop = box.scrollHeight;
    $('tx-count').textContent = s.lines.length ? `${s.lines.length} lines` : '';
    const partials = Object.values(s.partials).sort((a, b) => (a.source === 'mic' ? -1 : 1) - (b.source === 'mic' ? -1 : 1));
    // The newest words of a long partial.
    const tailOf = (text) => (text.length > 220 ? `…${text.slice(-220)}` : text);
    const html = partials.map((p) => `<p><b>${p.source === 'mic' ? 'You' : 'Others'}</b>${esc(tailOf(p.text))}</p>`).join('');
    if (html !== renderedPartials) {
      $('partials').innerHTML = html;
      renderedPartials = html;
    }
  }

  const QUICK = [['what_to_say', 'What should I say?'], ['explain', 'Explain that'], ['catch_up', 'Catch me up'], ['follow_up', 'Draft follow-up']];

  function statsLine(stats, extra) {
    if (!stats && !extra) return '';
    const s = stats || {};
    const parts = [];
    if (s.firstTokenMs != null) parts.push(`first word ${(s.firstTokenMs / 1000).toFixed(2)} s`);
    if (s.tokensPerSecond != null) parts.push(`${Number(s.tokensPerSecond).toFixed(1)} tok/s`);
    if (s.promptTokens != null) parts.push(`prompt ${s.reusedTokens ?? 0}/${s.promptTokens} tokens reused`);
    if (s.totalMs != null) parts.push(`total ${(s.totalMs / 1000).toFixed(1)} s`);
    if (extra) parts.push(extra);
    return `<p class="stats">${esc(parts.join(' · '))}</p>`;
  }

  function reply(m) {
    const working = m.status === 'pending' || m.status === 'streaming';
    if (m.status === 'error') {
      return `<div class="err-card" role="alert"><b>Copilot couldn't get an answer</b>${m.error ? `<p>${esc(m.error)}</p>` : ''}${m.canUseBuiltin ? '<span class="small">Use built-in model instead</span>' : ''}</div>`;
    }
    const body = m.reply ? `<div class="reply">${markdown(m.reply)}</div>` : working ? '<p class="working"><span class="pulse">✦</span> Listening back…</p>' : '';
    return `${body}${m.status === 'stopped' ? '<p class="stats">Stopped</p>' : ''}${statsLine(m.stats)}`;
  }

  /** A live ask (the viewer's own question to the real Copilot). */
  function askHtml(a) {
    const working = a.status === 'pending' || a.status === 'streaming';
    const when = `<button type="button" data-seek-audio="${a.audio_ms}" title="Go to this moment">[${esc(R.clock(a.audio_ms))}]</button>`;
    let body;
    if (a.status === 'error') {
      body = `<div class="err-card" role="alert"><b>Copilot couldn't get an answer</b><p>${esc(a.error || 'Unknown error')}</p></div>`;
    } else if (a.reply) body = `<div class="reply">${markdown(a.reply)}</div>`;
    else if (working) body = `<p class="working"><span class="pulse">✦</span> ${esc(a.phase || 'Listening back…')}</p>`;
    else body = '';
    const actions = working
      ? `<div class="actions-row"><button type="button" class="small" data-ask-stop="${a.key}">■ Stop</button></div>`
      : a.reply ? `<div class="actions-row"><button type="button" class="small" data-ask-copy="${a.key}">Copy</button></div>` : '';
    const stats = a.stats || (a.firstTokenMs != null ? { firstTokenMs: a.firstTokenMs } : null);
    const extra = [a.model, a.context ? `${a.context.lines} lines · ${a.context.timeline} summary entries${a.context.notes_chars ? ' · notes' : ''}` : '', a.warm_ms > 1500 ? `model load ${(a.warm_ms / 1000).toFixed(1)} s` : ''].filter(Boolean).join(' · ');
    return `<div class="live-ask" id="ask-${a.key}"><div class="bubble-wrap"><div class="bubble live"><small>You asked live at ${when}${a.action ? ` · ${esc(a.prompt)}` : ''}</small>${a.action ? '' : esc(a.prompt)}</div></div>${body}${a.status === 'stopped' ? '<p class="stats">Stopped</p>' : ''}${actions}${statsLine(stats, extra)}</div>`;
  }

  function renderCopilot(s) {
    const c = s.copilot;
    const messages = (c && c.messages) || [];
    const ready = s.lines.length > 0;
    const asksKey = view.asks.map((a) => `${a.key}${a.status}${(a.reply || '').length}`).join(',');
    const key = `${ready}|${c && c.suggestionsEnabled}|${messages.map((m) => `${m.id.slice(0, 6)}${m.status}${(m.reply || '').length}${m.cardState || ''}`).join(',')}|${asksKey}|${!!view.asking}`;
    if (key === keys.copilot) return;
    keys.copilot = key;
    const fired = new Set(messages.filter((m) => m.action && (m.status === 'pending' || m.status === 'streaming')).map((m) => m.action));
    if (view.asking && view.asking.ask.action) fired.add(view.asking.ask.action);
    $('cp-pills').innerHTML = QUICK.map(([id, label]) => `<button type="button" data-action="${id}" class="${fired.has(id) ? 'fired' : ''}" ${ready && !view.asking ? '' : 'disabled'}>${esc(label)}</button>`).join('');
    $('cp-send').disabled = !!view.asking;
    $('cp-suggestions').className = `switch${!c || c.suggestionsEnabled !== false ? ' on' : ''}`;
    const list = $('cp-list');
    const nearBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 80;
    const recorded = messages.map((m) => {
      if (m.kind === 'suggestion') {
        const activeCard = m.cardState === 'active';
        return `<div class="sugg${activeCard ? ' active' : ''}"><span class="sugg-head">OTHERS ASKED YOU · [${esc(R.clock(m.audioMs))}]</span><p class="sugg-quote">“${esc(m.prompt)}”</p>${reply(m)}</div>`;
      }
      return `<div><div class="bubble-wrap"><div class="bubble"><small>[${esc(R.clock(m.audioMs))}]</small>${esc(m.prompt)}</div></div>${reply(m)}</div>`;
    }).join('');
    const empty = !messages.length && !view.asks.length
      ? `<p class="cp-empty">${ready ? 'Ask about the meeting at this moment, or pick a quick action. The replay\'s own Copilot moments appear here as it plays.' : 'Copilot starts once the first words are transcribed.'}</p>`
      : '';
    const asks = view.asks.length ? `<p class="live-head-label">Asked live in the viewer</p>${view.asks.map(askHtml).join('')}` : '';
    list.innerHTML = `${empty}${recorded}${asks}`;
    if (nearBottom || view.playing || view.asking) list.scrollTop = list.scrollHeight;
    if (c && c.debug) {
      const d = c.debug.detector || {};
      const last = c.debug.last;
      const parts = [];
      if (last) {
        parts.push(`first word ${last.firstTokenMs == null ? '–' : `${(last.firstTokenMs / 1000).toFixed(2)}s`}`);
        parts.push(`${last.tokensPerSecond == null ? '–' : Number(last.tokensPerSecond).toFixed(1)} tok/s`);
        parts.push(`reused ${last.reusedTokens ?? '–'}/${last.promptTokens ?? '–'}`);
      } else parts.push('no answers yet');
      parts.push(`detector ${d.rulePass ?? 0}✓/${d.ruleReject ?? 0}✗ rule, ${d.confirmYes ?? 0}✓/${d.confirmNo ?? 0}✗/${d.confirmUnknown ?? 0}? model, ${d.skipped ?? 0} skipped`);
      parts.push(`false alarms ${d.falseAlarms ?? 0}`);
      $('cp-debug').textContent = `Replay: ${parts.join(' · ')}`;
    } else $('cp-debug').textContent = '';
  }

  function renderNow(s) {
    const t = s.timeline;
    const key = `${s.phase}|${s.lines.length}|${(t && t.entries || []).length}|${(s.copilot && s.copilot.messages || []).length}|${s.progress ? JSON.stringify(s.progress.payload) : ''}|${!!s.finalPass}|${s.notices.length}|${s.summaries.length}`;
    if (key === keys.now) return;
    keys.now = key;
    const bits = [`<p>${badge('neutral', PHASE_LABEL[s.phase] || s.phase)} <span class="subtle">${s.lines.length} lines · ${(t && t.entries || []).length} summary entries · ${(s.copilot && s.copilot.messages || []).length} Copilot moments</span></p>`];
    if (s.progress) bits.push(`<p class="subtle">${esc(s.progress.name)}: ${esc(s.progress.payload && (s.progress.payload.stage || s.progress.payload.message || ''))} ${esc(s.progress.payload && s.progress.payload.progress != null ? `${s.progress.payload.progress}%` : '')}</p>`);
    if (s.finalPass) bits.push(`<p>${badge(s.finalPass.outcome === 'replaced' ? 'success' : 'warning', `final pass: ${s.finalPass.outcome}`)} <span class="subtle">${esc(s.finalPass.provider || '')} ${esc(s.finalPass.model || '')}</span></p>`);
    for (const n of s.notices.slice(-3)) bits.push(`<p class="notice warning">${esc(n.text)}</p>`);
    $('now').innerHTML = bits.join('');
    const runs = s.summaries;
    if (!runs.length) $('summaries').innerHTML = '<p class="subtle">No summary yet.</p>';
    else {
      const same = runs.length > 1 && runs.every((r) => r.markdown === runs[0].markdown);
      $('summaries').innerHTML = `<p>${runs.length > 1 ? badge(same ? 'success' : 'warning', same ? 'Runs identical' : 'Runs differ') : ''}</p>` +
        runs.map((r) => `<details class="detail"><summary>Run ${esc(r.run)}: ${esc(r.status)} in ${((r.ms || 0) / 1000).toFixed(0)} s</summary><pre class="summary-md">${esc(r.markdown || r.error || '')}</pre></details>`).join('');
    }
  }

  // ---------------------------------------------------------------------
  // The recording (served): plays along with the cursor on the meeting axis
  // ---------------------------------------------------------------------

  const audio = $('audio');
  audio.preservesPitch = true;

  function audioNote(html) {
    if ($('audio-note').innerHTML !== html) $('audio-note').innerHTML = html;
  }

  /** Points the player at the run's recording (or nothing, in file mode). */
  async function setupAudio(run) {
    view.audio = { available: false, blocked: false, message: '' };
    audio.pause();
    audio.removeAttribute('src');
    audio.load();
    $('audio-ctl').classList.toggle('off', !(view.served && run));
    if (!view.served || !run) {
      $('audio-ctl').title = 'Audio needs replay serve';
      audioNote(view.records.length ? 'Audio needs <code>replay serve</code>' : '');
      return;
    }
    $('audio-ctl').title = 'The meeting recording, in step with the cursor';
    const url = api(`/${encodeURIComponent(run)}/audio`);
    try {
      const res = await fetch(url, { headers: { Range: 'bytes=0-0' }, cache: 'no-store' });
      if (!res.ok) {
        const message = (await res.text()) || `The recording is unavailable (${res.status})`;
        view.audio.message = message;
        $('audio-ctl').classList.add('off');
        $('audio-ctl').title = message;
        audioNote(esc(message));
        return;
      }
    } catch (error) {
      audioNote(esc(`Audio: ${error.message}`));
      return;
    }
    if (view.run !== run) return;
    audio.src = url;
    view.audio.available = true;
    syncAudio();
  }

  function audioPlan() {
    return R.audioPlan({
      available: view.audio.available,
      axis: view.axis,
      speed: view.speed,
      playing: view.playing,
      cursorMs: view.cursor,
      durationMs: Number.isFinite(audio.duration) ? audio.duration * 1000 : 0,
    });
  }

  /** The recording drives the cursor: it is playing as planned. */
  function audioClocked() {
    return view.audio.available && audioPlan().play && !audio.paused && !audio.seeking && audio.readyState >= 2;
  }

  /** Brings the recording in line with the cursor, the speed and the axis. */
  function syncAudio() {
    if (!view.audio.available) return;
    const plan = audioPlan();
    const notes = {
      wall: 'Audio off on wall time',
      fast: `Audio muted above ${R.AUDIO_MAX_RATE}×`,
      ended: 'Past the end of the recording',
    };
    audioNote(view.audio.blocked && plan.play ? 'Press ▶ to hear the audio' : notes[plan.reason] || '');
    if (plan.play) {
      if (audio.playbackRate !== plan.rate) audio.playbackRate = plan.rate;
      if (R.needsResync(audio.currentTime, view.cursor)) audio.currentTime = view.cursor / 1000;
      if (audio.paused && !view.audio.starting) {
        view.audio.starting = true;
        audio.play().then(() => {
          view.audio.blocked = false;
          audioNote('');
        }).catch(() => {
          // Autoplay without a click is refused: the cursor plays on silently.
          view.audio.blocked = true;
          audioNote('Press ▶ to hear the audio');
        }).finally(() => { view.audio.starting = false; });
      }
    } else {
      if (!audio.paused) audio.pause();
      // Paused on the meeting axis: park the recording at the cursor.
      if (plan.reason === 'paused' && R.needsResync(audio.currentTime, view.cursor)) {
        audio.currentTime = view.cursor / 1000;
      }
    }
  }

  function applyVolume() {
    $('mute-icon').style.opacity = audio.muted ? '0.35' : '1';
    $('mute').setAttribute('aria-label', audio.muted ? 'Unmute' : 'Mute');
    try {
      localStorage.setItem('minutes-replay-volume', String(audio.volume));
      localStorage.setItem('minutes-replay-muted', audio.muted ? '1' : '0');
    } catch (_) { /* private mode */ }
  }
  try {
    const volume = Number(localStorage.getItem('minutes-replay-volume'));
    if (localStorage.getItem('minutes-replay-volume') !== null && volume >= 0 && volume <= 1) audio.volume = volume;
    audio.muted = localStorage.getItem('minutes-replay-muted') === '1';
  } catch (_) { /* private mode */ }
  $('volume').value = String(audio.volume);
  $('volume').addEventListener('input', () => {
    audio.volume = Number($('volume').value);
    if (audio.volume > 0 && audio.muted) audio.muted = false;
    applyVolume();
  });
  $('mute').addEventListener('click', () => {
    audio.muted = !audio.muted;
    applyVolume();
  });
  $('mute-icon').style.opacity = audio.muted ? '0.35' : '1';
  // A user press of ▶ may start audio the browser refused to autoplay.
  $('play').addEventListener('click', () => {
    if (view.audio.blocked) {
      view.audio.blocked = false;
      syncAudio();
    }
  });

  // ---------------------------------------------------------------------
  // Live Copilot
  // ---------------------------------------------------------------------

  let askKeys = 0;
  /** An interactive.jsonl entry as the list shows it. */
  function savedAsk(entry) {
    askKeys += 1;
    return Object.assign({}, entry, { key: askKeys });
  }

  function note(html) {
    $('cp-note').innerHTML = html;
  }

  function renderAsks() {
    $('asks-count').textContent = view.asks.length ? String(view.asks.length) : '';
    $('asks').innerHTML = view.asks.length
      ? `<table><tbody>${view.asks.map((a) => `<tr class="ask-row"><td><button type="button" data-seek-audio="${a.audio_ms}">${esc(R.clock(a.audio_ms))}</button></td><td>${esc(a.prompt)}</td><td>${badge(a.status === 'done' ? 'success' : a.status === 'error' ? 'error' : 'neutral', a.status || '')}</td><td class="mono">${a.stats && a.stats.firstTokenMs != null ? `${(a.stats.firstTokenMs / 1000).toFixed(2)} s` : '–'}</td></tr>`).join('')}</tbody></table>`
      : '<p class="subtle">Questions you ask the Copilot here are saved to the run\'s interactive.jsonl.</p>';
    keys.copilot = null;
    if (view.state) renderCopilot(view.state);
  }

  /** Re-renders one streaming ask in place (cheap per token). */
  let askFrame = 0;
  function updateAsk(ask) {
    if (askFrame) return;
    askFrame = requestAnimationFrame(() => {
      askFrame = 0;
      const el = document.getElementById(`ask-${ask.key}`);
      if (!el) {
        keys.copilot = null;
        renderCopilot(view.state);
        return;
      }
      el.outerHTML = askHtml(ask);
      const list = $('cp-list');
      list.scrollTop = list.scrollHeight;
    });
  }

  async function askCopilot(kind, text, action) {
    if (!view.served || !view.run) {
      note(view.records.length ? 'Start <code>replay serve</code> and open the run from its list to ask the Copilot live.' : 'Open a run first.');
      return;
    }
    if (view.asking) return;
    if (!view.state || !view.state.lines.length) {
      note('Copilot starts once the first words are transcribed: move the cursor later.');
      return;
    }
    note('');
    if ($('cp-pause').checked && view.playing) setPlaying(false);
    const cursor = R.liveCursor(view.records, view.n);
    const label = action ? (QUICK.find(([id]) => id === action) || [])[1] : text;
    const ask = savedAsk({ kind, action: action || null, prompt: label, audio_ms: cursor.audio_ms, seq: cursor.seq, reply: '', status: 'pending', phase: 'Sending…' });
    view.asks.push(ask);
    const controller = new AbortController();
    view.asking = { ask, controller };
    renderAsks();
    const started = performance.now();
    try {
      const res = await fetch(api(`/${encodeURIComponent(view.run)}/copilot`), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ audio_ms: cursor.audio_ms, seq: cursor.seq, kind, text: text || undefined, action: action || undefined }),
        signal: controller.signal,
      });
      if (!res.ok || !res.body) {
        let message = `The server answered ${res.status}`;
        try { message = (await res.json()).error || message; } catch (_) { /* not JSON */ }
        throw new Error(message);
      }
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        const parsed = R.parseSse(buffer + decoder.decode(value, { stream: true }));
        buffer = parsed.rest;
        for (const { event, data } of parsed.events) {
          const payload = JSON.parse(data);
          if (event === 'queued') ask.phase = 'Waiting for the previous answer…';
          else if (event === 'context') {
            ask.context = { lines: payload.lines, timeline: payload.timeline, notes_chars: payload.notes_chars };
            ask.phase = 'Reading the last few minutes…';
          } else if (event === 'warming') ask.phase = `Loading ${payload.model || 'the model'}…`;
          else if (event === 'start') {
            ask.status = 'streaming';
            ask.model = payload.model;
            ask.phase = 'Listening back…';
          } else if (event === 'first-token') ask.firstTokenMs = payload.ms;
          else if (event === 'delta') ask.reply += payload.text;
          else if (event === 'done') {
            Object.assign(ask, payload, { key: ask.key });
            ask.clientMs = Math.round(performance.now() - started);
          } else if (event === 'error') {
            ask.status = 'error';
            ask.error = payload.message;
          }
          updateAsk(ask);
        }
      }
      if (ask.status === 'pending' || ask.status === 'streaming') {
        ask.status = ask.reply ? 'stopped' : 'error';
        if (!ask.reply) ask.error = 'The answer broke off.';
      }
    } catch (error) {
      if (error.name === 'AbortError') ask.status = 'stopped';
      else {
        ask.status = 'error';
        ask.error = error.message;
      }
    } finally {
      view.asking = null;
      renderAsks();
    }
  }

  async function stopAsk() {
    const current = view.asking;
    if (!current) return;
    try {
      await fetch(api(`/${encodeURIComponent(view.run)}/copilot/stop`), { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' });
    } catch (_) {
      /* the abort below still ends it */
    }
    // The server ends the stream with the partial answer; give up waiting after a moment.
    setTimeout(() => { if (view.asking === current) current.controller.abort(); }, 4000);
  }

  $('cp-form').addEventListener('submit', (e) => {
    e.preventDefault();
    const text = $('cp-text').value.trim();
    if (!text) return;
    if (!view.served || !view.run) {
      askCopilot('ask', text);
      return;
    }
    if (view.asking) return;
    $('cp-text').value = '';
    autosize();
    askCopilot('ask', text);
  });
  $('cp-text').addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      $('cp-form').requestSubmit();
    }
  });
  const autosize = () => {
    const t = $('cp-text');
    t.style.height = 'auto';
    t.style.height = `${Math.min(t.scrollHeight, 120)}px`;
  };
  $('cp-text').addEventListener('input', autosize);
  $('cp-pills').addEventListener('click', (e) => {
    const b = e.target.closest('[data-action]');
    if (b) askCopilot('action', null, b.dataset.action);
  });
  $('cp-list').addEventListener('click', async (e) => {
    const stop = e.target.closest('[data-ask-stop]');
    if (stop) stopAsk();
    const copy = e.target.closest('[data-ask-copy]');
    if (copy) {
      const ask = view.asks.find((a) => String(a.key) === copy.dataset.askCopy);
      try {
        await navigator.clipboard.writeText((ask && ask.reply) || '');
        copy.textContent = 'Copied';
      } catch (_) {
        copy.textContent = 'Copy failed';
      }
      setTimeout(() => { copy.textContent = 'Copy'; }, 1200);
    }
  });
  try {
    const pause = localStorage.getItem('minutes-replay-pause-answering');
    if (pause !== null) $('cp-pause').checked = pause === '1';
  } catch (_) { /* private mode */ }
  $('cp-pause').addEventListener('change', () => {
    try { localStorage.setItem('minutes-replay-pause-answering', $('cp-pause').checked ? '1' : '0'); } catch (_) { /* private mode */ }
  });

  // ---------------------------------------------------------------------
  // Timeline (canvas): the lanes are drawn once into a cached layer per
  // view (zoom, size, axis, theme, selection); each frame only blits it and
  // draws the cursor.
  // ---------------------------------------------------------------------

  const LABEL_W = 116;
  const LANE_H = 20;
  const LIVE_H = 46;
  const canvas = $('tl');
  const ctx = canvas.getContext('2d');
  const layer = document.createElement('canvas');
  const lctx = layer.getContext('2d');
  let layerKey = '';
  let layout = [];
  let colors = null;

  function palette() {
    if (!colors) {
      const style = getComputedStyle(document.documentElement);
      const v = (name) => style.getPropertyValue(name).trim();
      colors = {
        ink: v('--ink'), subtle: v('--ink-subtle'), hair: v('--hairline'), muted: v('--ink-muted'),
        info: v('--info'), success: v('--success'), warning: v('--warning'), error: v('--error'),
        surface0: v('--surface-0'), surface1: v('--surface-1'), recording: v('--recording'),
      };
    }
    return colors;
  }

  function toneColor(tone) {
    const c = palette();
    return { info: c.info, success: c.success, warning: c.warning, error: c.error, neutral: c.muted, subtle: c.subtle }[tone] || c.muted;
  }

  function fitTimeline() {
    const end = Math.max(1000, endOfAxis());
    view.tl = { v0: -end * 0.01, v1: end * 1.01 };
  }

  const xOf = (ms, width) => LABEL_W + ((ms - view.tl.v0) / (view.tl.v1 - view.tl.v0)) * (width - LABEL_W);
  const msOf = (x, width) => view.tl.v0 + ((x - LABEL_W) / (width - LABEL_W)) * (view.tl.v1 - view.tl.v0);

  function timelineHeight() {
    return view.lanes.order.reduce((h, [id]) => h + (id === 'live' ? LIVE_H : LANE_H), 0) + 22;
  }

  function drawLayer(width, height, dpr) {
    if (layer.width !== Math.round(width * dpr) || layer.height !== Math.round(height * dpr)) {
      layer.width = Math.round(width * dpr);
      layer.height = Math.round(height * dpr);
    }
    const c = lctx;
    const p = palette();
    c.setTransform(dpr, 0, 0, dpr, 0, 0);
    c.clearRect(0, 0, width, height);
    c.font = '11px "Source Sans 3", sans-serif';
    const axisKey = view.axis;
    layout = [];
    // Time grid.
    const span = view.tl.v1 - view.tl.v0;
    const steps = [1000, 5000, 10000, 30000, 60000, 120000, 300000, 600000, 1800000];
    const step = steps.find((s) => span / s < 12) || 3600000;
    c.fillStyle = p.subtle;
    c.strokeStyle = p.hair;
    for (let t = Math.ceil(view.tl.v0 / step) * step; t < view.tl.v1; t += step) {
      const x = xOf(t, width);
      c.beginPath();
      c.moveTo(x, 0);
      c.lineTo(x, height - 16);
      c.stroke();
      c.fillText(R.clock(t), x + 2, height - 4);
    }
    let y = 4;
    for (const [id] of view.lanes.order) {
      const h = id === 'live' ? LIVE_H : LANE_H;
      c.strokeStyle = p.hair;
      c.beginPath();
      c.moveTo(0, y + h);
      c.lineTo(width, y + h);
      c.stroke();
      if (id === 'live') drawSeries(c, y, h, width);
      else {
        // Items narrower than a pixel that share one are drawn (and hit) once.
        let lastPx = -1;
        let lastTone = '';
        for (const item of view.lanes.lanes[id] || []) {
          const [a, b] = item[axisKey];
          if (b < view.tl.v0 || a > view.tl.v1) continue;
          const x0 = xOf(a, width);
          const x1 = Math.max(xOf(b, width), x0 + (item.bar ? 2 : 1.5));
          const px = Math.round(x0);
          const selected = view.selected && view.selected.item === item;
          if (!selected && !item.bar && !item.first && px === lastPx && item.tone === lastTone) continue;
          lastPx = px;
          lastTone = item.tone;
          c.fillStyle = toneColor(item.tone);
          const top = item.big ? y + 1 : y + 4;
          const hh = item.big ? h - 2 : h - 8;
          c.fillRect(x0, top, x1 - x0, hh);
          if (item.first) {
            c.fillStyle = p.ink;
            c.fillRect(xOf(item.first[axisKey], width) - 0.5, y + 2, 1.5, h - 4);
          }
          if (item.differs) {
            c.fillStyle = toneColor('error');
            c.fillText('≠', x1 + 2, y + h - 5);
          }
          if (selected) {
            c.strokeStyle = p.ink;
            c.lineWidth = 1.5;
            c.strokeRect(x0 - 1, top - 1, x1 - x0 + 2, hh + 2);
            c.lineWidth = 1;
          }
          layout.push({ item, lane: id, x0: x0 - 2, x1: x1 + 2, y0: y, y1: y + h });
        }
      }
      y += h;
    }
  }

  function drawTimeline() {
    if (!view.lanes) return;
    const wrap = $('tl-canvas-wrap');
    const width = wrap.clientWidth;
    if (!width) return;
    const height = timelineHeight();
    const dpr = window.devicePixelRatio || 1;
    if (canvas.width !== Math.round(width * dpr) || canvas.height !== Math.round(height * dpr)) {
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      canvas.style.height = `${height}px`;
    }
    const key = [width, height, dpr, view.tl.v0, view.tl.v1, view.axis, document.documentElement.dataset.theme || '', view.lanesVersion, view.selected ? view.selected.i : -1].join('|');
    if (key !== layerKey) {
      drawLayer(width, height, dpr);
      layerKey = key;
    }
    const p = palette();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(layer, 0, 0);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    // What has not happened yet is dimmed.
    const cx = xOf(view.cursor, width);
    if (cx < width) {
      ctx.globalAlpha = 0.55;
      ctx.fillStyle = p.surface0;
      ctx.fillRect(Math.max(cx, LABEL_W), 0, width - Math.max(cx, LABEL_W), height - 16);
      ctx.globalAlpha = 1;
    }
    if (cx >= LABEL_W) {
      ctx.strokeStyle = p.recording;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(cx, 0);
      ctx.lineTo(cx, height - 16);
      ctx.stroke();
      ctx.lineWidth = 1;
    }
    // Lane names over everything.
    ctx.fillStyle = p.surface1;
    ctx.fillRect(0, 0, LABEL_W - 4, height - 16);
    ctx.font = '11px "Source Sans 3", sans-serif';
    ctx.fillStyle = p.subtle;
    let y = 4;
    for (const [id, name] of view.lanes.order) {
      const h = id === 'live' ? LIVE_H : LANE_H;
      ctx.fillText(name, 8, y + h / 2 + 4);
      y += h;
    }
    const range = `${view.axis === 'audio' ? 'Meeting time' : 'Wall time'} ${R.clock(Math.max(0, view.tl.v0))}–${R.clock(view.tl.v1)}`;
    if ($('tl-range').textContent !== range) $('tl-range').textContent = range;
  }

  function drawSeries(c, y, h, width) {
    const series = view.lanes.series;
    if (!series.length) return;
    let maxBacklog = 2;
    for (const s of series) if (s.backlog > maxBacklog) maxBacklog = s.backlog;
    const key = view.axis;
    // Model work in flight.
    c.fillStyle = toneColor('warning');
    c.globalAlpha = 0.12;
    for (let k = 1; k < series.length; k += 1) {
      if (!series[k].llm) continue;
      const x0 = xOf(series[k - 1][key], width);
      c.fillRect(x0, y + 1, Math.max(1, xOf(series[k][key], width) - x0), h - 2);
    }
    c.globalAlpha = 1;
    const line = (value, max, color) => {
      c.strokeStyle = color;
      c.beginPath();
      let started = false;
      for (const s of series) {
        const v = value(s);
        if (v == null) { started = false; continue; }
        const x = xOf(s[key], width);
        const yy = y + h - 3 - Math.min(1, v / max) * (h - 6);
        if (!started) c.moveTo(x, yy);
        else c.lineTo(x, yy);
        started = true;
      }
      c.stroke();
    };
    // RTF limit (0.9 of a 1.5 scale).
    c.strokeStyle = toneColor('error');
    c.setLineDash([3, 3]);
    const limitY = y + h - 3 - (0.9 / 1.5) * (h - 6);
    c.beginPath();
    c.moveTo(LABEL_W, limitY);
    c.lineTo(width, limitY);
    c.stroke();
    c.setLineDash([]);
    line((s) => s.rtf, 1.5, toneColor('info'));
    line((s) => s.backlog, maxBacklog, toneColor('warning'));
    layout.push({ series: true, x0: LABEL_W, x1: width, y0: y, y1: y + h });
  }

  function hit(x, y) {
    let best = null;
    for (const l of layout) {
      if (l.series || y < l.y0 || y > l.y1 || x < l.x0 || x > l.x1) continue;
      const d = Math.abs((l.x0 + l.x1) / 2 - x);
      if (!best || d < best.d) best = { l, d };
    }
    return best && best.l;
  }

  function seriesAt(ms) {
    const s = view.lanes.series;
    let best = null;
    for (const p of s) {
      if (p[view.axis] <= ms) best = p;
      else break;
    }
    return best;
  }

  let drag = null;
  canvas.addEventListener('pointerdown', (e) => {
    drag = { x: e.offsetX, v0: view.tl.v0, v1: view.tl.v1, moved: false };
    canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener('pointermove', (e) => {
    const width = canvas.clientWidth;
    if (drag) {
      const dx = e.offsetX - drag.x;
      if (Math.abs(dx) > 3) drag.moved = true;
      if (drag.moved) {
        const per = (drag.v1 - drag.v0) / (width - LABEL_W);
        view.tl = { v0: drag.v0 - dx * per, v1: drag.v1 - dx * per };
        drawTimeline();
      }
      return;
    }
    const tip = $('tl-tip');
    const found = hit(e.offsetX, e.offsetY);
    let text = null;
    if (found) {
      const r = view.records[found.item.i];
      text = `${found.item.label}\n${R.clock(r.audio_ms)} meeting · ${R.clock(r.t_ms)} wall`;
    } else if (view.lanes && e.offsetX > LABEL_W) {
      const s = layout.find((l) => l.series && e.offsetY >= l.y0 && e.offsetY <= l.y1) && seriesAt(msOf(e.offsetX, width));
      if (s) text = `backlog ${s.backlog.toFixed(1)} s · RTF ${s.rtf == null ? '–' : s.rtf.toFixed(2)}${s.llm ? ' · model busy' : ''}`;
    }
    if (text) {
      tip.hidden = false;
      tip.textContent = text;
      tip.style.left = `${Math.min(e.offsetX + 12, width - 260)}px`;
      tip.style.top = `${e.offsetY + 14}px`;
    } else tip.hidden = true;
  });
  canvas.addEventListener('pointerleave', () => { $('tl-tip').hidden = true; });
  canvas.addEventListener('pointerup', (e) => {
    const was = drag;
    drag = null;
    if (!was || was.moved || !view.records.length) return;
    const found = hit(e.offsetX, e.offsetY);
    if (found) {
      seekCount(found.item.i + 1);
      select(found.item.i, found.item);
    } else if (e.offsetX > LABEL_W) {
      seekTime(msOf(e.offsetX, canvas.clientWidth));
      syncAudio();
    }
  });
  canvas.addEventListener('wheel', (e) => {
    if (!view.records.length) return;
    if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
    e.preventDefault();
    const width = canvas.clientWidth;
    const at = msOf(e.offsetX, width);
    const factor = Math.exp(e.deltaY * 0.0015);
    const v0 = at - (at - view.tl.v0) * factor;
    const v1 = at + (view.tl.v1 - at) * factor;
    if (v1 - v0 < 200) return;
    view.tl = { v0, v1 };
    drawTimeline();
  }, { passive: false });
  canvas.addEventListener('dblclick', () => { fitTimeline(); drawTimeline(); });
  $('fit').addEventListener('click', () => { fitTimeline(); drawTimeline(); });
  window.addEventListener('resize', drawTimeline);
  if (window.matchMedia) {
    window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', () => { colors = null; layerKey = ''; drawTimeline(); });
  }

  // ---------------------------------------------------------------------
  // Inspector
  // ---------------------------------------------------------------------

  function linkedRecords(record) {
    const p = record.payload || {};
    const keysOf = [];
    if (p.decision_id) keysOf.push((q) => q.decision_id === p.decision_id);
    if (p.id != null && (record.name === 'copilot-request' || record.name === 'sidecar' || record.name === 'timeline-request')) {
      keysOf.push((q, r) => r.name === record.name && q.id === p.id);
    }
    if (p.message_id) keysOf.push((q) => q.id === p.message_id || q.message_id === p.message_id);
    if (record.name === 'copilot-delta') keysOf.push((q, r) => r.name === 'copilot-request' && q.id === p.id);
    if (!keysOf.length) return [];
    return view.records.filter((r) => r !== record && keysOf.some((k) => k(r.payload || {}, r))).slice(0, 40);
  }

  function select(i, item) {
    const record = view.records[i];
    if (!record) return;
    view.selected = { i, item };
    const linked = linkedRecords(record);
    $('inspector').innerHTML = `
      <p><b>${esc(record.name)}</b> <span class="subtle">${esc(record.type)} · #${i} · ${esc(R.clock(record.audio_ms))} meeting · ${esc(R.clock(record.t_ms))} wall</span></p>
      <pre class="json">${esc(JSON.stringify(record.payload, null, 2))}</pre>
      ${linked.length ? `<p class="subtle">Linked events</p><ul class="linked">${linked.map((r) => `<li><button type="button" data-at="${r.i}">${esc(R.clock(r.audio_ms))}</button> ${esc(r.name)} ${esc((r.payload && (r.payload.phase || r.payload.step)) || '')}</li>`).join('')}</ul>` : ''}`;
    drawTimeline();
  }

  // ---------------------------------------------------------------------
  // Start: a served viewer lists the runs; `#run=<name>` opens one,
  // `#sample` the demo (`#sample&at=300` also seeks to record 300).
  // ---------------------------------------------------------------------

  async function start() {
    view.served = await detectServer();
    renderCopilot(R.initialState());
    if (view.served) {
      document.body.classList.add('served');
      $('runs-ctl').hidden = false;
      $('runs-inline').hidden = false;
      $('runs-pop').innerHTML = runRows(view.runs) + RUNS_FOOT;
      $('runs-inline').innerHTML = `<p class="subtle" style="padding:10px 12px 0;margin:0">Runs on this machine (newest first). Click one to play it and ask its Copilot live.</p>${runRows(view.runs)}`;
    } else {
      note('Start <code>replay serve</code> to ask the Copilot live.');
      $('audio-ctl').classList.add('off');
    }
    const hash = new URLSearchParams(location.hash.slice(1));
    if (view.served && hash.has('run')) {
      await openRun(hash.get('run'));
      return;
    }
    if (!hash.has('sample')) return;
    loadSample();
    if (hash.has('at')) {
      setPlaying(false);
      seekCount(Number(hash.get('at')) || 0);
    }
  }
  start();
})();
