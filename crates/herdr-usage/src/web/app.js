'use strict';
const $ = id => document.getElementById(id);
const number = value => new Intl.NumberFormat('zh-CN').format(value || 0);
const isQuery = location.pathname === '/query';
const isPlans = location.pathname === '/plans';
const isRequests = location.pathname === '/requests';
const isHourly = location.pathname === '/hourly';
const isCloudflare = location.pathname === '/cloudflare';
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
let hourlyReport = null, hourlyRequest = 0, hourlyController = null;
let cfReport = null, cfRequest = 0, cfController = null;
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
if (isRequests) {
  $('refresh').addEventListener('click', loadRequests);
  let searchTimer;
  $('request-filters').addEventListener('submit', event => { event.preventDefault(); clearTimeout(searchTimer); requestPage = 0; renderRequests(); });
  $('request-session').addEventListener('input', () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => { requestPage = 0; renderRequests(); }, 200);
  });
  for (const id of ['request-client', 'request-provider', 'request-model', 'request-account']) {
    $(id).addEventListener('change', () => { requestPage = 0; renderRequests(); });
  }
  $('request-clear').addEventListener('click', () => {
    for (const id of ['request-session', 'request-client', 'request-provider', 'request-model', 'request-account']) $(id).value = '';
    clearTimeout(searchTimer);
    requestPage = 0; renderRequests();
  });
  $('request-prev').addEventListener('click', () => { requestPage--; renderRequests(); });
  $('request-next').addEventListener('click', () => { requestPage++; renderRequests(); });
  loadRequests();
} else if (isPlans) {
  $('metric').addEventListener('change',renderLineCharts);
  $('weeks').addEventListener('change',renderLineCharts);
  $('refresh').addEventListener('click',loadPlans);
  loadPlans();
} else if (isHourly) {
  $('hourly-month').addEventListener('change', () => loadHourly($('hourly-month').value));
  $('hourly-metric').addEventListener('change', renderHourlyContent);
  $('refresh').addEventListener('click', () => loadHourly($('hourly-month').value));
  loadHourly(initial.get('month') || '');
} else if (isCloudflare) {
  $('refresh').addEventListener('click', () => loadCloudflare(true));
  $('cf-metric').addEventListener('change', renderCloudflareChart);
  loadCloudflare(false);
} else {
  $('refresh').addEventListener('click',load);
  $('metric').addEventListener('change',renderChart);
  load();
}

