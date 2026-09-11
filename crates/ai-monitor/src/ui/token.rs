//! Local usage presentation only. SQL, pricing, cache accounting and provider
//! polling stay in their existing owners. All geometry is in terminal cells.

use super::{CYAN, GREEN, INK, MUTED, TRACK, columns, compact, truncate};
use crate::model::{ModelUsage, UsageStats, UsageTotal};
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
const BAR: Color = Color::Rgb(33, 150, 243);

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
    /// Four quarter hours per hourly bar, or four six-hour buckets per day.
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

pub(super) fn draw_charts(frame: &mut Frame, areas: &[Rect], usage: &UsageStats) {
    let charts = local_charts(usage);
    // A shared gutter keeps the monthly money and token timelines aligned even
    // when one Y axis needs an extra digit. Summing the series gives a safe
    // label-width bound before the width-dependent bucket merge is selected.
    let origin = plot_origin(&charts);
    for (chart, area) in charts.iter().zip(areas) {
        if area.height < MIN_CHART_HEIGHT {
            continue;
        }
        if area.width as usize <= origin {
            frame.render_widget(
                Paragraph::new(truncate("请拉宽查看趋势", area.width as usize))
                    .style(Style::default().fg(MUTED)),
                *area,
            );
        } else {
            frame.render_widget(
                Paragraph::new(histogram(
                    chart,
                    area.width,
                    chart_rows(area.height),
                    origin,
                )),
                *area,
            );
        }
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
            let total: f64 = (0..chart.series.len())
                .map(|index| chart.series.value(index))
                .filter(|value| value.is_finite() && *value > 0.)
                .sum();
            let max = nice_ceiling(total);
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

/// Defaults stay at one hour / one day regardless of spare screen width.
/// Coarsen only when real one-cell gaps cannot fit, always in whole hours or
/// days. Never invent a 45-minute / 18-hour bucket just to fill a row.
fn merge_factor(chart: &Chart<'_>, plot: usize) -> usize {
    let capacity = plot.div_ceil(2).max(1);
    let needed = chart.series.len().div_ceil(capacity);
    needed.max(chart.tick_buckets).div_ceil(chart.tick_buckets) * chart.tick_buckets
}

fn merged_values(series: &Series<'_>, factor: usize) -> Vec<f64> {
    (0..series.len().div_ceil(factor))
        .map(|index| {
            let start = index * factor;
            let end = (start + factor).min(series.len());
            (start..end).map(|bucket| series.value(bucket)).sum()
        })
        .collect()
}

/// A single integer-cell geometry for bars and the ticks naming those bars.
/// All bars have equal width; leftover columns become real space, not a
/// three-quarter glyph whose full-width top would produce a burr.
struct Geometry {
    origin: usize,
    plot: usize,
    bar_width: usize,
    starts: Vec<usize>,
}

impl Geometry {
    fn new(width: u16, origin: usize, count: usize) -> Self {
        let plot = (width as usize).saturating_sub(origin);
        let count = count.max(1);
        let bar_width = plot
            .saturating_sub(count - 1)
            .checked_div(count)
            .unwrap_or(0)
            .max(1);
        let gap_cells = plot.saturating_sub(count * bar_width);
        let starts = (0..count)
            .map(|index| {
                index * bar_width
                    + if count > 1 {
                        index * gap_cells / (count - 1)
                    } else {
                        0
                    }
            })
            .collect();
        Self {
            origin,
            plot,
            bar_width,
            starts,
        }
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

fn histogram(chart: &Chart<'_>, width: u16, rows: usize, origin: usize) -> Vec<Line<'static>> {
    let plot = (width as usize).saturating_sub(origin);
    if chart.series.len() == 0 || plot == 0 || rows == 0 {
        return vec![];
    }
    let factor = merge_factor(chart, plot);
    let merged = merged_values(&chart.series, factor);
    let geometry = Geometry::new(width, origin, merged.len());
    let peak = merged
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .fold(0f64, f64::max);
    let max = nice_ceiling(peak);
    let subrows = rows * 8;
    let eighths: Vec<_> = merged
        .iter()
        .map(|value| {
            if !value.is_finite() || *value <= 0. {
                0
            } else {
                ((*value / max * subrows as f64).round() as usize).min(subrows)
            }
        })
        .collect();
    let ticks: Vec<_> = (0..merged.len())
        .map(|index| {
            (
                geometry.centre(index),
                (chart.first_tick as usize + index * factor / chart.tick_buckets).to_string(),
            )
        })
        .collect();
    let partial = if chart.series.len().is_multiple_of(factor) {
        ""
    } else {
        " · 末柱不足"
    };
    let title = format!(
        " {} · 每柱 {}{}",
        chart.label,
        span_label(chart.span_seconds * factor as u64),
        partial,
    );
    let mut result = vec![Line::styled(
        truncate(&title, width as usize),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    )];
    // Labels are sparse, independently of the number of usable plot rows.
    // Each labelled boundary is computed from its actual row, not a second
    // vertical scale that could disagree with the bar's height.
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
        let base = (rows - 1 - row) * 8;
        for (index, filled) in eighths.iter().enumerate() {
            let glyph = match filled.saturating_sub(base).min(8) {
                0 => ' ',
                8 => '█',
                n => ['▁', '▂', '▃', '▄', '▅', '▆', '▇'][n - 1],
            };
            spans.push(Span::styled(
                glyph.to_string().repeat(geometry.bar_width),
                Style::default().fg(BAR),
            ));
            spans.push(Span::raw(" ".repeat(geometry.gap_after(index))));
        }
        result.push(Line::from(spans));
    }
    let mut rule = vec!['─'; geometry.plot];
    for (column, _) in &ticks {
        if let Some(cell) = rule.get_mut(*column) {
            *cell = '┴';
        }
    }
    let mut baseline = axis_prefix(
        &value_label(0., max, chart.series.money()),
        '└',
        geometry.origin,
    );
    baseline.push(Span::styled(
        rule.into_iter().collect::<String>(),
        Style::default().fg(TRACK),
    ));
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

/// A label may be skipped for collision or an edge, but never moved away from
/// the tick it names. Even-length labels use the same integer-cell centre
/// convention as an even-width bar (at most half a character optically).
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
