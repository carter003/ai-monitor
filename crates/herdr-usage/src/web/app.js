'use strict';
const $ = id => document.getElementById(id);
const number = value => new Intl.NumberFormat('zh-CN').format(value || 0);
const isQuery = location.pathname === '/query';
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
$('refresh').addEventListener('click',load);
$('metric').addEventListener('change',renderChart);
load();