// ---- Request / account tracking ------------------------------------------
const duration = value => value == null ? '未记录' : value < 1000 ? `${value} ms` : `${(value / 1000).toFixed(2)} s`;
const unknownFilter = '__unknown__';
let requestReport;
let requestPage = 0;
const requestPageSize = 100;
function simpleTable(columns, rows) {
  if (!rows.length) return node('div', '暂无记录', 'empty');
  const table = node('table'), head = node('thead'), hr = node('tr'), body = node('tbody');
  for (const column of columns) { const th = node('th', column.label); th.scope = 'col'; hr.append(th); }
  head.append(hr);
  for (const row of rows) {
    const tr = node('tr');
    for (const column of columns) tr.append(node('td', column.value(row)));
    body.append(tr);
  }
  table.append(head, body);
  return table;
}
function requestField(row, key) {
  const value = row[key];
  return value == null || value === '' ? unknownFilter : value;
}
function requestFilterOptions(id, allLabel, rows, key, unknownLabel) {
  const select = $(id), selected = select.value;
  const values = [...new Set(rows.map(row => requestField(row, key)))].sort((a, b) => {
    if (a === unknownFilter) return 1;
    if (b === unknownFilter) return -1;
    return a.localeCompare(b, 'zh-CN');
  });
  select.replaceChildren(new Option(allLabel, ''));
  for (const value of values) select.add(new Option(value === unknownFilter ? unknownLabel : value, value));
  if ([...select.options].some(option => option.value === selected)) select.value = selected;
}
function renderRequests() {
  if (!requestReport) return;
  const session = $('request-session').value.trim().toLocaleLowerCase();
  const filters = [
    ['client', $('request-client').value],
    ['provider', $('request-provider').value],
    ['model', $('request-model').value],
    ['account', $('request-account').value],
  ];
  const rows = requestReport.recent.filter(row =>
    (!session || row.session_id.toLocaleLowerCase().includes(session)) &&
    filters.every(([key, value]) => !value || requestField(row, key) === value)
  );
  const pages = Math.max(1, Math.ceil(rows.length / requestPageSize));
  requestPage = Math.max(0, Math.min(requestPage, pages - 1));
  $('request-filter-summary').textContent = `显示 ${number(rows.length)} / ${number(requestReport.recent.length)} 条 · 最近窗口最多 ${number(requestReport.recent_limit || 3000)} 条，SQLite 保存全部历史`;
  $('request-page-info').textContent = `${requestPage + 1} / ${pages} 页`;
  $('request-prev').disabled = requestPage === 0;
  $('request-next').disabled = requestPage >= pages - 1;
  $('request-table').replaceChildren(simpleTable([
    {label:'开始时间', value:r => r.local_time},
    {label:'耗时', value:r => duration(r.duration_ms)},
    {label:'客户端', value:r => r.client},
    {label:'通道', value:r => r.provider},
    {label:'模型', value:r => modelName(r.model)},
    {label:'账户', value:r => r.account || '未解析'},
    {label:'Session', value:r => r.session_id},
  ], rows.slice(requestPage * requestPageSize, (requestPage + 1) * requestPageSize)));
}
async function loadRequests() {
  const current = ++request;
  controller?.abort(); controller = new AbortController();
  $('refresh').disabled = true; $('updated').textContent = '正在更新…';
  try {
    const response = await fetch('/api/requests', {signal: controller.signal});
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || '查询失败');
    if (current !== request) return;
    requestReport = data;
    requestPage = 0;
    $('error').hidden = true;
    $('context').textContent = `按 request 统计 · ${data.timezone} · OMP / Codex / Grok / OpenCode`;
    const rate = data.requests ? data.resolved / data.requests * 100 : 0;
    $('request-cards').replaceChildren(
      card('Request 总数', number(data.requests), '每次 assistant 请求一条'),
      card('账户已解析', number(data.resolved), `${rate.toFixed(1)}%`),
      card('平均耗时', duration(data.average_duration_ms), '发出到完成'),
      card('跨账户 Session', number(data.cross_account_sessions.length), '同 session + provider'),
    );
    $('cross-sessions').replaceChildren(simpleTable([
      {label:'客户端', value:r => r.client},
      {label:'Session', value:r => r.session_id},
      {label:'通道', value:r => r.provider},
      {label:'账户', value:r => r.accounts.join(' → ')},
      {label:'Request 数', value:r => number(r.requests)},
      {label:'最后时间', value:r => new Date(r.last_at).toLocaleString('zh-CN')},
    ], data.cross_account_sessions));
    requestFilterOptions('request-client', '全部客户端', data.recent, 'client', '未识别客户端');
    requestFilterOptions('request-provider', '全部通道', data.recent, 'provider', '未识别通道');
    requestFilterOptions('request-model', '全部模型', data.recent, 'model', '未识别模型');
    requestFilterOptions('request-account', '全部账户', data.recent, 'account', '未解析账户');
    renderRequests();
    $('updated').textContent = '更新于 ' + new Date().toLocaleTimeString('zh-CN');
  } catch (error) {
    if (current !== request || error.name === 'AbortError') return;
    $('error').textContent = error.message; $('error').hidden = false; $('updated').textContent = '更新失败';
  } finally { if (current === request) $('refresh').disabled = false; }
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

// ---- Hourly token usage & monthly aggregation ------------------------------


