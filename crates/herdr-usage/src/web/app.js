'use strict';
const $ = id => document.getElementById(id);
const number = value => new Intl.NumberFormat('zh-CN').format(value || 0);
const isQuery = location.pathname === '/query';
const isPlans = location.pathname === '/plans';
const initial = new URLSearchParams(location.search);
const tokens = value => {
  value = value || 0;
  const divisor = Math.abs(value) >= 1e9 ? 1e9 : 1e6;
  return new Intl.NumberFormat('en-US', {maximumFractionDigits: value !== 0 && Math.abs(value) < 10000 ? 6 : 2}).format(value / divisor) + (divisor === 1e9 ? ' B' : ' M');
};
const money = value => value == null ? '未计价' : '$' + value.toFixed(2);
const modelName = model => model == null ? '未识别模型' : model;
let reportLoaded = false;
let report, page = 0, request = 0, controller;
const pageSize = 50;
function node(tag, text, className) {
  const element = document.createElement(tag);
  if (text != null) element.textContent = text;
  if (className) element.className = className;
  return element;
}
function cost(total) { return total.events === 0 ? '$0.00' : money(total.cost_usd); }
function card(label, value, description) {
  const el = node('div', null, 'card');
  el.append(node('span', label, 'label'), node('strong', value), node('small', description));
  return el;
}
function costNote(total) { return total.unpriced ? `${number(total.unpriced)} 条记录未计价，金额仅含已计价部分` : '已记录费用 · USD'; }
function parameters() {
  const params = new URLSearchParams();
  if (!isQuery) return params;
  if ($('start').value) params.set('start', $('start').value);
  if ($('end').value) params.set('end', $('end').value);
  const model = $('model').value;
  if (model === 'unknown') params.set('unknown', '1');
  else if (model.startsWith('model:')) params.set('model', model.slice(6));
  return params;
}
async function load() {
  const current = ++request;
  controller?.abort();
  controller = new AbortController();
  $('refresh').disabled = true;
  $('updated').textContent = '正在更新…';
  try {
    const response = await fetch('/api/usage?' + parameters(), {signal: controller.signal});
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || '查询失败');
    if (current !== request) return;
    report = data;
    $('error').hidden = true;
    if (isQuery) {
    $('start').value = data.start;
    $('end').value = data.end;
    const selected = reportLoaded ? $('model').value : initial.has('unknown') ? 'unknown' : initial.has('model') ? 'model:' + initial.get('model') : 'all';
    $('model').replaceChildren(new Option('全部模型', 'all'));
    for (const model of data.models) $('model').add(new Option(modelName(model), model == null ? 'unknown' : 'model:' + model));
    if ([...$('model').options].some(o => o.value === selected)) $('model').value = selected;
    reportLoaded = true;
    }
    $('context').textContent = `按服务器本地日期统计 · ${data.timezone} · ${data.first_day ? '历史记录始于 ' + data.first_day : '暂无历史记录，等待 collector 采集'}`;
    if (!isQuery) $('overview-cards').replaceChildren(...[['today','今日'],['week','本周'],['month','本月'],['all','累计']].map(([key,label]) => {
      const t = data.overview[key];
      return card(label + ' Token', tokens(t.tokens), cost(t) + (t.unpriced ? ` · ${number(t.unpriced)} 条未计价` : ' · USD'));
    }));
    if (!isQuery) $('overview-cards').append(
      card('累计总金额（USD）', cost(data.overview.all), '全部历史 · ' + costNote(data.overview.all))
    );
    if (isQuery) {
    const t = data.total;
    $('range-label').textContent = `${data.start} 至 ${data.end} · ${$('model').selectedOptions[0].textContent}`;
    $('range-cards').replaceChildren(card('总 Token', tokens(t.tokens), `${number(t.events)} 条用量记录`), card('输入 Token',tokens(t.input),`缓存命中 ${tokens(t.cache_read)}`), card('输出 Token',tokens(t.output),'包含推理 Token'), card('金额（USD）',cost(t),costNote(t)));
    history.replaceState(null, '', '/query?' + parameters() + '&group=' + $('group').value);
    }
    page = 0;
    renderChart(); if (isQuery) renderDaily(); else renderRanking();
    $('updated').textContent = '更新于 ' + new Date().toLocaleTimeString('zh-CN');
  } catch (error) {
    if (current !== request || error.name === 'AbortError') return;
    $('error').textContent = error.message + (report ? '（下方保留上次成功查询的结果）' : '');
    $('error').hidden = false;
    $('updated').textContent = '更新失败';
  } finally { if (current === request) $('refresh').disabled = false; }
}
function selectDay(day) {
  if (!isQuery) { location.href = '/query?' + new URLSearchParams({start: day, end: day, group: 'model'}); return; }
  $('start').value = day; $('end').value = day; $('group').value = 'model'; load();
}
function selectModel(model) {
  if (!isQuery) {
    const params = new URLSearchParams({start: report.first_day || report.start, end: report.today});
    if (model == null) params.set('unknown', '1'); else params.set('model', model);
    location.href = '/query?' + params;
    return;
  }
  $('model').value = model == null ? 'unknown' : 'model:' + model;
  $('group').value = 'day'; load();
}
function renderChart() {
  if (!report) return;
  const metric = $('metric').value;
  const max = Math.max(1, ...report.daily.map(row => row[metric] || 0));
  $('chart').replaceChildren();
  for (const row of report.daily) {
    const value = metric === 'tokens' ? tokens(row.tokens) + ' Token' : cost(row);
    const label = `${row.day}：${value}${row.unpriced ? `，${row.unpriced} 条未计价` : ''}`;
    const button = node('button',null,'bar-button');
    button.title = label; button.setAttribute('aria-label',label + '，查看明细');
    const bar = node('span',null,'bar');
    bar.style.height = `${Math.max(2, (row[metric] || 0) / max * 165)}px`;
    button.append(bar, node('span', row.day.slice(5), 'bar-label'));
    button.addEventListener('click', () => selectDay(row.day));
    $('chart').append(button);
  }
}
function table(rows, columns) {
  if (!rows.length) return node('div', '所选范围内暂无记录', 'empty');
  const el = node('table'), head = node('thead'), tr = node('tr'), body = node('tbody');
  for (const label of [...columns.map(c => c.label), '输入', '缓存命中', '输出', '总 Token', '金额（USD）', '记录数']) {
    const th = node('th',label); th.scope = 'col'; tr.append(th);
  }
  head.append(tr);
  for (const row of rows) {
    const tr = node('tr');
    for (const column of columns) {
      const td = node('td'), button = node('button',column.key === 'model' ? modelName(row.model) : row.day);
      button.addEventListener('click', () => column.key === 'model' ? selectModel(row.model) : selectDay(row.day));
      td.append(button); tr.append(td);
    }
    for (const key of ['input','cache_read','output','tokens']) tr.append(node('td',tokens(row[key])));
    const td = node('td',cost(row));
    if (row.unpriced) td.append(node('span', `${number(row.unpriced)} 条未计价`, 'warning'));
    tr.append(td,node('td',number(row.events))); body.append(tr);
  }
  el.append(head,body); return el;
}
function renderRanking() { $('ranking').replaceChildren(table(report.overall_ranking,[{key:'model',label:'模型'}])); }
function renderDaily() {
  if (!report) return;
  const byModel = $('group').value === 'model';
  const rows = [...(byModel ? report.details : report.daily)].sort((a,b) => b.day.localeCompare(a.day) || (a.model || '').localeCompare(b.model || ''));
  const pages = Math.max(1,Math.ceil(rows.length/pageSize));
  page = Math.max(0,Math.min(page,pages-1));
  const columns = [{key:'day',label:'日期'}];
  if (byModel) columns.push({key:'model',label:'模型'});
  $('daily-table').replaceChildren(table(rows.slice(page*pageSize,(page+1)*pageSize),columns));
  $('page-info').textContent = `${page+1} / ${pages} 页 · ${number(rows.length)} 行`;
  $('prev').disabled = page === 0; $('next').disabled = page >= pages-1;
}
function dateShift(date, days) {
  const d = new Date(date + 'T12:00:00Z'); d.setUTCDate(d.getUTCDate() + days); return d.toISOString().slice(0,10);
}
if (isQuery) {
$('query').addEventListener('submit', event => { event.preventDefault(); load(); });
$('group').addEventListener('change',() => {page=0;renderDaily();});
$('prev').addEventListener('click',() => {page--;renderDaily();});
$('next').addEventListener('click',() => {page++;renderDaily();});
for (const button of document.querySelectorAll('[data-range]')) button.addEventListener('click',() => {
  if (!report) return;
  const range = button.dataset.range, today = report.today;
  $('end').value = today;
  $('start').value = range === 'today' ? today : range === 'month' ? today.slice(0,8)+'01' : range === 'all' ? report.first_day || today : dateShift(today,1-Number(range));
  load();
});
for (const id of ['start', 'end']) if (initial.has(id)) $(id).value = initial.get(id);
if (initial.has('model') || initial.has('unknown')) {
  const value = initial.has('unknown') ? 'unknown' : 'model:' + initial.get('model');
  $('model').add(new Option(initial.get('model') || '未识别模型', value));
  $('model').value = value;
}
if (initial.get('group') === 'model') $('group').value = 'model';
}
if (isPlans) {
  $('metric').addEventListener('change',renderLineCharts);
  $('weeks').addEventListener('change',renderLineCharts);
  $('refresh').addEventListener('click',loadPlans);
  loadPlans();
} else {
  $('refresh').addEventListener('click',load);
  $('metric').addEventListener('change',renderChart);
  load();
}

