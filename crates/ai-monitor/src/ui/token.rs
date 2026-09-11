//! Local usage presentation only. SQL, pricing, cache accounting and provider
//! polling stay in their existing owners. All geometry is in terminal cells.

use super::{CYAN, GREEN, INK, MUTED, TRACK, View, columns, compact, truncate};
use crate::model::{ModelUsage, UsageStats, UsageTotal};
use chrono::{Datelike, Timelike};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

#[cfg(test)]
mod tests;

pub(super) const MIN_CHART_HEIGHT: u16 = 5;
pub(super) const MIN_CHART_WIDTH: u16 = 16;
const NAME_MIN: usize = 10;
const GAP: usize = 2;
const BAR_LOW: Color = Color::Rgb(144, 202, 249);
const BAR_MID: Color = Color::Rgb(33, 150, 243);
const BAR_HIGH: Color = Color::Rgb(13, 71, 161);
const BAR_RED: Color = Color::Rgb(163, 22, 22);

pub(super) fn lines(usage: &UsageStats, width: u16, height: usize) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return vec![];
    }
    let summary = summary_lines(usage, width as usize);
    for limit in (0..=usage.models.len().min(6)).rev() {
        let mut result = summary.clone();
        if limit > 0 {
            result.push(Line::raw(""));
            result.push(Line::styled(
                truncate(" 模型用量 · 最近24小时", width as usize),
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            ));
            result.extend(model_table(&usage.models, width as usize, limit));
        } else if usage.models.is_empty() {
            result.push(Line::styled(
                truncate(" 暂无用量记录", width as usize),
                Style::default().fg(MUTED),
            ));
        }
        if result.len() <= height {
            return result;
        }
    }
    // At very small heights retain the historical figure rather than clipping
    // it behind a decorative frame. The existing page scrollbar can reveal
    // any remaining lines when even these three rows cannot fit.
    let total = &usage.all_total;
    [
        " 历史累计".to_owned(),
        format!(" {} tokens", compact(total.tokens)),
        format!(" {}", money(total.cost)),
    ]
    .into_iter()
    .map(|text| {
        Line::styled(
            truncate(&text, width as usize),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        )
    })
    .collect()
}

/// Four peers, without an oversized double-bordered historical-total card.
/// Choose four, two or one columns from the *token pane's* measured width.
fn summary_lines(usage: &UsageStats, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![];
    }
    let totals = [
        ("当日", &usage.day_total),
        ("本周", &usage.week_total),
        ("本月", &usage.month_total),
        ("历史累计", &usage.all_total),
    ];
    let needed = totals
        .iter()
        .map(|(label, total)| {
            columns(label).max(columns(&compact(total.tokens)) + GAP + columns(&money(total.cost)))
        })
        .max()
        .unwrap_or(1);
    let count = [4usize, 2, 1]
        .into_iter()
        .find(|count| needed * count + GAP * (count - 1) <= width)
        .unwrap_or(1);
    let cell_width = (width - GAP * (count - 1)) / count;
    let mut result = vec![];
    for group in totals.chunks(count) {
        if !result.is_empty() {
            result.push(Line::raw(""));
        }
        let cards: Vec<_> = group
            .iter()
            .map(|(label, total)| summary_card(label, total, cell_width))
            .collect();
        let rows = cards.iter().map(Vec::len).max().unwrap_or(0);
        for row in 0..rows {
            let mut spans = vec![];
            for (column, card) in cards.iter().enumerate() {
                let line = card.get(row).cloned().unwrap_or_default();
                let used = line.width();
                spans.extend(line.spans);
                if column + 1 < cards.len() {
                    spans.push(Span::raw(" ".repeat(cell_width.saturating_sub(used) + GAP)));
                }
            }
            result.push(Line::from(spans));
        }
    }
    result
}

fn summary_card(label: &str, total: &UsageTotal, width: usize) -> Vec<Line<'static>> {
    let mut result = vec![Line::styled(
        truncate(label, width),
        Style::default().fg(MUTED),
    )];
    let amount = compact(total.tokens);
    let cost = money(total.cost);
    let amount_style = Style::default().fg(INK).add_modifier(Modifier::BOLD);
    let cost_style = Style::default().fg(CYAN).add_modifier(Modifier::BOLD);
    if columns(&amount) + GAP + columns(&cost) <= width {
        let padding = width - columns(&amount) - columns(&cost);
        result.push(Line::from(vec![
            Span::styled(amount, amount_style),
            Span::raw(" ".repeat(padding)),
            Span::styled(cost, cost_style),
        ]));
    } else {
        result.push(Line::styled(truncate(&amount, width), amount_style));
        result.push(Line::styled(truncate(&cost, width), cost_style));
    }
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Input,
    Output,
    Think,
    Hit,
    Total,
    Cost,
}