async function loadHourly(month) {
  const current = ++hourlyRequest;
  hourlyController?.abort();
  hourlyController = new AbortController();
  $('refresh').disabled = true;
  $('updated').textContent = '正在更新…';
  try {
    const url = '/api/hourly' + (month ? '?month=' + encodeURIComponent(month) : '');
    const response = await fetch(url, {signal: hourlyController.signal});
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || '查询失败');
    if (current !== hourlyRequest) return;
    hourlyReport = data;
    $('error').hidden = true;

    const monthSelect = $('hourly-month');
    const existingMonths = [...monthSelect.options].map(o => o.value);
    if (JSON.stringify(existingMonths) !== JSON.stringify(data.available_months)) {
      monthSelect.replaceChildren();
      for (const m of data.available_months) {
        monthSelect.add(new Option(m, m));
      }
    }
    monthSelect.value = data.month;

    const currentParams = new URLSearchParams(location.search);
    if (data.month) {
      currentParams.set('month', data.month);
    }
    history.replaceState(null, '', '/hourly?' + currentParams.toString());

    $('context').textContent = `按服务器本地时间统计 · 归档粒度：已闭合整点（不含进行中当前小时）`;
    renderHourlyContent();
    $('updated').textContent = '更新于 ' + new Date().toLocaleTimeString('zh-CN');
  } catch (error) {
    if (current !== hourlyRequest || error.name === 'AbortError') return;
    $('error').textContent = error.message + (hourlyReport ? '（下方保留上次成功查询的结果）' : '');
    $('error').hidden = false;
    $('updated').textContent = '更新失败';
  } finally {
    if (current === hourlyRequest) $('refresh').disabled = false;
  }
}

function renderHourlyContent() {
  if (!hourlyReport) return;
  renderHourlySummary(hourlyReport);
  renderDailyChartsList(hourlyReport);
}

function renderHourlySummary(report) {
  const s = report.summary;
  const totalEvents = report.days.reduce((acc, d) => acc + d.hours.reduce((ha, h) => ha + h.events, 0), 0);
  const peakDesc = s.peak_tokens > 0
    ? `${tokens(s.peak_tokens)} Token · 峰值小时`
    : '当月暂无用量';
  const peakVal = s.peak_tokens > 0
    ? `${s.peak_day} ${String(s.peak_hour).padStart(2, '0')}:00`
    : '—';

  $('hourly-cards').replaceChildren(
    card('当月总 Token', tokens(s.total_tokens), `${number(totalEvents)} 条用量记录`),
    card('当月预估金额', money(s.total_cost), '已记录费用 · USD'),
    card('月度峰值时段', peakVal, peakDesc),
    card('活跃天数', `${s.active_days} 天`, `共 ${report.days.length} 个自然日`)
  );
}

