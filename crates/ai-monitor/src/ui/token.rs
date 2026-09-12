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
const LINE_COLOR: Color = Color::Rgb(33, 150, 243);

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
    for expendable in [Column::Think, Column::Hit, Column::Output, Column::Input] {
        if NAME_MIN + 1 + numeric_width(&slots) <= width {
            break;
        }
        slots.retain(|(column, _)| *column != expendable);
    }
    let name_width = width.saturating_sub(numeric_width(&slots) + 1);
    if name_width < columns("模型") {
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
                format!("{}{}", " ".repeat(cell_width.saturating_sub(columns(&text))), text),
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

#[derive(Clone, Copy)]
enum Series<'a> {
    Tokens(&'a [u64]),
    Money(&'a [f64]),
}

impl Series<'_> {
    fn values(&self) -> Vec<f64> {
        match self {
            Self::Tokens(values) => values.iter().map(|value| *value as f64).collect(),
            Self::Money(values) => values.to_vec(),
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
    aggregate_daily: bool,
    first_tick: u32,
    axis_units: usize,
    buckets_per_unit: usize,
}

fn local_charts(usage: &UsageStats, month_days: usize) -> [Chart<'_>; 3] {
    [
        Chart {
            label: "今日 Token",
            series: Series::Tokens(&usage.hours.buckets),
            span_seconds: 15 * 60,
            aggregate_daily: false,
            first_tick: 0,
            axis_units: 24,
            buckets_per_unit: 4,
        },
        Chart {
            label: "本月 Token",
            series: Series::Tokens(&usage.month.buckets),
            span_seconds: 6 * 3_600,
            aggregate_daily: false,
            first_tick: 1,
            axis_units: month_days,
            buckets_per_unit: 4,
        },
        Chart {
            label: "本月金额",
            series: Series::Money(&usage.month.costs),
            span_seconds: 86_400,
            aggregate_daily: true,
            first_tick: 1,
            axis_units: month_days,
            buckets_per_unit: 1,
        },
    ]
}

pub(super) fn draw_charts(frame: &mut Frame, areas: &[Rect], usage: &UsageStats, now: i64) {
    let month_days = days_in_month(now);
    let charts = local_charts(usage, month_days);
    let through_day = chrono::DateTime::from_timestamp(now, 0)
        .map(|time| chrono::Datelike::day(&time.with_timezone(&chrono::Local)) as usize)
        .unwrap_or(0);
    for (chart, area) in charts.iter().zip(areas) {
        if area.height >= MIN_CHART_HEIGHT {
            draw_stem_chart(frame, *area, chart, through_day);
        }
    }
}

fn days_in_month(now: i64) -> usize {
    let Some(time) = chrono::DateTime::from_timestamp(now, 0) else {
        return 31;
    };
    let local = time.with_timezone(&chrono::Local);
    let year = chrono::Datelike::year(&local);
    let month = chrono::Datelike::month(&local);
    let first = chrono::NaiveDate::from_ymd_opt(year, month, 1);
    let next = if month == 12 {
        chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    match (first, next) {
        (Some(first), Some(next)) => (next - first).num_days() as usize,
        _ => 31,
    }
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

fn span_label(seconds: u64) -> String {
    match seconds {
        s if s % 86_400 == 0 => format!("{}天", s / 86_400),
        s if s % 3_600 == 0 => format!("{}小时", s / 3_600),
        s if s % 60 == 0 => format!("{}分钟", s / 60),
        s => format!("{s}秒"),
    }
}

fn chart_values(chart: &Chart<'_>, through_day: usize) -> Vec<f64> {
    let raw = chart.series.values();
    if !chart.aggregate_daily {
        return raw;
    }
    raw.chunks(4)
        .take(through_day.max(1))
        .map(|day| day.iter().copied().filter(|value| value.is_finite()).sum())
        .collect()
}

fn slot_x(index: usize, slots: usize, plot_width: usize) -> usize {
    if slots == 0 || plot_width == 0 {
        return 0;
    }
    ((2 * index + 1) * plot_width / (2 * slots)).min(plot_width - 1)
}

fn unit_x(index: usize, units: usize, plot_width: usize) -> usize {
    slot_x(index, units, plot_width)
}

fn tick_positions(chart: &Chart<'_>, plot_width: usize) -> Vec<(usize, String)> {
    if chart.axis_units == 0 || plot_width == 0 {
        return vec![];
    }
    let max_labels = (plot_width / 3).max(1).min(chart.axis_units);
    let indices = if max_labels == 1 {
        vec![chart.axis_units - 1]
    } else if max_labels == chart.axis_units {
        (0..chart.axis_units).collect()
    } else {
        (0..max_labels)
            .map(|index| index * (chart.axis_units - 1) / (max_labels - 1))
            .collect()
    };
    indices
        .into_iter()
        .map(|index| {
            (
                unit_x(index, chart.axis_units, plot_width),
                (chart.first_tick as usize + index).to_string(),
            )
        })
        .collect()
}

fn stem_heights(
    chart: &Chart<'_>,
    values: &[f64],
    max: f64,
    plot_width: usize,
    plot_rows: usize,
) -> Vec<usize> {
    let mut heights = vec![0; plot_width];
    let slots = chart.axis_units.saturating_mul(chart.buckets_per_unit);
    if slots == 0 || plot_width == 0 || plot_rows == 0 || max <= 0. {
        return heights;
    }
    for (index, value) in values.iter().copied().take(slots).enumerate() {
        if !value.is_finite() || value <= 0. {
            continue;
        }
        let x = slot_x(index, slots, plot_width);
        let height = ((value / max) * plot_rows as f64).ceil() as usize;
        heights[x] = heights[x].max(height.clamp(1, plot_rows));
    }
    heights
}

fn draw_stem_chart(frame: &mut Frame, area: Rect, chart: &Chart<'_>, through_day: usize) {
    if area.width < MIN_CHART_WIDTH || area.height < MIN_CHART_HEIGHT {
        return;
    }
    let values = chart_values(chart, through_day);
    if values.is_empty() {
        return;
    }
    let max = nice_ceiling(
        values
            .iter()
            .copied()
            .filter(|value| value.is_finite())
            .fold(0., f64::max),
    );
    let top_label = value_label(max, max, chart.series.money());
    let mid_label = value_label(max / 2., max, chart.series.money());
    let origin = columns(&top_label).max(columns(&mid_label)).max(3) + 2;
    if area.width as usize <= origin + 4 {
        return;
    }
    let title = format!(" {} · {}", chart.label, span_label(chart.span_seconds));
    frame.render_widget(
        Paragraph::new(Line::styled(
            truncate(&title, area.width as usize),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        )),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let plot_rows = area.height.saturating_sub(3);
    let plot_width = area.width.saturating_sub(origin as u16);
    if plot_rows < 2 || plot_width < 2 {
        return;
    }
    let axis = [
        (0u16, top_label),
        (plot_rows / 2, mid_label),
        (plot_rows, value_label(0., max, chart.series.money())),
    ];
    for (offset, label) in axis {
        let y = area.y + 1 + offset.min(plot_rows);
        let mark = if offset == plot_rows { '└' } else { '┤' };
        let text = format!(" {:>width$} {mark}", label, width = origin.saturating_sub(3));
        frame.render_widget(
            Paragraph::new(Span::styled(text, Style::default().fg(TRACK))),
            Rect::new(area.x, y, origin as u16, 1),
        );
    }

    let plot_x = area.x + origin as u16;
    let baseline_y = area.y + 1 + plot_rows;
    frame.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(plot_width as usize),
            Style::default().fg(TRACK),
        )),
        Rect::new(plot_x, baseline_y, plot_width, 1),
    );

    let heights = stem_heights(
        chart,
        &values,
        max,
        plot_width as usize,
        plot_rows as usize,
    );
    let stem_style = Style::default().fg(LINE_COLOR).bg(Color::Reset);
    for (column, height) in heights.into_iter().enumerate() {
        if height == 0 {
            continue;
        }
        let x = plot_x + column as u16;
        for offset in 0..height as u16 {
            let y = baseline_y - 1 - offset;
            frame.buffer_mut()[(x, y)]
                .set_symbol("│")
                .set_style(stem_style);
        }
        frame.buffer_mut()[(x, baseline_y)]
            .set_symbol("┴")
            .set_style(stem_style);
    }

    let mut labels = vec![' '; plot_width as usize];
    for (x, text) in tick_positions(chart, plot_width as usize) {
        let text_width = columns(&text);
        let start = x
            .saturating_sub(text_width / 2)
            .min(labels.len().saturating_sub(text_width));
        for (offset, ch) in text.chars().enumerate() {
            if start + offset < labels.len() {
                labels[start + offset] = ch;
            }
        }
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            labels.into_iter().collect::<String>(),
            Style::default().fg(MUTED),
        )),
        Rect::new(plot_x, baseline_y + 1, plot_width, 1),
    );
}