impl Column {
    fn label(self) -> &'static str {
        match self {
            Self::Input => "IN",
            Self::Output => "OUT",
            Self::Think => "THINK",
            Self::Hit => "缓存命中",
            Self::Total => "合计",
            Self::Cost => "COST",
        }
    }

    fn cell(self, model: &ModelUsage) -> (String, Color) {
        match self {
            Self::Input => (compact(model.input_total), INK),
            Self::Output => (compact(model.output), INK),
            Self::Think => (compact(model.reasoning), MUTED),
            Self::Hit => (model.hit_display().unwrap_or_else(|| "—".into()), MUTED),
            Self::Total => (compact(model.total_tokens()), INK),
            Self::Cost => match model.cost {
                Some(value) => (money(value), GREEN),
                None => ("—".into(), MUTED),
            },
        }
    }
}

fn model_table(models: &[ModelUsage], width: usize, limit: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![];
    }
    if models.is_empty() || limit == 0 {
        return vec![Line::styled(
            truncate(" 暂无用量记录", width),
            Style::default().fg(MUTED),
        )];
    }
    let taken = &models[..models.len().min(limit)];
    let mut slots: Vec<_> = [
        Column::Input,
        Column::Output,
        Column::Think,
        Column::Hit,
        Column::Total,
        Column::Cost,
    ]
    .into_iter()
    .map(|column| {
        let cell_width = taken
            .iter()
            .map(|model| columns(&column.cell(model).0))
            .max()
            .unwrap_or(0)
            .max(columns(column.label()));
        (column, cell_width)
    })
    .collect();
    let numeric_width = |slots: &[(Column, usize)]| {
        slots.iter().map(|(_, width)| width).sum::<usize>() + slots.len().saturating_sub(1)
    };
    // Total and cost are the overview, not the first things to disappear.
    // Cache and reasoning remain available on wide panes without making IN
    // an ambiguous token-count/percentage composite.
    for expendable in [Column::Think, Column::Hit, Column::Output, Column::Input] {
        if NAME_MIN + 1 + numeric_width(&slots) <= width {
            break;
        }
        slots.retain(|(column, _)| *column != expendable);
    }
    let name_width = width.saturating_sub(numeric_width(&slots) + 1);
    if name_width < columns("模型") {
        // At a genuinely tiny width, stack values instead of corrupting the
        // numeric columns or allowing a long amount to overwrite its neighbour.
        let mut result = vec![];
        for model in taken {
            result.push(Line::styled(
                truncate(&model.model, width),
                Style::default().fg(CYAN),
            ));
            for column in [Column::Total, Column::Cost] {
                let (value, color) = column.cell(model);
                result.push(Line::styled(
                    truncate(&value, width),
                    Style::default().fg(color),
                ));
            }
        }
        return result;
    }
    let row = |name: &str, color: Color, cells: Vec<(String, Color)>| {
        let name = truncate(name, name_width);
        let mut spans = vec![
            Span::styled(name.clone(), Style::default().fg(color)),
            Span::raw(" ".repeat(name_width + 1 - columns(&name))),
        ];
        for (index, ((_, cell_width), (text, color))) in slots.iter().zip(cells).enumerate() {
            if index > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                format!(
                    "{}{}",
                    " ".repeat(cell_width.saturating_sub(columns(&text))),
                    text
                ),
                Style::default().fg(color),
            ));
        }
        Line::from(spans)
    };
    let mut result = vec![row(
        "模型",
        MUTED,
        slots
            .iter()
            .map(|(column, _)| (column.label().into(), MUTED))
            .collect(),
    )];
    for model in taken {
        result.push(row(
            &model.model,
            CYAN,
            slots.iter().map(|(column, _)| column.cell(model)).collect(),
        ));
    }
    result
}

fn money(value: f64) -> String {
    if value >= 1_000. {
        format!("${value:.0}")
    } else if value >= 100. {
        format!("${value:.1}")
    } else {
        format!("${value:.2}")
    }
}