function renderDailyChartsList(report) {
  const container = $('daily-charts');
  container.replaceChildren();
  if (!report.days.length) {
    container.append(node('div', '所选月份暂无用量记录', 'empty'));
    return;
  }

  const metric = $('hourly-metric').value; // 'tokens' or 'cost'
  const maxVal = Math.max(1, ...report.days.flatMap(d => d.hours.map(h => metric === 'cost' ? (h.cost_usd || 0) : h.tokens)));

  for (const day of report.days) {
    const dayCard = node('div', null, 'day-chart-card');
    const header = node('div', null, 'day-chart-header');
    const title = node('div', null, 'day-chart-title');
    title.append(
      node('strong', day.day),
      node('span', day.weekday, 'weekday')
    );

    const meta = node('div', null, 'day-chart-meta');
    const dayCostStr = day.total_cost != null ? money(day.total_cost) : '$0.00';
    meta.append(
      node('span', 'Token: '),
      node('b', tokens(day.total_tokens)),
      node('span', '费用: '),
      node('b', dayCostStr)
    );

    header.append(title, meta);
    dayCard.append(header);

    const barsContainer = node('div', null, 'hour-bars-container');
    for (const h of day.hours) {
      const col = node('div', null, 'hour-col');
      const track = node('div', null, 'hour-bar-track');
      const bar = node('div', null, 'hour-bar');

      const val = metric === 'cost' ? (h.cost_usd || 0) : h.tokens;
      const isZero = val <= 0;

      if (isZero) {
        bar.classList.add('zero');
      } else {
        const heightPercent = Math.max(4, Math.min(100, Math.round((val / maxVal) * 100)));
        bar.style.height = `${heightPercent}%`;
      }

      if (h.closed) {
        bar.classList.add('closed');
      } else {
        bar.classList.add('in-progress');
      }

      track.append(bar);

      const tooltip = node('div', null, 'hour-tooltip');
      const hourLabel = `${String(h.hour).padStart(2, '0')}:00 - ${String(h.hour).padStart(2, '0')}:59`;
      const statusStr = h.closed ? '已闭合归档' : '进行中 / 未结束';
      tooltip.innerHTML = `
        <div style="font-weight:700;margin-bottom:4px">${day.day} ${hourLabel} <span style="opacity:0.75;font-size:10px">(${statusStr})</span></div>
        <div>总 Token: <b>${tokens(h.tokens)}</b></div>
        <div>输入: ${tokens(h.input)} (缓存命中 ${tokens(h.cache_read)})</div>
        <div>输出: ${tokens(h.output)}</div>
        <div>费用: <b>${money(h.cost_usd)}</b></div>
        <div>请求数: <b>${number(h.events)}</b></div>
      `;
      col.append(track, tooltip);

      const tick = node('span', String(h.hour).padStart(2, '0'), 'hour-tick');
      col.append(tick);

      barsContainer.append(col);
    }

    dayCard.append(barsContainer);
    container.append(dayCard);
  }
}

// ---- Cloudflare stats ----------------------------------------------------
async function loadCloudflare(force = false) {
  const current = ++cfRequest;
  cfController?.abort();
  cfController = new AbortController();
  $('refresh').disabled = true;
  $('updated').textContent = force ? '正在远程拉取…' : '正在加载…';
  $('error').hidden = true;

  try {
    const url = '/api/cloudflare' + (force ? '?refresh=1' : '');
    const response = await fetch(url, { signal: cfController.signal });
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || '获取 Cloudflare 数据失败');
    if (current !== cfRequest) return;
    cfReport = data;
    renderCloudflare(data);
  } catch (err) {
    if (err.name === 'AbortError') return;
    $('error').hidden = false;
    $('error').textContent = err.message;
    $('updated').textContent = '加载失败';
  } finally {
    if (current === cfRequest) $('refresh').disabled = false;
  }
}

function renderMetricCard(item, customFooter) {
  const card = node('div', null, 'cf-metric-card');

  // Header: Name + Badge
  const head = node('div', null, 'cf-metric-head');
  head.append(node('span', item.name, 'cf-metric-name'));
  const badge = node('span', item.percent > 0 ? `${item.percent.toFixed(item.percent < 0.1 && item.percent > 0 ? 2 : 1)}%` : (item.quota > 0 ? '0%' : '正常'), `cf-badge ${item.status || 'normal'}`);
  head.append(badge);
  card.append(head);

  // Main Value
  const main = node('div', null, 'cf-metric-main');
  main.append(
    node('span', item.formatted_value, 'main-val'),
    node('span', item.unit || '', 'main-unit')
  );
  card.append(main);

  // Progress Bar
  if (item.quota > 0) {
    const meterWrap = node('div', null, 'cf-meter-wrap');
    const meter = node('div', null, 'cf-meter');
    const fill = node('div', null, `cf-meter-fill ${item.status || 'normal'}`);
    fill.style.width = `${Math.min(100, Math.max(0, item.percent))}%`;
    meter.append(fill);
    meterWrap.append(meter);
    card.append(meterWrap);
  }

  // Details Footer: 3 Essential Data Points (当前已用, Paid月配额, 周期剩余)
  const details = node('div', null, 'cf-metric-details');
  if (customFooter) {
    for (const f of customFooter) {
      const col = node('div', null, 'cf-detail-col');
      col.append(
        node('span', f.label, 'cf-detail-label'),
        node('span', f.value, 'cf-detail-val')
      );
      details.append(col);
    }
  } else {
    const col1 = node('div', null, 'cf-detail-col');
    col1.append(
      node('span', '当前已用量', 'cf-detail-label'),
      node('span', `${item.formatted_value} ${item.unit}`, 'cf-detail-val')
    );
    const col2 = node('div', null, 'cf-detail-col');
    col2.append(
      node('span', 'Paid月度配额', 'cf-detail-label'),
      node('span', `${item.formatted_quota} ${item.unit}`, 'cf-detail-val')
    );
    const col3 = node('div', null, 'cf-detail-col');
    col3.append(
      node('span', '周期剩余额度', 'cf-detail-label'),
      node('span', `${item.formatted_remaining} ${item.unit}`, 'cf-detail-val')
    );
    details.append(col1, col2, col3);
  }
  card.append(details);

  return card;
}

