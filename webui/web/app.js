'use strict';

const $ = (id) => document.getElementById(id);
const POLL_MS = 5000;

let caps = {};
let signal = {};
let history = [];
let toastTimer = null;

// --- утилиты -----------------------------------------------------------

function toast(message, kind) {
  const el = $('toast');
  el.textContent = message;
  el.className = 'toast' + (kind ? ' ' + kind : '');
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, kind === 'error' ? 9000 : 4500);
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

const has = (v) => v !== null && v !== undefined;
const num = (v, d) => (has(v) ? Number(v).toFixed(d || 0) : '—');

/** LTE-диапазон по EARFCN — те же границы, что и на сервере (3GPP 36.101). */
const NDL = [
  [1, 0, 599], [2, 600, 1199], [3, 1200, 1949], [4, 1950, 2399], [5, 2400, 2649],
  [6, 2650, 2749], [7, 2750, 3449], [8, 3450, 3799], [9, 3800, 4149], [10, 4150, 4749],
  [11, 4750, 4949], [12, 5010, 5179], [13, 5180, 5279], [14, 5280, 5379], [17, 5730, 5849],
  [18, 5850, 5999], [19, 6000, 6149], [20, 6150, 6449], [21, 6450, 6599], [22, 6600, 7399],
  [25, 8040, 8689], [26, 8690, 9039], [28, 9210, 9659], [38, 37750, 38249], [39, 38250, 38649],
  [40, 38650, 39649], [41, 39650, 41589], [42, 41590, 43589], [43, 43590, 45589],
  [66, 66436, 67335],
];
const bandOf = (earfcn) => {
  const hit = NDL.find(([, lo, hi]) => earfcn >= lo && earfcn <= hi);
  return hit ? 'B' + hit[0] : '—';
};

// --- шкала качества ----------------------------------------------------

/**
 * RSRP — величина порядковая, поэтому кодируется положением на линейке дБм,
 * а не цветом. Слово рядом называет категорию: цвет ничего не решает в одиночку.
 */
const GAUGE_MIN = -120;
const GAUGE_MAX = -60;

function verdict(rsrp) {
  if (rsrp >= -80) return 'отличный сигнал';
  if (rsrp >= -90) return 'хороший сигнал';
  if (rsrp >= -100) return 'слабый сигнал, скорость будет проседать';
  return 'очень слабый сигнал, связь неустойчива';
}

function renderGauge(rsrp) {
  const mark = $('gauge-mark');
  if (!has(rsrp)) {
    mark.hidden = true;
    $('gauge-verdict').textContent = 'Жду первого замера';
    return;
  }
  const clamped = Math.min(GAUGE_MAX, Math.max(GAUGE_MIN, rsrp));
  const pct = ((clamped - GAUGE_MIN) / (GAUGE_MAX - GAUGE_MIN)) * 100;
  mark.hidden = false;
  mark.style.left = pct + '%';
  $('gauge-verdict').textContent = num(rsrp) + ' дБм — ' + verdict(rsrp);
}

// --- отрисовка ---------------------------------------------------------

function renderSignal(s) {
  signal = s;
  $('m-rsrp').textContent = num(s.rsrp);
  $('m-rsrq').textContent = num(s.rsrq, 1);
  $('m-sinr').textContent = num(s.sinr, 1);
  $('operator').textContent = s.operator || 'оператор неизвестен';

  const cell = [];
  if (has(s.earfcn)) cell.push('EARFCN ' + s.earfcn + ' · ' + (s.band || bandOf(s.earfcn)));
  if (has(s.pci)) cell.push('PCI ' + s.pci);
  $('cell').textContent = cell.join('  ·  ') || '';

  const reg = $('reg');
  reg.textContent = s.registered ? 'в сети' : 'нет регистрации';
  reg.className = 'badge ' + (s.registered ? 'on' : 'off');

  renderGauge(s.rsrp);
}