// ---- 套餐额度页 ----------------------------------------------------------
const SVG_NS = 'http://www.w3.org/2000/svg';
const planColor = id => `var(--plan-${id})`;
// 图表轴与提示里的紧凑数值：套餐额度可以小到几千 Token，M/B 会看不见。
const compact = value => {
  const magnitude = Math.abs(value || 0);
  if (magnitude >= 1e9) return (value / 1e9).toFixed(2) + ' B';
  if (magnitude >= 1e6) return (value / 1e6).toFixed(2) + ' M';
  if (magnitude >= 1000) return (value / 1000).toFixed(1) + ' k';
  return number(Math.round(value || 0));
};
const formatMetric = (metric, value) => metric === 'tokens' ? compact(value) + ' Token' : '$' + value.toFixed(2);
const axisLabel = (metric, value) => metric === 'tokens' ? compact(value) : '$' + value.toFixed(2);
const planMoney = value => value == null ? null : '$' + value.toFixed(2);
const planTokens = value => value == null ? null : tokens(value) + ' Token';
function svgNode(tag, attributes, text) {
  const element = document.createElementNS(SVG_NS, tag);
  for (const [key, value] of Object.entries(attributes || {})) if (value != null) element.setAttribute(key, value);
  if (text != null) element.textContent = text;
  return element;
}
let planReport;
async function loadPlans() {
  const current = ++request;
  controller?.abort();
  controller = new AbortController();
  $('refresh').disabled = true;
  $('updated').textContent = '正在更新…';
  try {
    const response = await fetch('/api/plans', {signal: controller.signal});
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || '查询失败');
    if (current !== request) return;
    planReport = data;
    $('error').hidden = true;
    $('context').textContent = [
      data.ledger_found ? '已找到额度比例来源（omp 额度账本 / Grok 账单日志）' : '未找到额度比例来源，比例与推算不可用',
      `时区 ${data.timezone}`,
      `生成于 ${data.generated_at}`,
      'omp2（pro2 profile）的历史会话不回填，见下方说明',
    ].join(' · ');
    renderPlanCards();
    renderAccounts();
    renderNotes();
    renderLineCharts();
    $('updated').textContent = '更新于 ' + new Date().toLocaleTimeString('zh-CN');
  } catch (error) {
    if (current !== request || error.name === 'AbortError') return;
    $('error').textContent = error.message + (planReport ? '（下方保留上次成功查询的结果）' : '');
    $('error').hidden = false;
    $('updated').textContent = '更新失败';
  } finally { if (current === request) $('refresh').disabled = false; }
}
function renderPlanCards() {
  const cards = planReport.plans.map(plan => {
    const el = node('div', null, 'card plan-card');
    const label = node('span', null, 'label');
    label.append(plan.title, node('span', plan.open ? '进行中' : '已结束', 'tag'));
    el.append(label, node('strong', plan.used_percent == null ? '—' : `${plan.used_percent.toFixed(1)}%`));
    const detail = [];
    if (plan.account) detail.push(`账户 ${plan.account}`);
    if (plan.used_percent == null) {
      detail.push(plan.account_resolved ? '该周期没有比例样本' : '账户归属未配置，见下方账户映射');
    } else {
      detail.push(`本周期已用 ${planMoney(plan.used_cost_usd) || '—'} · ${planTokens(plan.used_tokens) || '—'}`);
    }
    detail.push(plan.max_cost_usd == null
      ? '数据不足，无法推算每周上限'
      : `每周上限 ${planMoney(plan.max_cost_usd)} · ${planTokens(plan.max_tokens)}`);
    if (plan.period) {
      const days = Math.max(0, Math.ceil((plan.period.resets_at_ms - Date.now()) / 86400000));
      detail.push(`周期 ${plan.period.start} → ${plan.period.end} · 重置 ${plan.period.end} · 剩余 ${days} 天`);
    }
    if (plan.sample_at) detail.push(`最近样本 ${plan.sample_at}`);
    if (plan.group_percent != null) {
      // 两份订阅的比例各自相对自己的额度，所以这里写明「合计」与两人各自的占比，
      // 避免把 Σf 误读成单池占用率。
      const siblings = planReport.plans.filter(other => other.group === plan.group && other.used_percent != null);
      // 单份订阅的组（Codex、SuperGrok）里 Σf 就等于自己那一个比例，无需复述。
      if (siblings.length > 1) {
        const shares = siblings.map(other => `${other.title} ${other.used_percent.toFixed(1)}%`).join(' · ');
        detail.push(`两份订阅合计消耗 ${plan.group_percent.toFixed(1)}%（${shares}，各自占自己额度） · 上限共享`);
      }
    }
    const point = plan.weekly[plan.weekly.length - 1];
    if (point && point.missing > 0) detail.push(`该周期前 ${(point.missing * 100).toFixed(1)}% 无用量数据`);
    for (const line of detail) el.append(node('small', line));
    return el;
  });
  $('plan-cards').replaceChildren(...cards);
}
function renderAccounts() {
  const titles = Object.fromEntries(planReport.plans.map(plan => [plan.id, plan.title]));
  $('accounts').replaceChildren(...planReport.accounts.map(entry => {
    const el = node('div', null, 'account-item');
    el.append(
      node('strong', `${titles[entry.plan] || entry.plan} · ${entry.resolved ? entry.label : '未配置'}`),
      node('small', entry.used_percent == null ? '暂无本周期比例' : `当前周已用 ${entry.used_percent.toFixed(1)}%`)
    );
    return el;
  }));
}
function renderNotes() {
  $('notes').replaceChildren(...(planReport.notes || []).map(note => node('li', note)));
}
function renderLineCharts() {
  if (!planReport) return;
  const metric = $('metric').value;
  const weeks = $('weeks').value;
  const series = planReport.plans.map(plan => {
    const points = weeks === 'all' ? plan.weekly : plan.weekly.slice(-Number(weeks));
    return {plan, points};
  });
  drawLineChart($('used-chart'), series, metric, 'used');
  drawLineChart($('max-chart'), series, metric, 'max');
}
function drawLineChart(host, series, metric, kind) {
  host.replaceChildren();
  const key = (kind === 'used' ? 'used_' : 'max_') + metric;
  const width = 720, height = 260, left = 58, right = 14, top = 14, bottom = 30;
  const innerWidth = width - left - right, innerHeight = height - top - bottom;
  const values = [], times = [];
  for (const {points} of series) for (const point of points) {
    const value = point[key];
    if (value != null && isFinite(value)) values.push(value);
    const time = Date.parse(point.start);
    if (!isNaN(time)) times.push(time);
  }
  const yMax = values.length ? Math.max(...values) * 1.1 : 1;
  const tMin = times.length ? Math.min(...times) : 0;
  const tMax = times.length ? Math.max(...times) : 1;
  const svg = svgNode('svg', {viewBox: `0 0 ${width} ${height}`, class: 'line-chart', role: 'img'});
  const x = time => times.length && tMax > tMin ? left + (time - tMin) / (tMax - tMin) * innerWidth : left + innerWidth / 2;
  const y = value => top + innerHeight - Math.min(1, Math.max(0, value / yMax)) * innerHeight;
  for (let tick = 0; tick <= 4; tick++) {
    const gy = top + innerHeight * (1 - tick / 4);
    svg.append(svgNode('line', {x1: left, y1: gy, x2: left + innerWidth, y2: gy, class: 'chart-grid'}));
    svg.append(svgNode('text', {x: left - 8, y: gy + 4, class: 'chart-axis', 'text-anchor': 'end'}, axisLabel(metric, yMax * tick / 4)));
  }
  if (times.length) {
    const ticks = tMax > tMin ? [0, 1, 2, 3].map(index => tMin + (tMax - tMin) * index / 3) : [tMin];
    for (const time of ticks) svg.append(svgNode('text', {x: x(time), y: height - 8, class: 'chart-axis', 'text-anchor': 'middle'}, new Date(time).toISOString().slice(5, 10)));
  }
  for (const {plan, points} of series) {
    // agy2 与 agy 的上限逐点相同，用虚线让两条线都看得见。
    const dashed = kind === 'max' && plan.id === 'agy2';
    let run = [];
    const flush = () => {
      if (run.length > 1) svg.append(svgNode('polyline', {points: run.join(' '), fill: 'none', stroke: planColor(plan.id), 'stroke-width': 2, 'stroke-dasharray': dashed ? '5 4' : null}));
      run = [];
    };
    for (const point of points) {
      const value = point[key];
      if (value == null || !isFinite(value)) { flush(); continue; }
      const px = x(Date.parse(point.start)), py = y(value);
      run.push(`${px},${py}`);
      const circle = svgNode('circle', {cx: px, cy: py, r: 3.5, fill: point.open ? 'none' : planColor(plan.id), stroke: planColor(plan.id), 'stroke-width': 1.5});
      circle.append(svgNode('title', null, `${plan.title} · ${point.start} → ${point.end} · ${formatMetric(metric, value)} · 样本 ${point.sample_at} · 缺口 ${(point.missing * 100).toFixed(0)}%`));
      svg.append(circle);
    }
    flush();
  }
  const legend = node('div', null, 'legend');
  for (const {plan, points} of series) {
    const last = [...points].reverse().find(point => point[key] != null);
    const item = node('span', null, 'legend-item');
    item.append(node('i', null, `swatch plan-${plan.id}`), node('b', plan.title), node('small', last ? formatMetric(metric, last[key]) : '—'));
    legend.append(item);
  }
  host.append(svg, legend);
}