function renderCloudflare(data) {
  if (!data.configured) {
    $('status-dot').className = 'dot';
    $('sync-status').textContent = '未配置';
    $('updated').textContent = '等待配置';
    $('unconfigured-guide').hidden = false;
    $('cf-content').hidden = true;
    return;
  }

  $('unconfigured-guide').hidden = true;
  $('cf-content').hidden = false;

  // Status and Stale banner
  $('status-dot').className = data.stale ? 'dot warn' : 'dot';
  $('sync-status').textContent = data.stale ? '缓存数据（网络重试中）' : '本地缓存';
  if (data.synced_at) {
    const syncDate = new Date(data.synced_at);
    $('updated').textContent = `更新于 ${syncDate.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', second: '2-digit' })}`;
  }
  if (data.stale) {
    $('stale-banner').hidden = false;
    $('stale-banner').textContent = `数据远程拉取遇到异常：${data.error || '网络通信故障'}；已自动展示本地最近快照。`;
  } else {
    $('stale-banner').hidden = true;
  }

  // Top Billing Cycle Hero
  const cycle = data.billing_cycle;
  const isFree = (data.plan || '').toLowerCase() === 'free';
  $('cf-plan-badge').textContent = isFree ? 'Workers Free Plan' : 'Workers Paid Plan ($5/mo)';
  $('cf-cycle-type').textContent = cycle.cycle_type || (cycle.is_fallback ? '自然月周期' : '套餐订阅周期');
  $('cf-days-remaining').textContent = cycle.days_remaining;
  $('cf-cycle-range').textContent = `${cycle.start_date} 至 ${cycle.end_date}`;
  $('cf-cycle-progress-text').textContent = `已过 ${cycle.elapsed_days} 天 / 共 ${cycle.total_days} 天 (${cycle.elapsed_percent.toFixed(1)}%)`;
  $('cf-cycle-progress-bar').style.width = `${cycle.elapsed_percent.toFixed(1)}%`;

  const curr = data.current_period;

  // 1. Workers Grid
  const workersSubhead = $('cf-workers-subhead');
  if (workersSubhead) {
    workersSubhead.textContent = isFree
      ? '含 10 万次/日请求、10 毫秒/请求 CPU 耗时与 100 个 Worker 部署配额'
      : '含 1000 万次请求、3000 万毫秒（30,000 秒）CPU 耗时与 500 个 Worker 部署配额';
  }

  const scriptsItem = curr.workers_scripts || {
    name: 'Workers 部署服务数',
    value: 0,
    quota: isFree ? 100 : 500,
    remaining: isFree ? 100 : 500,
    percent: 0,
    formatted_value: '0',
    formatted_quota: isFree ? '100' : '500',
    formatted_remaining: isFree ? '100' : '500',
    unit: '个',
    status: 'normal'
  };

  $('cf-workers-cards').replaceChildren(
    renderMetricCard(curr.workers_requests),
    renderMetricCard(curr.workers_cpu_time),
    renderMetricCard(scriptsItem, [
      { label: '已部署服务', value: `${scriptsItem.formatted_value} 个` },
      { label: isFree ? 'Free 部署配额' : 'Paid 部署配额', value: `${scriptsItem.formatted_quota} 个` },
      { label: '剩余可用额度', value: `${scriptsItem.formatted_remaining} 个` },
      { label: '构建分钟数配额', value: `${number(curr.workers_build_minutes_limit || (isFree ? 3000 : 6000))} 分钟/月` },
      { label: '并发构建数', value: `${curr.workers_concurrent_builds_limit || (isFree ? 1 : 6)} 个` },
      { label: '单 Worker 体积上限', value: '64 MiB' }
    ]),
    renderMetricCard(curr.workers_errors, [
      { label: '错误次数', value: `${curr.workers_errors.formatted_value} 次` },
      { label: '错误率占比', value: `${curr.workers_errors.percent.toFixed(2)}%` },
      { label: '健康率', value: curr.workers_errors.formatted_remaining }
    ])
  );

  // 2. D1 Grid
  const d1QueryItem = {
    name: 'D1 SQL 查询操作数',
    value: (curr.d1_read_queries || 0) + (curr.d1_write_queries || 0),
    quota: 0,
    remaining: 0,
    percent: 0,
    formatted_value: number((curr.d1_read_queries || 0) + (curr.d1_write_queries || 0)),
    unit: '次',
    status: 'normal'
  };
  $('cf-d1-cards').replaceChildren(
    renderMetricCard(curr.d1_rows_read),
    renderMetricCard(curr.d1_rows_written),
    renderMetricCard(curr.d1_storage, [
      { label: '已存容量', value: curr.d1_storage.formatted_value },
      { label: 'Paid包含配额', value: curr.d1_storage.formatted_quota },
      { label: '数据库总数', value: `${curr.d1_databases_count || 0} 个 DB` }
    ]),
    renderMetricCard(d1QueryItem, [
      { label: '总查询次数', value: `${number((curr.d1_read_queries || 0) + (curr.d1_write_queries || 0))} 次` },
      { label: '读查询数', value: `${number(curr.d1_read_queries || 0)} 次` },
      { label: '写查询数', value: `${number(curr.d1_write_queries || 0)} 次` }
    ])
  );

  // 3. R2 Grid
  $('cf-r2-cards').replaceChildren(
    renderMetricCard(curr.r2_class_a_operations),
    renderMetricCard(curr.r2_class_b_operations),
    renderMetricCard(curr.r2_storage, [
      { label: '已存容量', value: curr.r2_storage.formatted_value },
      { label: '包含存储额度', value: curr.r2_storage.formatted_quota },
      { label: '桶与对象数', value: `${curr.r2_buckets_count || 0} 桶 / ${curr.r2_objects_count || 0} 实体` }
    ])
  );

  // 4. KV Grid
  $('cf-kv-cards').replaceChildren(
    renderMetricCard(curr.kv_read_operations),
    renderMetricCard(curr.kv_write_operations),
    renderMetricCard(curr.kv_storage, [
      { label: '已存体积', value: curr.kv_storage.formatted_value },
      { label: '包含存储额度', value: curr.kv_storage.formatted_quota },
      { label: '命名空间数', value: `${curr.kv_namespaces_count || 0} 个` }
    ])
  );

  // 5. Observability & Limits Grid
  const subreqItem = {
    name: '单请求子请求安全限额',
    value: 50,
    quota: 50,
    percent: 0,
    formatted_value: '50',
    unit: '次 / 请求',
    status: 'normal'
  };
  const tailItem = {
    name: '实时日志追踪与外推限额',
    value: 2,
    quota: 2,
    percent: 0,
    formatted_value: '2 会话 / 4 作业',
    unit: '',
    status: 'normal'
  };
  $('cf-observability-cards').replaceChildren(
    renderMetricCard(curr.observability_events),
    renderMetricCard(curr.analytics_engine_points),
    renderMetricCard(subreqItem, [
      { label: '单请求硬上限', value: '50 次' },
      { label: '健康状态', value: '正常' },
      { label: '超限影响', value: '触发 1101 错误' }
    ]),
    renderMetricCard(tailItem, [
      { label: '活跃 Tail 上限', value: '最多 2 个' },
      { label: 'Logpush 任务', value: '最多 4 个' },
      { label: 'Cron 调度上限', value: '每脚本 5 个' }
    ])
  );
  // Chart & Monthly Table
  renderCloudflareChart();
  renderCloudflareMonthly(data.monthly_history || []);
}

