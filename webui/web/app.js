'use strict';

const $ = (id) => document.getElementById(id);
const POLL_MS = 5000;

let caps = {};
let toastTimer = null;

// --- утилиты -----------------------------------------------------------

function toast(message, kind) {
  const el = $('toast');
  el.textContent = message;
  el.className = 'toast' + (kind ? ' ' + kind : '');
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, kind === 'error' ? 8000 : 4000);
}

async function api(path, params) {
  const opts = params
    ? {
        method: 'POST',
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        body: new URLSearchParams(params).toString(),
      }
    : { method: 'GET' };

  const resp = await fetch(path, opts);
  let data;
  try {
    data = await resp.json();
  } catch (e) {
    throw new Error('сервер вернул не JSON (HTTP ' + resp.status + ')');
  }
  if (!resp.ok) throw new Error(data.error || 'HTTP ' + resp.status);
  return data;
}

/** Обёртка для кнопок: блокирует на время запроса и показывает ошибку. */
async function run(button, fn) {
  const prev = button && button.disabled;
  if (button) button.disabled = true;
  try {
    await fn();
  } catch (e) {
    toast(e.message, 'error');
  } finally {
    if (button) button.disabled = prev || false;
  }
}

const fmt = (v, digits) => (v === null || v === undefined ? '—' : Number(v).toFixed(digits || 0));

// --- отрисовка ---------------------------------------------------------

function renderSignal(s) {
  $('m-rsrp').textContent = fmt(s.rsrp);
  $('m-rsrq').textContent = fmt(s.rsrq, 1);
  $('m-sinr').textContent = fmt(s.sinr, 1);
  $('m-earfcn').textContent = s.earfcn === null ? '—' : s.earfcn;
  $('m-pci').textContent = s.pci === null ? '—' : s.pci;
  $('operator').textContent = s.operator || '—';

  const reg = $('reg');
  reg.textContent = s.registered ? 'в сети' : 'нет регистрации';
  reg.className = 'badge ' + (s.registered ? 'on' : 'off');
}

function renderLock(l) {
  const parts = [];
  if (l.earfcn !== null) parts.push('несущая EARFCN ' + l.earfcn);
  if (l.pci !== null) parts.push('сектор EARFCN ' + l.pciEarfcn + ' / PCI ' + l.pci);
  $('lock-state').textContent = parts.length ? 'Зафиксировано: ' + parts.join('; ') : 'Фиксация не установлена';
  $('lock-conflict').hidden = !l.conflict;
}

function renderCaps(c) {
  caps = c;
  $('card-scan').hidden = !c.neighbors;
  $('card-bands').hidden = !c.bands;
  $('scan-hint').textContent = c.neighbors ? 'команда ' + c.neighbors : '';
  $('form-bands').hidden = !c.bandsWritable;

  if (!c.efs) {
    $('lock-state').textContent = 'Модем не отвечает на at^efs — фиксация недоступна';
    document.querySelectorAll('#card-lock button, #card-lock input').forEach((el) => {
      if (el.id !== 'btn-reset') el.disabled = true;
    });
  }
}

/**
 * График RSRP и SINR. Каждая метрика масштабируется по своему диапазону:
 * у них разные единицы, общая ось сделала бы одну из линий плоской.
 */
function drawChart(samples) {
  const canvas = $('chart');
  const dpr = window.devicePixelRatio || 1;
  const cssWidth = canvas.clientWidth || 300;
  const cssHeight = 160;

  canvas.width = cssWidth * dpr;
  canvas.height = cssHeight * dpr;
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  const css = getComputedStyle(document.documentElement);
  const pad = 4;

  const series = [
    { key: 'rsrp', color: css.getPropertyValue('--rsrp').trim() },
    { key: 'sinr', color: css.getPropertyValue('--sinr').trim() },
  ];

  for (const s of series) {
    const points = samples.map((x) => x[s.key]);
    const valid = points.filter((v) => v !== null && v !== undefined);
    if (valid.length < 2) continue;

    let min = Math.min(...valid);
    let max = Math.max(...valid);
    if (max - min < 1) { max += 0.5; min -= 0.5; }

    const x = (i) => pad + (i / (points.length - 1 || 1)) * (cssWidth - 2 * pad);
    const y = (v) => cssHeight - pad - ((v - min) / (max - min)) * (cssHeight - 2 * pad);

    ctx.beginPath();
    ctx.strokeStyle = s.color;
    ctx.lineWidth = 1.75;
    ctx.lineJoin = 'round';

    let started = false;
    points.forEach((v, i) => {
      if (v === null || v === undefined) { started = false; return; }
      if (!started) { ctx.moveTo(x(i), y(v)); started = true; }
      else ctx.lineTo(x(i), y(v));
    });
    ctx.stroke();
  }
}

