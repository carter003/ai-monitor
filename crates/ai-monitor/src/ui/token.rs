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

mod daily_money;
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

pub(super) fn draw_charts(frame: &mut Frame, areas: &[Rect], usage: &UsageStats, now: i64) {
    let charts = local_charts(usage);
    let origin = plot_origin(&charts);
    let through_day = chrono::DateTime::from_timestamp(now, 0)
        .map(|time| chrono::Datelike::day(&time.with_timezone(&chrono::Local)) as usize)
        .unwrap_or(0);
    for (chart, area) in charts.iter().zip(areas) {
        if area.height >= MIN_CHART_HEIGHT {
            if chart.series.money() {
                daily_money::draw(frame, *area, chart, origin, through_day);
                continue;
            }
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
    // Title, baseline and two label rows (the second is normally blank). Plot height
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
            let max = match &chart.series {
                Series::Money(costs) => daily_money::ceiling(costs),
                _ => nice_ceiling(series_peak(&chart.series)),
            };
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

/// The scale is computed over all native buckets. Resizing changes only
/// raster resolution, never the denominator or a bucket's colour band.
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

/// Full-period, gap-free tiling. Each shared edge ends one bucket AND starts
/// its neighbour. Rounding changes a bin's raster width by at most one unit;
/// it can never create unowned half-cells between positive adjacent buckets.
/// Do not centre a narrower, fixed-width bar inside independently rounded
/// slots: that was the source of the alternating blank/no-blank regression.
struct Geometry {
    origin: usize,
    plot: usize,
    resolution: usize,
    edges: Vec<usize>,
}

impl Geometry {
    fn new(width: u16, origin: usize, count: usize) -> Option<Self> {
        let plot = (width as usize).saturating_sub(origin);
        if count == 0 || plot.saturating_mul(2) < count {
            return None;
        }
        let resolution = if plot >= count { 1 } else { 2 };
        let available = plot * resolution;
        let edges = (0..=count).map(|index| index * available / count).collect();
        Some(Self {
            origin,
            plot,
            resolution,
            edges,
        })
    }

    fn centre(&self, index: usize) -> usize {
        // The terminal column containing the bin midpoint. Half-cell mode
        // still has character-level ticks; it does not claim subpixel ticks.
        (self.edges[index] + self.edges[index + 1] - 1) / (2 * self.resolution)
    }

    fn owners(&self) -> Vec<Option<usize>> {
        let mut owners = vec![None; self.plot * self.resolution];
        for (index, edges) in self.edges.windows(2).enumerate() {
            owners[edges[0]..edges[1]].fill(Some(index));
        }
        owners
    }
}

/// A very slight alternating shade separates touching bars without consuming
/// an empty column. The four semantic bands are selected BEFORE this tint.
fn bar_ink(share: f64, index: usize) -> Color {
    let color = bar_color(share);
    if index.is_multiple_of(2) {
        return color;
    }
    match color {
        Color::Rgb(r, g, b) => {
            let shade = |channel: u8| (u16::from(channel) * 94 / 100) as u8;
            Color::Rgb(shade(r), shade(g), shade(b))
        }
        _ => color,
    }
}

fn quantized_height(value: f64, max: f64, steps: usize) -> usize {
    if !value.is_finite() || value <= 0. || max <= 0. {
        0
    } else {
        ((value / max * steps as f64).round() as usize).min(steps)
    }
}

/// Two independent coloured half-columns in one terminal cell. A foreground
/// left half plus a background right half preserves BOTH colours; braille
/// with a single foreground would lose one bucket's colour. Dense mode uses
/// whole terminal-row heights because a cell cannot encode two differently
/// coloured partial caps plus transparent background (three colours).
fn half_cell(left: Option<Color>, right: Option<Color>) -> Span<'static> {
    let clean = Style::default().fg(Color::Reset).bg(Color::Reset);
    match (left, right) {
        (None, None) => Span::styled(" ", clean),
        (Some(l), None) => Span::styled("▌", clean.fg(l)),
        (None, Some(r)) => Span::styled("▐", clean.fg(r)),
        (Some(l), Some(r)) => Span::styled("▌", clean.fg(l).bg(r)),
    }
}

fn histogram(chart: &Chart<'_>, width: u16, rows: usize, origin: usize) -> Vec<Line<'static>> {
    let plot = (width as usize).saturating_sub(origin);
    let count = chart.series.len();
    if count == 0 || plot == 0 || rows == 0 || chart.tick_buckets == 0 {
        return vec![];
    }
    let last = chart.first_tick as usize + count.div_ceil(chart.tick_buckets) - 1;
    let unit = if chart.first_tick == 0 { "时" } else { "日" };
    let title = format!(
        " {} · 每柱 {} · {}-{}{}",
        chart.label,
        span_label(chart.span_seconds),
        chart.first_tick,
        last,
        unit
    );
    let mut result = vec![Line::styled(
        truncate(&title, width as usize),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    )];
    let Some(geometry) = Geometry::new(width, origin, count) else {
        // Even half-columns have a finite resolution. Do not silently crop,
        // invent a scrollbar, or combine buckets below this physical minimum.
        result.push(Line::styled(
            truncate(
                &format!("完整 {} 柱需至少 {} 列", count, origin + count.div_ceil(2)),
                width as usize,
            ),
            Style::default().fg(MUTED),
        ));
        return result;
    };
    let max = nice_ceiling(series_peak(&chart.series));
    let vertical = if geometry.resolution == 1 { 8 } else { 1 };
    let heights: Vec<_> = (0..count)
        .map(|index| quantized_height(chart.series.value(index), max, rows * vertical))
        .collect();
    let inks: Vec<_> = (0..count)
        .map(|index| bar_ink(chart.series.value(index) / max, index))
        .collect();
    let owners = geometry.owners();
    let ticks: Vec<_> = (0..count)
        .step_by(chart.tick_buckets)
        .map(|index| {
            (
                geometry.centre(index),
                (chart.first_tick as usize + index / chart.tick_buckets).to_string(),
            )
        })
        .collect();
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
        let base = (rows - 1 - row) * vertical;
        if geometry.resolution == 1 {
            for owner in &owners {
                let (used, color) = owner.map_or((0, Color::Reset), |index| {
                    (heights[index].saturating_sub(base).min(8), inks[index])
                });
                let glyph = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'][used];
                spans.push(Span::styled(
                    glyph.to_string(),
                    Style::default().fg(color).bg(Color::Reset),
                ));
            }
        } else {
            for pair in owners.as_chunks::<2>().0 {
                let ink = |owner: Option<usize>| {
                    owner
                        .filter(|index| heights[*index] > base)
                        .map(|index| inks[index])
                };
                spans.push(half_cell(ink(pair[0]), ink(pair[1])));
            }
        }
        result.push(Line::from(spans));
    }
    let mut rule = vec!['─'; geometry.plot];
    // A half-cell cannot have its own box-drawing tick. Dense mode keeps the
    // hour/day major ticks; full-cell mode also draws each bucket's minor tick.
    if geometry.resolution == 1 {
        for index in 0..count {
            rule[geometry.centre(index)] = '┴';
        }
    }
    for (centre, _) in &ticks {
        rule[*centre] = '┴';
    }
    let mut baseline = axis_prefix(
        &value_label(0., max, chart.series.money()),
        '└',
        geometry.origin,
    );
    for (column, glyph) in rule.into_iter().enumerate() {
        let major = ticks.iter().any(|(centre, _)| *centre == column);
        baseline.push(Span::styled(
            glyph.to_string(),
            Style::default().fg(if major { CYAN } else { TRACK }),
        ));
    }
    result.push(Line::from(baseline));
    result.extend(tick_labels(&ticks, &geometry));
    result
}

fn axis_prefix(label: &str, mark: char, origin: usize) -> Vec<Span<'static>> {
    let label = truncate(label, origin.saturating_sub(3));
    let padding = origin.saturating_sub(columns(&label) + 3);
    vec![
        Span::styled(
            format!(" {}{label} ", " ".repeat(padding)),
            Style::default().fg(MUTED),
        ),
        Span::styled(mark.to_string(), Style::default().fg(TRACK)),
    ]
}

/// Keep ALL hour/day numbers. When adjacent two-digit labels cannot share a
/// row, use the existing blank separator row rather than drop labels or move
/// them off their tick. The second row stays blank when everything fits.
fn tick_labels(ticks: &[(usize, String)], geometry: &Geometry) -> [Line<'static>; 2] {
    let mut labels = [String::new(), String::new()];
    let mut next_free = [0usize; 2];
    for (centre, text) in ticks {
        let width = columns(text);
        let start = centre.saturating_sub(width / 2);
        if start + width > geometry.plot {
            continue;
        }
        if let Some(row) = (0..2).find(|row| start >= next_free[*row]) {
            let padding = start.saturating_sub(columns(&labels[row]));
            labels[row].push_str(&" ".repeat(padding));
            labels[row].push_str(text);
            next_free[row] = start + width + 1;
        }
    }
    labels.map(|row| {
        Line::styled(
            format!("{}{row}", " ".repeat(geometry.origin)),
            Style::default().fg(MUTED),
        )
    })
}