function renderCloudflareChart() {
  const container = $('cf-chart');
  container.replaceChildren();

  if (!cfReport || !cfReport.daily_trend || cfReport.daily_trend.length === 0) {
    container.append(node('div', '近 30 天暂无每日记录（将在周期数据同步后按日归档）', 'empty'));
    return;
  }

  const metric = $('cf-metric').value;
  const days = cfReport.daily_trend;
  const maxVal = Math.max(1, ...days.map(d => d[metric] || 0));

  const metricNames = {
    workers_requests: '请求数',
    workers_cpu_time_us: 'CPU 耗时',
    d1_rows_read: 'D1 读行',
    d1_rows_written: 'D1 写行',
    r2_operations: 'R2 操作'
  };
  const metricLabel = metricNames[metric] || '用量';

  const chartWrap = node('div', null, 'cf-bars-container');

  for (const day of days) {
    const val = day[metric] || 0;
    const col = node('div', null, 'cf-col');
    const track = node('div', null, 'cf-bar-track');
    const bar = node('div', null, 'cf-bar');

    if (val <= 0) {
      bar.classList.add('zero');
    } else {
      const heightPercent = Math.max(4, Math.min(100, Math.round((val / maxVal) * 100)));
      bar.style.height = `${heightPercent}%`;
    }
    track.append(bar);

    const valFormatted = metric === 'workers_cpu_time_us' ?
      (val >= 1e6 ? `${(val / 1e6).toFixed(2)}s` : `${(val / 1e3).toFixed(1)}ms`) :
      number(val);

    const tooltip = node('div', null, 'cf-tooltip');
    tooltip.innerHTML = `<strong>${day.date}</strong><br>${metricLabel}: ${valFormatted}`;
    col.append(track, tooltip);

    const tick = node('span', day.date.slice(5), 'cf-tick');
    col.append(tick);

    chartWrap.append(col);
  }

  container.append(chartWrap);
}