enum Series<'a> {
    Tokens(&'a [u64]),
    Money(&'a [f64]),
}

impl Series<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Tokens(values) => values.len(),
            Self::Money(values) => values.len(),
        }
    }

    fn value(&self, index: usize) -> f64 {
        match self {
            Self::Tokens(values) => values.get(index).copied().unwrap_or(0) as f64,
            Self::Money(values) => values.get(index).copied().unwrap_or(0.0),
        }
    }

    fn money(&self) -> bool {
        matches!(self, Self::Money(_))
    }
}

struct Chart<'a> {
    label: &'static str,
    series: Series<'a>,
    span_seconds: u64,
    /// Four independent bars per hour/day. This groups ticks, never values.
    tick_buckets: usize,
    first_tick: u32,
}

fn local_charts(usage: &UsageStats) -> [Chart<'_>; 3] {
    [
        Chart {
            label: "今日 Token",
            series: Series::Tokens(&usage.hours.buckets),
            span_seconds: 15 * 60,
            tick_buckets: 4,
            first_tick: 0,
        },
        Chart {
            label: "本月 Token",
            series: Series::Tokens(&usage.month.buckets),
            span_seconds: 6 * 3_600,
            tick_buckets: 4,
            first_tick: 1,
        },
        Chart {
            label: "本月金额",
            series: Series::Money(&usage.month.costs),
            span_seconds: 6 * 3_600,
            tick_buckets: 4,
            first_tick: 1,
        },
    ]
}

pub(super) fn draw_charts(
    frame: &mut Frame,
    areas: &[Rect],
    usage: &UsageStats,
    view: &mut View,
    now: i64,
) {
    let charts = local_charts(usage);
    let origin = plot_origin(&charts);
    let local =
        chrono::DateTime::from_timestamp(now, 0).map(|time| time.with_timezone(&chrono::Local));
    for (index, (chart, area)) in charts.iter().zip(areas).enumerate() {
        if area.height < MIN_CHART_HEIGHT {
            continue;
        }
        // Monthly tokens and money deliberately use the same navigation state.
        let nav = usize::from(index > 0);
        let plot = (area.width as usize).saturating_sub(origin);
        if let Some(window) = Window::new(chart.series.len(), chart.tick_buckets, plot, 0) {
            view.max_chart_starts[nav] = window.max_start;
            let current = local.as_ref().map_or(0, |time| {
                if nav == 0 {
                    time.hour() as usize
                } else {
                    time.day0() as usize
                }
            });
            if !view.chart_manual {
                // Follow the current hour/day, not the far-right future zeros.
                view.chart_starts[nav] = current
                    .saturating_add(1)
                    .saturating_sub(window.groups)
                    .min(window.max_start);
            } else {
                view.chart_starts[nav] = view.chart_starts[nav].min(window.max_start);
            }
        }
        frame.render_widget(
            Paragraph::new(histogram(
                chart,
                area.width,
                chart_rows(area.height),
                origin,
                view.chart_starts[nav],
            )),
            *area,
        );
    }
}

fn chart_rows(room: u16) -> usize {
    // Title, baseline, tick labels, and one blank separation row. Plot height
    // is no longer snapped to 8/5/4/2 just to make the Y labels look round.
    room.saturating_sub(4) as usize
}

fn axis_scale(max: f64) -> (f64, &'static str) {
    if max >= 1e9 {
        (1e9, "B")
    } else if max >= 1e6 {
        (1e6, "M")
    } else if max >= 1e3 {
        (1e3, "K")
    } else {
        (1., "")
    }
}

fn axis_label(value: f64, divisor: f64, suffix: &str) -> String {
    if value.abs() < f64::EPSILON {
        return "0".into();
    }
    let scaled = value / divisor;
    if (scaled - scaled.round()).abs() < 1e-6 || scaled >= 10. {
        format!("{scaled:.0}{suffix}")
    } else {
        format!("{scaled:.1}{suffix}")
    }
}

fn nice_ceiling(value: f64) -> f64 {
    if !value.is_finite() || value <= 0. {
        return 1.;
    }
    let unit = 10f64.powf(value.log10().floor());
    let scaled = value / unit;
    let step = [1., 2., 2.5, 5., 10.]
        .into_iter()
        .find(|step| *step >= scaled)
        .unwrap_or(10.);
    (step * unit).max(value)
}

fn value_label(value: f64, max: f64, money: bool) -> String {
    if money {
        if max < 1. {
            format!("${value:.2}")
        } else {
            format!("${value:.0}")
        }
    } else {
        let (divisor, suffix) = axis_scale(max);
        axis_label(value, divisor, suffix)
    }
}

