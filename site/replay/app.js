/*
 * Replay viewer UI. Everything shown is derived from the loaded records:
 * the cursor is a record count `n`, and the screen renders stateAt(n) (see
 * parser.js). Playing advances a time cursor on the chosen axis and maps it
 * to `n`. Files never leave the browser.
 */
(function () {
  'use strict';
  const R = window.ReplayParser;
  const $ = (id) => document.getElementById(id);

  const view = {
    records: [],
    index: null,
    lanes: null,
    info: null,
    report: null,
    state: null,
    n: 0,
    cursor: 0, // ms on the current axis
    axis: 'audio',
    speed: 1,
    playing: false,
    selected: null,
    tl: { v0: 0, v1: 1 },
    renderedLines: 0,
    followTranscript: true,
  };

  // ---------------------------------------------------------------------
  // Loading
  // ---------------------------------------------------------------------

  function load(eventsText, reportJson) {
    let records;
    try {
      records = R.parseJsonl(eventsText);
    } catch (error) {
      alert(`Could not read the events: ${error.message}`);
      return;
    }
    view.records = records;
    view.index = R.buildIndex(records);
    view.lanes = R.buildLanes(records);
    view.info = R.summarize(records);
    if (reportJson !== undefined) view.report = reportJson;
    view.n = 0;
    view.cursor = 0;
    view.state = R.stateAt(view.index, 0);
    view.renderedLines = -1;
    view.selected = null;
    $('drop').hidden = true;
    $('run').hidden = false;
    $('rec-pill').hidden = false;
    for (const id of ['play', 'prev', 'next', 'jump-name']) $(id).disabled = false;
    const select = $('jump-name');
    select.innerHTML = view.info.names.map((n) => `<option ${n === 'copilot-state' ? 'selected' : ''}>${esc(n)}</option>`).join('');
    const steps = [...new Set(view.info.detector.map((d) => d.step))].sort();
    $('detector-filter').innerHTML = '<option value="">All</option>' + steps.map((s) => `<option>${esc(s)}</option>`).join('');
    fitTimeline();
    renderStatic();
    seekCount(0);
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
    if (events) load(events, report === undefined ? null : report);
    else if (report && view.records.length) {
      view.report = report;
      renderStatic();
    } else alert('Choose an events.jsonl (and optionally its report.json).');
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
    if (!window.REPLAY_SAMPLE) return alert('The sample is missing (sample-data.js).');
    load(window.REPLAY_SAMPLE.events, window.REPLAY_SAMPLE.report || null);
  };
  $('load-sample').addEventListener('click', loadSample);
  // `#sample` opens the demo; `#sample&at=300` also seeks to record 300.
  window.addEventListener('load', () => {
    const hash = new URLSearchParams(location.hash.slice(1));
    if (!hash.has('sample')) return;
    loadSample();
    if (hash.has('at')) seekCount(Number(hash.get('at')) || 0);
  });
  document.querySelectorAll('[data-load-sample]').forEach((b) => b.addEventListener('click', loadSample));

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
  function tick(now) {
    if (!view.playing) return;
    const dt = lastFrame ? now - lastFrame : 0;
    lastFrame = now;
    const next = view.cursor + dt * view.speed;
    if (next >= endOfAxis()) {
      seekTime(endOfAxis());
      setPlaying(false);
      return;
    }
    seekTime(next);
    requestAnimationFrame(tick);
  }

  function setPlaying(on) {
    view.playing = on && view.records.length > 0;
    $('play-icon').innerHTML = view.playing ? '<rect x="3.5" y="2.5" width="3" height="11" rx="1"/><rect x="9.5" y="2.5" width="3" height="11" rx="1"/>' : '<path d="M4 2.5v11l9-5.5z"/>';
    $('play').setAttribute('aria-label', view.playing ? 'Pause' : 'Play');
    if (view.playing) {
      if (view.cursor >= endOfAxis()) seekTime(0);
      lastFrame = 0;
      requestAnimationFrame(tick);
    }
  }

  $('play').addEventListener('click', () => setPlaying(!view.playing));
  document.querySelectorAll('[data-speed]').forEach((b) => b.addEventListener('click', () => {
    view.speed = Number(b.dataset.speed);
    document.querySelectorAll('[data-speed]').forEach((o) => o.setAttribute('aria-pressed', String(o === b)));
  }));
  document.querySelectorAll('[data-axis]').forEach((b) => b.addEventListener('click', () => {
    if (view.axis === b.dataset.axis) return;
    view.axis = b.dataset.axis;
    document.querySelectorAll('[data-axis]').forEach((o) => o.setAttribute('aria-pressed', String(o === b)));
    if (view.records.length) {
      fitTimeline();
      seekCount(view.n);
    }
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
    const finals = view.state.finalLines;
    if (!finals) return true;
    return finals.some((l) => seconds >= (l.start ?? 0) - 2 && seconds <= (l.end ?? l.start ?? 0) + 2);
  }

  /** The tiny Markdown the replies use: paragraphs, "- " lists, **bold**, citations. */
  function markdown(text) {
    const inline = (s) => esc(s)
      .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
      .replace(CITE, (whole, h, m, sec) => {
        const seconds = Number(h || 0) * 3600 + Number(m) * 60 + Number(sec);
        return `<button type="button" class="cite${citeValid(seconds) ? '' : ' bad'}" data-seconds="${seconds}" title="${citeValid(seconds) ? 'Jump to this moment' : 'No final-transcript line at this time'}">${whole}</button>`;
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
    if (cite) jumpTranscript(Number(cite.dataset.seconds));
    const at = e.target.closest('[data-at]');
    if (at) {
      seekCount(Number(at.dataset.at) + 1);
      select(Number(at.dataset.at), null);
    }
  });

  function jumpTranscript(seconds) {
    const rows = [...$('transcript').querySelectorAll('.row')];
    let best = null;
    for (const row of rows) {
      if (Number(row.dataset.start) <= seconds + 0.5) best = row;
    }
    if (!best) return;
    view.followTranscript = false;
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
    let score = '<p class="subtle">Open the run\'s <b>report.json</b> too to see the pass criteria.</p>';
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
    $('run').innerHTML = `
      <div class="run-head">
        <h2>${esc(meta.audio_name || 'Replay')}</h2>
        <p>${esc(duration)} · ${esc(mode)} · live: ${esc(meta.live_engine || '?')} · model: ${esc(meta.summary_model || '?')} · ${view.records.length} events · replay took ${esc(R.clock(view.info.endMs))}</p>
        ${limits}
      </div>
      <div class="card"><h3>Pass criteria</h3>${score}</div>
      <div class="card"><h3>Now</h3><div id="now"></div></div>`;
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

  function renderState() {
    if (!view.state) return;
    const s = view.state;
    const live = s.phase === 'live-start' || s.phase === 'idle';
    const meta = view.info.meta;
    const recorded = Math.min(s.audioMs, (meta.fed_s || meta.duration_s || 1e9) * 1000);
    $('clock').textContent = `${R.clock(s.audioMs)} meeting · ${R.clock(s.tMs)} wall · #${view.n}`;
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

    renderLiveSummary(s, live);
    renderTranscript(s);
    renderCopilot(s);
    renderNow(s);
    updateLogCursor();
    drawTimeline();
  }

  function renderLiveSummary(s, live) {
    const t = s.timeline;
    const entries = (t && t.entries) || [];
    let notice = '';
    if (t && t.status === 'failed') notice = '<p class="notice warning">Live summary paused. It will try again with the next minute of conversation.</p>';
    else if (t && t.status === 'unavailable') notice = `<p class="notice ${t.error ? 'warning' : 'neutral'}">${esc(t.error || 'Live summary needs a summary model. Choose one in Settings → Summary.')}</p>`;
    else if (t && t.status === 'disabled') notice = '<p class="notice neutral">Live summary is off. Turn it on in Settings → Summary.</p>';
    const marker = (kind) => (kind === 'decision' ? badge('success', 'Decision') : kind === 'action' ? badge('info', 'Action') : '');
    const list = entries.length
      ? `<ol>${entries.map((e) => `<li><span class="ls-time">${esc(R.clock(e.start_ms))}</span><span class="ls-text">${marker(e.kind)}${esc(e.text)}${e.speaker ? `<span class="ls-speaker">· ${esc(e.speaker)}</span>` : ''}</span></li>`).join('')}</ol>`
      : '';
    const listening = live && (!t || (t.status !== 'disabled' && t.status !== 'unavailable'));
    const tail = listening
      ? `<p class="ls-listening"><i class="${t && t.status === 'generating' ? 'pulse' : ''}"></i>${t && t.status === 'generating' ? 'Summarizing the last minute…' : entries.length === 0 ? 'Listening… key points will appear here every minute or so.' : 'Listening…'}</p>`
      : '';
    $('live-summary').innerHTML = `<button type="button" class="ls-toggle" aria-expanded="true">▾ Live summary ${entries.length ? `<span>(${entries.length})</span>` : ''}</button><div class="ls-box">${notice}${list}${tail}</div>`;
  }

  function lineHtml(line, tag) {
    return `<div class="row${view.playing ? ' rise' : ''}" data-start="${line.start}" data-i="${line.i}">
      <span class="row-time">[${esc(R.clock(line.start * 1000))}]</span>
      <div class="row-body"><span class="chip ${line.source === 'mic' ? 'mic' : 'system'}">${line.source === 'mic' ? 'You' : 'Others'}</span>${tag ? `<span class="row-tag">${esc(tag)}</span>` : ''}
      <p class="row-text">${esc(line.text)}</p></div></div>`;
  }

  let renderedTags = '';
  function renderTranscript(s) {
    const box = $('transcript');
    const starts = R.questionLineStarts(s.copilot);
    const tagKey = starts.join(',');
    const tagOf = (line) => (R.isQuestionLine(starts, line.start, line.source) ? 'Question for you' : '');
    const nearBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 80;
    if (s.lines.length === 0) {
      box.innerHTML = '<p class="empty">Listening… live transcript will appear here as people speak.</p>';
      view.renderedLines = 0;
    } else if (view.renderedLines >= 0 && view.renderedLines <= s.lines.length && tagKey === renderedTags && box.querySelector('.row')) {
      if (s.lines.length > view.renderedLines) {
        box.insertAdjacentHTML('beforeend', s.lines.slice(view.renderedLines).map((l) => lineHtml(l, tagOf(l))).join(''));
      }
    } else {
      box.innerHTML = s.lines.map((l) => lineHtml(l, tagOf(l))).join('');
    }
    view.renderedLines = s.lines.length;
    renderedTags = tagKey;
    if (nearBottom || view.playing) box.scrollTop = box.scrollHeight;
    const partials = Object.values(s.partials).sort((a, b) => (a.source === 'mic' ? -1 : 1) - (b.source === 'mic' ? -1 : 1));
    $('partials').innerHTML = partials.map((p) => `<p><b>${p.source === 'mic' ? 'You' : 'Others'}</b>${esc(p.text)}</p>`).join('');
  }

  const QUICK = [['what_to_say', 'What should I say?'], ['explain', 'Explain that'], ['catch_up', 'Catch me up'], ['follow_up', 'Draft follow-up']];

  function reply(m) {
    const working = m.status === 'pending' || m.status === 'streaming';
    if (m.status === 'error') {
      return `<div class="err-card" role="alert"><b>Copilot couldn't get an answer</b>${m.error ? `<p>${esc(m.error)}</p>` : ''}<span class="small">Retry</span>${m.canUseBuiltin ? ' <span class="small">Use built-in model instead</span>' : ''}</div>`;
    }
    const body = m.reply ? `<div class="reply">${markdown(m.reply)}</div>` : working ? '<p class="working"><span class="pulse">✦</span> Listening back…</p>' : '';
    const stats = m.stats ? `<p class="stats">first word ${m.stats.firstTokenMs ?? '–'} ms · ${m.stats.tokensPerSecond != null ? Number(m.stats.tokensPerSecond).toFixed(1) : '–'} tok/s · reused ${m.stats.reusedTokens ?? '–'}/${m.stats.promptTokens ?? '–'}</p>` : '';
    const actions = working ? '<div class="actions-row"><span class="small">■ Stop</span></div>' : m.reply ? `<div class="actions-row"><span class="small">Copy</span>${m.kind !== 'suggestion' ? '<span class="small">Shorter</span>' : ''}</div>` : '';
    return `${body}${m.status === 'stopped' ? '<p class="stats">Stopped</p>' : ''}${actions}${stats}`;
  }

  function renderCopilot(s) {
    const c = s.copilot;
    const messages = (c && c.messages) || [];
    const active = !!(c && c.active);
    const ready = active && (c.lineCount || 0) > 0;
    const fired = new Set(messages.filter((m) => m.action && (m.status === 'pending' || m.status === 'streaming')).map((m) => m.action));
    $('cp-pills').innerHTML = QUICK.map(([id, label]) => `<span class="${ready ? '' : 'disabled'} ${fired.has(id) ? 'fired' : ''}">${esc(label)}</span>`).join('');
    $('cp-suggestions').className = `switch${!c || c.suggestionsEnabled !== false ? ' on' : ''}`;
    const list = $('cp-list');
    const nearBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 80;
    if (!messages.length) {
      list.innerHTML = `<p class="cp-empty">${!c || !active ? 'Copilot works while a meeting is recording.' : ready ? 'Ask about the last few minutes, or pick a quick action. When someone asks you something, a suggested reply appears here.' : 'Copilot starts once the first words are transcribed.'}</p>`;
    } else {
      list.innerHTML = messages.map((m) => {
        if (m.kind === 'suggestion') {
          const activeCard = m.cardState === 'active';
          return `<div class="sugg${activeCard ? ' active' : ''}"><span class="sugg-head">OTHERS ASKED YOU · [${esc(R.clock(m.audioMs))}]</span><p class="sugg-quote">“${esc(m.prompt)}”</p>${reply(m)}${activeCard ? '<div class="sugg-foot"><span class="small">Not for me</span><span class="small">Dismiss</span></div>' : ''}</div>`;
        }
        return `<div><div class="bubble-wrap"><div class="bubble"><small>[${esc(R.clock(m.audioMs))}]</small>${esc(m.prompt)}</div></div>${reply(m)}</div>`;
      }).join('');
    }
    if (nearBottom || view.playing) list.scrollTop = list.scrollHeight;
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
      parts.push(`live backlog ${Number(c.debug.liveBacklogSecs || 0).toFixed(1)}s`);
      if (d.last) parts.push(`last: ${d.last}`);
      $('cp-debug').textContent = parts.join(' · ');
    } else $('cp-debug').textContent = '';
  }

  function renderNow(s) {
    const now = $('now');
    if (!now) return;
    const bits = [`<p>${badge('neutral', PHASE_LABEL[s.phase] || s.phase)} <span class="subtle">${s.lines.length} lines · ${(s.timeline && s.timeline.entries || []).length} summary entries · ${(s.copilot && s.copilot.messages || []).length} Copilot moments</span></p>`];
    if (s.progress) bits.push(`<p class="subtle">${esc(s.progress.name)}: ${esc(s.progress.payload && (s.progress.payload.stage || s.progress.payload.message || ''))} ${esc(s.progress.payload && s.progress.payload.progress != null ? `${s.progress.payload.progress}%` : '')}</p>`);
    if (s.finalPass) bits.push(`<p>${badge(s.finalPass.outcome === 'replaced' ? 'success' : 'warning', `final pass: ${s.finalPass.outcome}`)} <span class="subtle">${esc(s.finalPass.provider || '')} ${esc(s.finalPass.model || '')}</span></p>`);
    for (const n of s.notices.slice(-3)) bits.push(`<p class="notice warning">${esc(n.text)}</p>`);
    now.innerHTML = bits.join('');
    const runs = s.summaries;
    if (!runs.length) $('summaries').innerHTML = '<p class="subtle">No summary yet.</p>';
    else {
      const same = runs.length > 1 && runs.every((r) => r.markdown === runs[0].markdown);
      $('summaries').innerHTML = `<p>${runs.length > 1 ? badge(same ? 'success' : 'warning', same ? 'Runs identical' : 'Runs differ') : ''}</p>` +
        runs.map((r) => `<details class="detail"><summary>Run ${esc(r.run)}: ${esc(r.status)} in ${((r.ms || 0) / 1000).toFixed(0)} s</summary><pre class="summary-md">${esc(r.markdown || r.error || '')}</pre></details>`).join('');
    }
  }

  // ---------------------------------------------------------------------
  // Timeline (canvas)
  // ---------------------------------------------------------------------

  const LABEL_W = 116;
  const LANE_H = 20;
  const LIVE_H = 46;
  const canvas = $('tl');
  const ctx = canvas.getContext('2d');
  let layout = [];

  function css(name) {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  function toneColor(tone) {
    return { info: css('--info'), success: css('--success'), warning: css('--warning'), error: css('--error'), neutral: css('--ink-muted'), subtle: css('--ink-subtle') }[tone] || css('--ink-muted');
  }

  function fitTimeline() {
    const end = Math.max(1000, endOfAxis());
    view.tl = { v0: -end * 0.01, v1: end * 1.01 };
  }

  const xOf = (ms, width) => LABEL_W + ((ms - view.tl.v0) / (view.tl.v1 - view.tl.v0)) * (width - LABEL_W);
  const msOf = (x, width) => view.tl.v0 + ((x - LABEL_W) / (width - LABEL_W)) * (view.tl.v1 - view.tl.v0);

  function drawTimeline() {
    if (!view.lanes) return;
    const wrap = $('tl-canvas-wrap');
    const width = wrap.clientWidth;
    const order = view.lanes.order;
    const height = order.reduce((h, [id]) => h + (id === 'live' ? LIVE_H : LANE_H), 0) + 22;
    const dpr = window.devicePixelRatio || 1;
    if (canvas.width !== Math.round(width * dpr) || canvas.height !== Math.round(height * dpr)) {
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      canvas.style.height = `${height}px`;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);
    const ink = css('--ink');
    const subtle = css('--ink-subtle');
    const hair = css('--hairline');
    const axisKey = view.axis;
    ctx.font = '11px "Source Sans 3", sans-serif';
    layout = [];
    let y = 4;
    // Time grid.
    const span = view.tl.v1 - view.tl.v0;
    const steps = [1000, 5000, 10000, 30000, 60000, 120000, 300000, 600000, 1800000];
    const step = steps.find((s) => span / s < 12) || 3600000;
    ctx.fillStyle = subtle;
    ctx.strokeStyle = hair;
    for (let t = Math.ceil(view.tl.v0 / step) * step; t < view.tl.v1; t += step) {
      const x = xOf(t, width);
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, height - 16);
      ctx.stroke();
      ctx.fillText(R.clock(t), x + 2, height - 4);
    }
    for (const [id, name] of order) {
      const h = id === 'live' ? LIVE_H : LANE_H;
      ctx.fillStyle = subtle;
      ctx.fillText(name, 8, y + h / 2 + 4);
      ctx.strokeStyle = hair;
      ctx.beginPath();
      ctx.moveTo(0, y + h);
      ctx.lineTo(width, y + h);
      ctx.stroke();
      if (id === 'live') {
        drawSeries(y, h, width);
      } else {
        for (const item of view.lanes.lanes[id] || []) {
          const [a, b] = item[axisKey];
          if (b < view.tl.v0 || a > view.tl.v1) continue;
          const x0 = xOf(a, width);
          const x1 = Math.max(xOf(b, width), x0 + (item.bar ? 2 : 1.5));
          ctx.fillStyle = toneColor(item.tone);
          const top = item.big ? y + 1 : y + 4;
          const hh = item.big ? h - 2 : h - 8;
          ctx.globalAlpha = item.i >= view.n ? 0.4 : 1;
          ctx.fillRect(x0, top, x1 - x0, hh);
          if (item.first) {
            ctx.fillStyle = ink;
            ctx.fillRect(xOf(item.first[axisKey], width) - 0.5, y + 2, 1.5, h - 4);
          }
          if (item.differs) {
            ctx.fillStyle = toneColor('error');
            ctx.fillText('≠', x1 + 2, y + h - 5);
          }
          if (view.selected && view.selected.item === item) {
            ctx.strokeStyle = ink;
            ctx.lineWidth = 1.5;
            ctx.strokeRect(x0 - 1, top - 1, x1 - x0 + 2, hh + 2);
            ctx.lineWidth = 1;
          }
          layout.push({ item, lane: id, x0: x0 - 2, x1: x1 + 2, y0: y, y1: y + h });
        }
        ctx.globalAlpha = 1;
      }
      y += h;
    }
    // Cursor.
    const cx = xOf(view.cursor, width);
    ctx.strokeStyle = css('--recording');
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(cx, 0);
    ctx.lineTo(cx, height - 16);
    ctx.stroke();
    ctx.lineWidth = 1;
    ctx.fillStyle = css('--surface-1');
    ctx.fillRect(0, 0, LABEL_W - 4, height - 16);
    y = 4;
    for (const [id, name] of order) {
      const h = id === 'live' ? LIVE_H : LANE_H;
      ctx.fillStyle = subtle;
      ctx.fillText(name, 8, y + h / 2 + 4);
      y += h;
    }
    $('tl-range').textContent = `${view.axis === 'audio' ? 'Meeting time' : 'Wall time'} ${R.clock(Math.max(0, view.tl.v0))}–${R.clock(view.tl.v1)}`;
  }

  function drawSeries(y, h, width) {
    const series = view.lanes.series;
    if (!series.length) return;
    const maxBacklog = Math.max(2, ...series.map((s) => s.backlog));
    const key = view.axis;
    // Model work in flight.
    ctx.fillStyle = toneColor('warning');
    ctx.globalAlpha = 0.12;
    for (let k = 1; k < series.length; k += 1) {
      if (!series[k].llm) continue;
      const x0 = xOf(series[k - 1][key], width);
      ctx.fillRect(x0, y + 1, Math.max(1, xOf(series[k][key], width) - x0), h - 2);
    }
    ctx.globalAlpha = 1;
    const line = (value, max, color) => {
      ctx.strokeStyle = color;
      ctx.beginPath();
      let started = false;
      for (const s of series) {
        const v = value(s);
        if (v == null) { started = false; continue; }
        const x = xOf(s[key], width);
        const yy = y + h - 3 - Math.min(1, v / max) * (h - 6);
        if (!started) ctx.moveTo(x, yy);
        else ctx.lineTo(x, yy);
        started = true;
      }
      ctx.stroke();
    };
    // RTF limit (0.9 of a 1.5 scale).
    ctx.strokeStyle = toneColor('error');
    ctx.setLineDash([3, 3]);
    const limitY = y + h - 3 - (0.9 / 1.5) * (h - 6);
    ctx.beginPath();
    ctx.moveTo(LABEL_W, limitY);
    ctx.lineTo(width, limitY);
    ctx.stroke();
    ctx.setLineDash([]);
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
    }
  });
  canvas.addEventListener('wheel', (e) => {
    if (!view.records.length) return;
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

  // ---------------------------------------------------------------------
  // Inspector
  // ---------------------------------------------------------------------

  function linkedRecords(record) {
    const p = record.payload || {};
    const keys = [];
    if (p.decision_id) keys.push((q) => q.decision_id === p.decision_id);
    if (p.id != null && (record.name === 'copilot-request' || record.name === 'sidecar' || record.name === 'timeline-request')) {
      keys.push((q, r) => r.name === record.name && q.id === p.id);
    }
    if (p.message_id) keys.push((q) => q.id === p.message_id || q.message_id === p.message_id);
    if (record.name === 'copilot-delta') keys.push((q, r) => r.name === 'copilot-request' && q.id === p.id);
    if (!keys.length) return [];
    return view.records.filter((r) => r !== record && keys.some((k) => k(r.payload || {}, r))).slice(0, 40);
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
})();