function renderNeighbors(list) {
  const table = $('neighbors');
  const tbody = table.querySelector('tbody');
  tbody.textContent = '';

  if (!list.length) {
    table.hidden = true;
    toast('Модем не вернул ни одной соседней соты', 'error');
    return;
  }

  for (const n of list) {
    const tr = document.createElement('tr');
    for (const v of [n.earfcn, n.pci, fmt(n.rsrp), fmt(n.rsrq, 1)]) {
      const td = document.createElement('td');
      td.textContent = v;
      tr.appendChild(td);
    }

    const td = document.createElement('td');
    const btn = document.createElement('button');
    btn.className = 'small secondary';
    btn.textContent = 'Зафиксировать';
    btn.disabled = !caps.efs;
    btn.addEventListener('click', () =>
      run(btn, async () => {
        const r = await api('/api/lock/pci', { earfcn: n.earfcn, pci: n.pci });
        renderLock(r.lock);
        toast(r.message, 'ok');
      })
    );
    td.appendChild(btn);
    tr.appendChild(td);
    tbody.appendChild(tr);
  }
  table.hidden = false;
}

// --- опрос -------------------------------------------------------------

async function refreshStatus() {
  const st = await api('/api/status');
  $('transport').textContent = st.transport;
  renderCaps(st.caps);
  renderLock(st.lock);
  renderSignal(st.signal);
}

async function refreshChart() {
  const h = await api('/api/history');
  drawChart(h.samples);
}

async function tick() {
  try {
    await refreshStatus();
    await refreshChart();
  } catch (e) {
    // Сеть могла отвалиться из-за перезагрузки модема — не шумим на каждый тик.
    console.warn('опрос не удался:', e.message);
  }
}

// --- обработчики -------------------------------------------------------

$('form-earfcn').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const btn = ev.target.querySelector('button');
  const earfcn = ev.target.earfcn.value;
  run(btn, async () => {
    const r = await api('/api/lock/earfcn', { earfcn });
    renderLock(r.lock);
    toast(r.message, 'ok');
  });
});

$('form-pci').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const btn = ev.target.querySelector('button');
  const earfcn = ev.target.earfcn.value;
  const pci = ev.target.pci.value;
  run(btn, async () => {
    const r = await api('/api/lock/pci', { earfcn, pci });
    renderLock(r.lock);
    toast(r.message, 'ok');
  });
});

$('btn-unlock').addEventListener('click', (ev) =>
  run(ev.target, async () => {
    const r = await api('/api/unlock', {});
    renderLock(r.lock);
    toast(r.message, 'ok');
  })
);

$('btn-reset').addEventListener('click', (ev) => {
  if (!confirm('Перезагрузить модем? Связь пропадёт на 30–60 секунд.')) return;
  run(ev.target, async () => {
    const r = await api('/api/reset', {});
    toast(r.message, 'ok');
  });
});

$('btn-scan').addEventListener('click', (ev) =>
  run(ev.target, async () => {
    const r = await api('/api/scan', {});
    renderNeighbors(r.neighbors);
  })
);

$('form-bands').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const btn = ev.target.querySelector('button');
  const mask = ev.target.mask.value;
  if (!confirm('Записать маску бэндов ' + mask + '? Неверная маска может лишить модем связи.')) return;
  run(btn, async () => {
    const r = await api('/api/bands', { mask });
    toast(r.message, 'ok');
    const b = await api('/api/bands');
    $('bands-raw').textContent = b.raw || '—';
  });
});

$('form-at').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const btn = ev.target.querySelector('button');
  const cmd = ev.target.cmd.value;
  run(btn, async () => {
    const r = await api('/api/at', { cmd });
    $('at-out').textContent = r.raw || r.body || '(пустой ответ)';
  });
});

window.addEventListener('resize', () => {
  refreshChart().catch(() => {});
});

// --- старт -------------------------------------------------------------

(async function start() {
  await tick();
  try {
    if (caps.bands) {
      const b = await api('/api/bands');
      $('bands-raw').textContent = b.raw || '—';
    }
  } catch (e) {
    $('bands-raw').textContent = 'ошибка: ' + e.message;
  }
  setInterval(tick, POLL_MS);
})();