function renderLock(l) {
  const parts = [];
  if (has(l.earfcn)) parts.push('несущая EARFCN ' + l.earfcn);
  if (has(l.pci)) parts.push('EARFCN ' + l.pciEarfcn + ' · PCI ' + l.pci);

  $('lock-state').textContent = parts.length
    ? 'Зафиксировано: ' + parts.join('; ')
    : 'Модем выбирает соту сам';

  // Источник сведений важнее самих сведений: у Intel фиксацию из модема
  // не прочитать, и выдавать свою запись за его показание нельзя.
  const note = $('lock-note');
  if (l.conflict) {
    note.textContent = 'Установлены обе фиксации сразу — в модеме конфликт приоритетов, оставьте одну.';
    note.hidden = false;
  } else if (l.fromOurRecords && parts.length) {
    note.textContent = 'По записям панели. Модем не умеет сообщать текущую фиксацию, подтвердить нечем.';
    note.hidden = false;
  } else {
    note.hidden = true;
  }
}

function renderCaps(c) {
  caps = c;
  const intel = c.family === 'intel';

  $('card-scan').hidden = !c.neighbors;
  $('card-bands').hidden = !c.bands;
  $('scan-hint').textContent = c.neighbors || '';
  $('form-bands').hidden = !c.bandsWritable;
  $('bands-note').textContent = c.bandsWritable
    ? 'Формат маски зависит от прошивки. Неверная маска может лишить модем связи.'
    : 'Прошивка отдаёт состав агрегации только для чтения.';

  // У Intel freq_lock требует и несущую, и сектор — «любая сота на несущей»
  // такой командой не выражается, поэтому форма прячется.
  $('form-earfcn').hidden = intel;

  $('lock-help').textContent = intel
    ? 'Фиксация задаётся парой EARFCN + PCI и применяется перезапуском радио. Если такой соты нет в эфире, модем останется без регистрации — выручит «Снять фиксацию».'
    : 'Фиксация несущей и фиксация соты взаимоисключающие: включение одной снимает другую. Применяется после перезапуска модема.';

  $('at-warn').textContent = intel
    ? 'Команда at@sic пишет в настройки модема напрямую. На свой страх и риск.'
    : 'Команда at^efs пишет в NV-память модема напрямую и может его окирпичить. На свой страх и риск.';

  if (c.family === 'unknown') {
    $('lock-state').textContent = 'Семейство модема не определено — фиксация недоступна';
    $('lock-help').textContent =
      'Модем не ответил ни на at^efs (Qualcomm), ни на AT+XCESQ (Intel).';
    document
      .querySelectorAll('#card-lock button, #card-lock input')
      .forEach((el) => { if (el.id !== 'btn-reset') el.disabled = true; });
  }
}

function renderAggregation(b) {
  $('bands-raw').textContent = b.raw || '—';
  const a = b.aggregation;
  const shown = !!a;
  $('agg-facts').hidden = !shown;
  $('agg-widths-wrap').hidden = !shown;
  if (!shown) return;

  $('agg-carriers').textContent = a.carriers;
  $('agg-total').textContent = a.totalMhz + ' МГц';
  $('agg-bands').textContent = a.bands.length ? a.bands.join(' + ') : '—';
  $('agg-widths').textContent = a.bandwidths.map((w) => w + ' МГц').join('  +  ');
}