fn plot_origin(charts: &[Chart<'_>]) -> usize {
    let label_width = charts
        .iter()
        .map(|chart| {
            let max = nice_ceiling(series_peak(&chart.series));
            columns(&value_label(max, max, chart.series.money()))
        })
        .max()
        .unwrap_or(0)
        .max(5);
    // Leading space + label + trailing space + vertical axis. Bars, baseline
    // and number labels all use this *same* origin, never a second +1/+2 rule.
    label_width + 3
}

fn span_label(seconds: u64) -> String {
    match seconds {
        s if s % 86_400 == 0 => format!("{}天", s / 86_400),
        s if s % 3_600 == 0 => format!("{}小时", s / 3_600),
        s if s % 60 == 0 => format!("{}分钟", s / 60),
        s => format!("{s}秒"),
    }
}

/// The scale is computed over ALL native buckets, not just the visible window.
/// Panning/resizing cannot change a bucket's height ratio or colour band.
fn series_peak(series: &Series<'_>) -> f64 {
    (0..series.len())
        .map(|index| series.value(index))
        .filter(|value| value.is_finite())
        .fold(0., f64::max)
}

fn bar_color(share: f64) -> Color {
    if share >= 0.95 {
        BAR_RED
    } else if share >= 0.80 {
        BAR_HIGH
    } else if share >= 0.30 {
        BAR_MID
    } else {
        BAR_LOW
    }
}

/// Show complete hour/day groups without merging their four native buckets.
/// A narrow terminal pans through the original series instead of changing time
/// resolution. Empty buckets keep their slots. Each bar needs a real gap.
#[derive(Debug)]
struct Window {
    first_group: usize,
    groups: usize,
    max_start: usize,
    start: usize,
    len: usize,
}

impl Window {
    fn new(len: usize, group: usize, plot: usize, requested: usize) -> Option<Self> {
        if len == 0 || group == 0 {
            return None;
        }
        let groups = (plot.saturating_sub(1) / (2 * group)).min(len.div_ceil(group));
        if groups == 0 {
            return None;
        }
        let max_start = len.div_ceil(group).saturating_sub(groups);
        let first_group = requested.min(max_start);
        let start = first_group * group;
        Some(Self {
            first_group,
            groups,
            max_start,
            start,
            len: (groups * group).min(len - start),
        })
    }
}

/// Odd-width full-cell bars have a real centre cell. The old two-column bar
/// placed its tick in the right cell: that was half a cell off optically even
/// though a test using the same floor(width/2) convention claimed alignment.
/// One blank at each plot edge also keeps two-digit group labels in bounds.
struct Geometry {
    origin: usize,
    plot: usize,
    bar_width: usize,
    starts: Vec<usize>,
}

impl Geometry {
    fn new(width: u16, origin: usize, count: usize) -> Option<Self> {
        let plot = (width as usize).saturating_sub(origin);
        if count == 0 || plot < 2 * count + 1 {
            return None;
        }
        let available = plot - 1;
        let mut bar_width = (available / count - 1).max(1);
        if bar_width.is_multiple_of(2) {
            bar_width -= 1;
        }
        let starts = (0..count)
            .map(|index| {
                let left = index * available / count;
                let right = (index + 1) * available / count;
                1 + left + (right - left - bar_width - 1) / 2
            })
            .collect();
        Some(Self {
            origin,
            plot,
            bar_width,
            starts,
        })
    }

    fn centre(&self, index: usize) -> usize {
        self.starts[index] + self.bar_width / 2
    }

    fn gap_after(&self, index: usize) -> usize {
        let end = self.starts[index] + self.bar_width;
        self.starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.plot)
            .saturating_sub(end)
    }
}