function renderCloudflareMonthly(history) {
  const container = $('monthly-history');
  container.replaceChildren();

  if (!history || history.length === 0) {
    container.append(node('div', '暂无历史月份数据', 'empty'));
    return;
  }

  const columns = [
    { key: 'month', label: '月份' },
    { key: 'workers_requests', label: 'Workers 请求数' },
    { key: 'workers_cpu_time_us', label: 'Workers CPU 耗时' },
    { key: 'workers_errors', label: 'Workers 错误数' },
    { key: 'd1_rows_read', label: 'D1 读行数' },
    { key: 'd1_rows_written', label: 'D1 写行数' },
    { key: 'r2_operations', label: 'R2 操作数' }
  ];

  const table = node('table');
  const thead = node('thead');
  const trHead = node('tr');
  for (const col of columns) {
    trHead.append(node('th', col.label));
  }
  thead.append(trHead);
  table.append(thead);

  const tbody = node('tbody');
  for (const row of history) {
    const tr = node('tr');
    tr.append(node('td', row.month));
    tr.append(node('td', number(row.workers_requests)));
    tr.append(node('td', row.workers_cpu_time_us >= 1e6 ? `${(row.workers_cpu_time_us / 1e6).toFixed(1)}s` : `${(row.workers_cpu_time_us / 1e3).toFixed(0)}ms`));
    tr.append(node('td', number(row.workers_errors)));
    tr.append(node('td', number(row.d1_rows_read)));
    tr.append(node('td', number(row.d1_rows_written)));
    tr.append(node('td', number(row.r2_operations)));
    tbody.append(tr);
  }
  table.append(tbody);

  container.append(table);
}