function renderNeighbors(list) {
  const table = $('neighbors');
  const tbody = table.querySelector('tbody');
  tbody.textContent = '';

  if (!list.length) {
    table.hidden = true;
    $('scan-empty').hidden = false;
    $('scan-empty').textContent = 'Модем не вернул ни одной соседней соты.';
    return;
  }

  for (const n of list) {
    const tr = document.createElement('tr');
    // Своя сота в списке соседей — полезная опора для сравнения.
    if (n.earfcn === signal.earfcn && n.pci === signal.pci) tr.className = 'is-serving';

    for (const v of [n.earfcn, bandOf(n.earfcn), n.pci, num(n.rsrp), num(n.rsrq, 1)]) {
      const td = document.createElement('td');
      td.textContent = v;
      tr.appendChild(td);
    }

    const td = document.createElement('td');
    const btn = document.createElement('button');
    btn.className = 'small';
    btn.textContent = 'Зафиксировать';
    btn.disabled = caps.family === 'unknown';
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
  $('scan-empty').hidden = true;
}

// --- графики -----------------------------------------------------------

/**
 * По одному графику на метрику. Совмещать RSRP и SINR на общей оси нельзя:
 * у них разные единицы, и любая общая шкала делает одну из линий плоской —
 * ровно это и было видно на прежней версии панели.
 */
function drawPlot(canvasId, key, color, unit, minSpan) {
  const canvas = $(canvasId);
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth || 320;
  const h = 64;

  canvas.width = w * dpr;
  canvas.height = h * dpr;
  // Битмап держим в физических пикселях, а высоту в вёрстке задаём явно: без
  // неё элемент занимает h * dpr экранных пикселей, и на HiDPI график выходит
  // вдвое выше задуманного, а подсказка (top в логических координатах) уезжает.
  canvas.style.height = h + 'px';
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const pts = history.map((s) => s[key]);
  const valid = pts.filter(has);
  const css = getComputedStyle(document.documentElement);

  if (valid.length < 2) {
    ctx.fillStyle = css.getPropertyValue('--ink-3').trim();
    ctx.font = '12px system-ui, sans-serif';
    ctx.fillText('накапливаю данные…', 0, h / 2);
    canvas._geom = null;
    return;
  }

  // Отступ сверху и снизу оставлен под подписи min/max: без него они
  // рисовались за пределами холста и обрезались.
  const pad = 12;
  let min = Math.min(...valid);
  let max = Math.max(...valid);

  // Минимальная ширина шкалы. Без неё стабильный сигнал растягивается на всю
  // высоту графика, и колебание в 1 дБ выглядит как обвал связи.
  if (max - min < minSpan) {
    const mid = (max + min) / 2;
    min = mid - minSpan / 2;
    max = mid + minSpan / 2;
  }

  const x = (i) => (i / (pts.length - 1 || 1)) * w;
  const y = (v) => h - pad - ((v - min) / (max - min)) * (h - 2 * pad);

  // Заливка под линией: помогает прочесть уровень, не перетягивая внимание.
  const grad = ctx.createLinearGradient(0, 0, 0, h);
  grad.addColorStop(0, color + '33');
  grad.addColorStop(1, color + '00');
  ctx.fillStyle = grad;
  ctx.beginPath();
  let fillStarted = false;
  pts.forEach((v, i) => {
    if (!has(v)) return;
    if (!fillStarted) { ctx.moveTo(x(i), h); ctx.lineTo(x(i), y(v)); fillStarted = true; }
    else ctx.lineTo(x(i), y(v));
  });
  if (fillStarted) {
    const lastIdx = pts.reduce((acc, v, i) => (has(v) ? i : acc), 0);
    ctx.lineTo(x(lastIdx), h);
    ctx.closePath();
    ctx.fill();
  }

  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  ctx.lineJoin = 'round';
  ctx.lineCap = 'round';
  ctx.beginPath();
  let started = false;
  pts.forEach((v, i) => {
    if (!has(v)) { started = false; return; }
    if (!started) { ctx.moveTo(x(i), y(v)); started = true; }
    else ctx.lineTo(x(i), y(v));
  });
  ctx.stroke();

  // Подписи ставим внутрь холста: max над своей линией, min под своей.
  // Сдвиги подобраны под pad: подпись в 10px требует 10px отступа, иначе
  // ограничитель срабатывает и затягивает её обратно на линию данных.
  ctx.fillStyle = css.getPropertyValue('--ink-3').trim();
  ctx.font = '10px ui-monospace, monospace';
  ctx.textBaseline = 'alphabetic';
  ctx.fillText(max.toFixed(0), 2, Math.max(9, y(max) - 3));
  ctx.fillText(min.toFixed(0), 2, Math.min(h - 2, y(min) + 10));

  canvas._geom = { pts, x, y, min, max, w, h, unit, color };
}

/** Наведение: перекрестие и значение в точке — график должен отвечать курсору. */
function attachHover(canvasId) {
  const canvas = $(canvasId);
  const wrap = canvas.parentElement;
  let tip = null;

  const clear = () => {
    if (tip) { tip.remove(); tip = null; }
    if (canvas._geom) redraw();
  };

  canvas.addEventListener('mouseleave', clear);
  canvas.addEventListener('mousemove', (ev) => {
    const g = canvas._geom;
    if (!g) return;

    const rect = canvas.getBoundingClientRect();
    const px = ev.clientX - rect.left;
    const i = Math.round((px / rect.width) * (g.pts.length - 1));
    const v = g.pts[i];
    if (!has(v)) return;

    redraw();
    const ctx = canvas.getContext('2d');
    const cx = g.x(i);
    ctx.strokeStyle = getComputedStyle(document.documentElement)
      .getPropertyValue('--ink-3').trim();
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(cx, 0);
    ctx.lineTo(cx, g.h);
    ctx.stroke();

    ctx.fillStyle = g.color;
    ctx.beginPath();
    ctx.arc(cx, g.y(v), 3.5, 0, Math.PI * 2);
    ctx.fill();

    if (!tip) {
      tip = document.createElement('div');
      tip.className = 'tip';
      wrap.appendChild(tip);
    }
    const ago = Math.round(((g.pts.length - 1 - i) * POLL_MS) / 1000);
    tip.textContent = v.toFixed(1) + ' ' + g.unit + (ago ? '  ·  ' + ago + ' с назад' : '  ·  сейчас');
    tip.style.left = cx + 'px';
    tip.style.top = g.y(v) + 'px';
  });
}

function redraw() {
  const css = getComputedStyle(document.documentElement);
  // 12 дБ для RSRP и 10 единиц для SINR — заметное изменение, а не дрожание.
  drawPlot('chart-rsrp', 'rsrp', css.getPropertyValue('--rsrp').trim(), 'дБм', 12);
  drawPlot('chart-sinr', 'sinr', css.getPropertyValue('--sinr').trim(), '', 10);
  $('plot-now-rsrp').textContent = has(signal.rsrp) ? num(signal.rsrp) + ' дБм' : '';
  $('plot-now-sinr').textContent = has(signal.sinr) ? num(signal.sinr, 1) : '';
}

// --- опрос -------------------------------------------------------------

async function refresh() {
  const st = await api('/api/status');
  $('transport').textContent = st.transport;
  renderCaps(st.caps);
  renderLock(st.lock);
  renderSignal(st.signal);

  const h = await api('/api/history');
  history = h.samples;
  redraw();
}

async function tick() {
  try {
    await refresh();
  } catch (e) {
    // Связь могла пропасть из-за перезапуска радио — не шумим на каждый тик.
    console.warn('опрос не удался:', e.message);
  }
}

async function loadBands() {
  if (!caps.bands) return;
  try {
    renderAggregation(await api('/api/bands'));
  } catch (e) {
    $('bands-raw').textContent = 'ошибка: ' + e.message;
  }
}

// --- обработчики -------------------------------------------------------

$('form-earfcn').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const earfcn = ev.target.earfcn.value;
  run(ev.target.querySelector('button'), async () => {
    const r = await api('/api/lock/earfcn', { earfcn });
    renderLock(r.lock);
    toast(r.message, 'ok');
  });
});