fn histogram(
    chart: &Chart<'_>,
    width: u16,
    rows: usize,
    origin: usize,
    first_group: usize,
) -> Vec<Line<'static>> {
    let plot = (width as usize).saturating_sub(origin);
    if chart.series.len() == 0 || plot == 0 || rows == 0 {
        return vec![];
    }
    let Some(window) = Window::new(chart.series.len(), chart.tick_buckets, plot, first_group)
    else {
        return vec![Line::styled(
            truncate(
                &format!(
                    "{} · 请拉宽，保留每柱{}",
                    chart.label,
                    span_label(chart.span_seconds)
                ),
                width as usize,
            ),
            Style::default().fg(MUTED),
        )];
    };
    let Some(geometry) = Geometry::new(width, origin, window.len) else {
        return vec![];
    };
    let max = nice_ceiling(series_peak(&chart.series));
    let subrows = rows * 8;
    let values: Vec<_> = (window.start..window.start + window.len)
        .map(|index| chart.series.value(index))
        .collect();
    let eighths: Vec<_> = values
        .iter()
        .map(|value| {
            if !value.is_finite() || *value <= 0. {
                0
            } else {
                ((*value / max * subrows as f64).round() as usize).min(subrows)
            }
        })
        .collect();
    // All four bars get a small tick. Integer hours/dates label the FIRST
    // bucket of the group (00 minutes / 00 hours), never an arbitrary bar.
    let ticks: Vec<_> = (0..window.len)
        .step_by(chart.tick_buckets)
        .map(|index| {
            (
                geometry.centre(index),
                (chart.first_tick as usize + window.first_group + index / chart.tick_buckets)
                    .to_string(),
            )
        })
        .collect();
    let first = chart.first_tick as usize + window.first_group;
    let last = first + window.groups - 1;
    let unit = if chart.first_tick == 0 { "时" } else { "日" };
    let title = format!(
        " {} · 每柱 {} · {}-{}{}{}",
        chart.label,
        span_label(chart.span_seconds),
        first,
        last,
        unit,
        if window.max_start > 0 { " ←→" } else { "" }
    );
    let mut result = vec![Line::styled(
        truncate(&title, width as usize),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    )];
    let tick_step = rows.div_ceil(4);
    for row in 0..rows {
        let head = if row.is_multiple_of(tick_step) {
            value_label(
                max * (rows - row) as f64 / rows as f64,
                max,
                chart.series.money(),
            )
        } else {
            String::new()
        };
        let mut spans = axis_prefix(&head, '┤', geometry.origin);
        spans.push(Span::raw(" ".repeat(geometry.starts[0])));
        let base = (rows - 1 - row) * 8;
        for (index, filled) in eighths.iter().enumerate() {
            let glyph = match filled.saturating_sub(base).min(8) {
                0 => ' ',
                8 => '█',
                n => ['▁', '▂', '▃', '▄', '▅', '▆', '▇'][n - 1],
            };
            spans.push(Span::styled(
                glyph.to_string().repeat(geometry.bar_width),
                Style::default().fg(bar_color(values[index] / max)),
            ));
            spans.push(Span::raw(" ".repeat(geometry.gap_after(index))));
        }
        result.push(Line::from(spans));
    }
    let mut rule = vec!['─'; geometry.plot];
    for index in 0..window.len {
        rule[geometry.centre(index)] = '┴';
    }
    let mut baseline = axis_prefix(
        &value_label(0., max, chart.series.money()),
        '└',
        geometry.origin,
    );
    // Emphasise the four-bucket group starts without shifting a tick's cell.
    for (column, glyph) in rule.into_iter().enumerate() {
        let major = ticks.iter().any(|(centre, _)| *centre == column);
        baseline.push(Span::styled(
            glyph.to_string(),
            Style::default().fg(if major { CYAN } else { TRACK }),
        ));
    }
    result.push(Line::from(baseline));
    result.push(tick_labels(&ticks, &geometry));
    result
}

fn axis_prefix(label: &str, mark: char, origin: usize) -> Vec<Span<'static>> {
    let padding = origin.saturating_sub(columns(label) + 3);
    vec![
        Span::styled(
            format!(" {}{label} ", " ".repeat(padding)),
            Style::default().fg(MUTED),
        ),
        Span::styled(mark.to_string(), Style::default().fg(TRACK)),
    ]
}

/// Only hour/day labels are thinned on narrow panes; buckets are NEVER merged.
/// Two-digit labels use the conventional integer-cell centring rule. A label
/// is not pinned elsewhere to fit an edge; the plot includes label gutters.
fn tick_labels(ticks: &[(usize, String)], geometry: &Geometry) -> Line<'static> {
    let mut row = String::new();
    let mut next_free = 0;
    for (centre, text) in ticks {
        let width = columns(text);
        let Some(start) = centre.checked_sub(width / 2) else {
            continue;
        };
        if start < next_free || start + width > geometry.plot {
            continue;
        }
        row.push_str(&" ".repeat(start.saturating_sub(columns(&row))));
        row.push_str(text);
        next_free = start + width + 1;
    }
    Line::styled(
        format!("{}{row}", " ".repeat(geometry.origin)),
        Style::default().fg(MUTED),
    )
}