$('form-pci').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const earfcn = ev.target.earfcn.value;
  const pci = ev.target.pci.value;
  run(ev.target.querySelector('button'), async () => {
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
  if (!confirm('Перезапустить радиомодуль? Связь пропадёт на 30–60 секунд.')) return;
  run(ev.target, async () => toast((await api('/api/reset', {})).message, 'ok'));
});

$('btn-scan').addEventListener('click', (ev) =>
  run(ev.target, async () => renderNeighbors((await api('/api/scan', {})).neighbors))
);

$('form-bands').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const mask = ev.target.mask.value;
  if (!confirm('Записать маску ' + mask + '? Неверная маска может лишить модем связи.')) return;
  run(ev.target.querySelector('button'), async () => {
    toast((await api('/api/bands', { mask })).message, 'ok');
    await loadBands();
  });
});

$('form-at').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const cmd = ev.target.cmd.value;
  run(ev.target.querySelector('button'), async () => {
    const r = await api('/api/at', { cmd });
    $('at-out').textContent = r.raw || r.body || '(пустой ответ)';
  });
});

let resizeTimer = null;
window.addEventListener('resize', () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(redraw, 120);
});

// --- старт -------------------------------------------------------------

attachHover('chart-rsrp');
attachHover('chart-sinr');

(async function start() {
  await tick();
  await loadBands();

  // Скан при загрузке: список соседей — это одна AT-команда, и без него
  // карточка стоит пустой ровно с той информацией, ради которой открыта.
  if (caps.neighbors) {
    try {
      renderNeighbors((await api('/api/scan', {})).neighbors);
    } catch (e) {
      console.warn('первичный скан не удался:', e.message);
    }
  }

  setInterval(tick, POLL_MS);
})();
