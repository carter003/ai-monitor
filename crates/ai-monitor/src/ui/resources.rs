//! System resources: values first, per-core meters second, history last.
//! This module only renders SystemStats; sampling and the other panels are unchanged.
use super::{AMBER, CYAN, GREEN, INK, MUTED, RED, columns, truncate};
use crate::system::{SystemStats, gib};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};

#[cfg(test)]
mod tests;

const TRACK: Color = Color::Rgb(205, 214, 221);
const HISTORY_HEIGHT: usize = 4; // heading, at least two plot rows, footer

fn content_width(width: u16) -> u16 {
    let inner = width.saturating_sub(2);
    inner.saturating_sub(if inner >= 26 { 2 } else { 0 })
}

fn core_columns(width: u16) -> usize {
    if width >= 28 { 2 } else { 1 }
}

/// Keep eight rows for quotas; count core rows, not individual CPUs.
pub(super) fn height(body_height: u16, width: u16, core_count: usize) -> u16 {
    let rows = core_count
        .max(1)
        .div_ceil(core_columns(content_width(width)));
    (rows.min(u16::MAX as usize) as u16)
        .saturating_add(14)
        .max(body_height / 3)
        .min(body_height.saturating_sub(8))
}

/// Slices are always contained in the parent, including zero-sized panes.
fn slice(area: Rect, x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(
        area.x + x.min(area.width),
        area.y + y.min(area.height),
        width.min(area.width.saturating_sub(x)),
        height.min(area.height.saturating_sub(y)),
    )
}

fn row(area: Rect, y: u16) -> Rect {
    slice(area, 0, y, area.width, 1)
}

fn text(frame: &mut Frame, area: Rect, value: &str, color: Color, bold: bool) {
    let mut style = Style::default().fg(color);
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    frame.render_widget(Paragraph::new(value).style(style), area);
}

/// Never clip a numeric value into a different, apparently valid number.
fn right(frame: &mut Frame, area: Rect, value: &str, color: Color, bold: bool) {
    let value = if columns(value) <= area.width as usize {
        value
    } else {
        "…"
    };
    let mut style = Style::default().fg(color);
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    frame.render_widget(Paragraph::new(value).style(style).right_aligned(), area);
}

fn percentage(value: Option<f64>) -> Option<f64> {
    value
        .filter(|n| n.is_finite() && *n >= 0.)
        .map(|n| n.min(100.))
}

fn used_percent(used: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| (used as f64 / total as f64 * 100.).min(100.))
}

fn usage_color(value: Option<f64>, normal: Color) -> Color {
    match value {
        Some(n) if n >= 95. => RED,
        Some(n) if n >= 85. => AMBER,
        Some(_) => normal,
        None => MUTED,
    }
}

fn thin_bar(frame: &mut Frame, area: Rect, value: Option<f64>, color: Color) {
    let width = area.width as usize;
    let filled = percentage(value)
        .filter(|n| *n > 0.)
        .map_or(0, |n| ((n * width as f64 / 100.).round() as usize).max(1))
        .min(width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("━".repeat(filled), Style::default().fg(color)),
            Span::styled("─".repeat(width - filled), Style::default().fg(TRACK)),
        ])),
        area,
    );
}

fn metric(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    value: Option<f64>,
    detail: &str,
    color: Color,
) {
    text(frame, slice(area, 0, 0, 4, 1), name, MUTED, false);
    let value = percentage(value);
    let percent = value.map_or_else(|| "—".into(), |n| format!("{n:.1}%"));
    let detail_width = columns(detail).max(13);
    let reserved = 4 + 2 + detail_width + 1 + 6;
    if area.width >= 40 && area.width as usize >= reserved + 4 {
        let bar_width = area.width - reserved as u16;
        thin_bar(
            frame,
            slice(area, 4, 0, bar_width, 1),
            value,
            usage_color(value, color),
        );
        right(
            frame,
            slice(area, 4 + bar_width + 2, 0, detail_width as u16, 1),
            detail,
            INK,
            false,
        );
        right(
            frame,
            slice(area, area.width - 6, 0, 6, 1),
            &percent,
            INK,
            true,
        );
    } else {
        let values = slice(area, 4, 0, area.width, 1);
        let full = format!("{detail}  {percent}");
        let compact = format!(
            "{} {percent}",
            detail.replace(" GiB", "G").replace(" 逻辑核", "核")
        );
        let selected = if value.is_none() {
            detail
        } else if columns(&full) <= values.width as usize {
            &full
        } else if columns(&compact) <= values.width as usize {
            &compact
        } else {
            &percent
        };
        right(frame, values, selected, INK, true);
    }
}

/// Binary units, no leading zeroes. Unknown/invalid is distinct from zero.
fn throughput(bytes: Option<f64>, compact: bool) -> String {
    let Some(mut value) = bytes.filter(|n| n.is_finite() && *n >= 0.) else {
        return "—".into();
    };
    let units = if compact {
        ["B/s", "K/s", "M/s", "G/s", "T/s", "P/s", "E/s"]
    } else {
        [
            " B/s", " KiB/s", " MiB/s", " GiB/s", " TiB/s", " PiB/s", " EiB/s",
        ]
    };
    let mut unit = 0;
    while value >= 1024. && unit < units.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    // Promote when integer rounding would print 1024 in the smaller unit.
    if value.round() >= 1024. && unit < units.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    if unit > 0 && value < 100. {
        format!("{value:.1}{}", units[unit])
    } else {
        format!("{value:.0}{}", units[unit])
    }
}

fn io_row(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    rates: [(&str, Option<f64>); 2],
    color: Color,
) {
    text(frame, slice(area, 0, 0, 4, 1), name, MUTED, false);
    let values = slice(area, 4, 0, area.width, 1);
    let gap = if values.width >= 24 { 2 } else { 1 };
    let width = values.width.saturating_sub(gap) / 2;
    for (index, (tag, rate)) in rates.into_iter().enumerate() {
        let cell = slice(values, index as u16 * (width + gap), 0, width, 1);
        text(frame, slice(cell, 0, 0, 1, 1), tag, color, true);
        let value_area = slice(cell, 2, 0, cell.width, 1);
        // Select units by column width so both directions change together,
        // rather than oscillating between KiB/s and K/s with each sample.
        let value = throughput(rate, width < 12);
        right(frame, value_area, &value, color, false);
    }
}

fn load_row(frame: &mut Frame, area: Rect, load: &str) {
    text(frame, slice(area, 0, 0, 4, 1), "LOAD", MUTED, false);
    let mut parts = load.split_whitespace();
    let values: Vec<_> = (0..3).map(|_| parts.next().unwrap_or("—")).collect();
    let details = slice(area, 5, 0, area.width, 1);
    let labels = ["1m", "5m", "15m"];
    let cell_width = details.width / 3;
    if area.width >= 38
        && labels
            .iter()
            .zip(&values)
            .all(|(label, value)| columns(label) + columns(value) + 1 < cell_width as usize)
    {
        for (index, (label, value)) in labels.iter().zip(&values).enumerate() {
            let cell = slice(details, index as u16 * cell_width, 0, cell_width, 1);
            text(frame, cell, label, MUTED, false);
            text(
                frame,
                slice(cell, label.len() as u16 + 1, 0, cell.width, 1),
                value,
                INK,
                true,
            );
        }
    } else {
        let all = values.join(" · ");
        if columns(&all) <= details.width as usize {
            right(frame, details, &all, INK, true);
        } else {
            right(frame, details, &format!("1m {}", values[0]), INK, true);
        }
    }
}

fn section(frame: &mut Frame, area: Rect, title: &str, suffix: &str) {
    let title = format!("{title} ");
    let suffix = format!(" {suffix}");
    text(frame, area, &"─".repeat(area.width as usize), TRACK, false);
    let suffix_width = columns(&suffix).min(area.width as usize) as u16;
    let title_width = area.width.saturating_sub(suffix_width + 2);
    text(
        frame,
        slice(area, 0, 0, title_width, 1),
        &truncate(&title, title_width as usize),
        CYAN,
        true,
    );
    right(
        frame,
        slice(area, area.width - suffix_width, 0, suffix_width, 1),
        &suffix,
        MUTED,
        false,
    );
}

fn core(frame: &mut Frame, area: Rect, index: usize, count: usize, value: f64) {
    let digits = count.to_string().len().max(2);
    let label = format!("{:0digits$}", index + 1);
    let value = percentage(Some(value));
    let percent = value.map_or_else(|| "—".into(), |n| format!("{n:.0}%"));
    text(
        frame,
        slice(area, 0, 0, digits as u16, 1),
        &label,
        MUTED,
        false,
    );
    let value_width = 4.min(area.width.saturating_sub(digits as u16 + 1));
    let value_x = area.width.saturating_sub(value_width);
    right(
        frame,
        slice(area, value_x, 0, value_width, 1),
        &percent,
        INK,
        true,
    );
    let bar_x = digits as u16 + 1;
    let bar_width = value_x.saturating_sub(bar_x + 1);
    thin_bar(
        frame,
        slice(area, bar_x, 0, bar_width, 1),
        value,
        usage_color(value, GREEN),
    );
}

fn history(frame: &mut Frame, area: Rect, stats: &SystemStats) {
    if area.height < HISTORY_HEIGHT as u16 || area.width == 0 {
        return;
    }
    let capacity = area.width as usize * 2;
    let samples: Vec<_> = stats
        .cpu_history
        .iter()
        .rev()
        .take(capacity)
        .rev()
        .map(|n| (*n).min(100))
        .collect();
    let peak_val = samples.iter().copied().max().unwrap_or(0);
    let peak = if samples.is_empty() {
        "峰值 —".into()
    } else {
        format!("峰值 {peak_val}%")
    };
    section(frame, row(area, 0), "CPU 趋势", &peak);
    let graph = slice(area, 0, 1, area.width, area.height - 2);
    let scale_max = match peak_val {
        0..=20 => 25,
        21..=45 => 50,
        46..=75 => 80,
        _ => 100,
    };
    if samples.is_empty() {
        text(frame, row(graph, 0), "采样中", MUTED, false);
    } else {
        let resolution = u64::from(graph.height) * 2;
        let levels: Vec<u64> = samples
            .iter()
            .map(|n| {
                if *n == 0 {
                    0
                } else {
                    let lvl = (*n * resolution + scale_max / 2) / scale_max;
                    lvl.clamp(1, resolution)
                }
            })
            .collect();
        let active_cols = samples.len().div_ceil(2) as u16;
        let start_col = graph.width.saturating_sub(active_cols);
        let is_odd = samples.len() % 2 != 0;

        const QUADRANTS: [[char; 3]; 3] = [[' ', '▗', '▐'], ['▖', '▄', '▟'], ['▌', '▙', '█']];

        let mut lines = Vec::with_capacity(graph.height as usize);
        for y in 0..graph.height {
            let row_from_bottom = graph.height - 1 - y;
            let base_level = u64::from(row_from_bottom) * 2;
            let mut line = String::with_capacity(graph.width as usize);
            line.push_str(&" ".repeat(start_col as usize));

            for c in 0..active_cols as usize {
                let (left_h, right_h) = if is_odd {
                    if c == 0 {
                        (0, levels[0])
                    } else {
                        (levels[2 * c - 1], levels[2 * c])
                    }
                } else {
                    (levels[2 * c], levels[2 * c + 1])
                };

                let left_cell = left_h.saturating_sub(base_level).min(2) as usize;
                let right_cell = right_h.saturating_sub(base_level).min(2) as usize;

                line.push(QUADRANTS[left_cell][right_cell]);
            }
            lines.push(Line::from(line));
        }
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().fg(CYAN)),
            graph,
        );
    }
    let footer = row(area, area.height - 1);
    let count = format!("最近 {} 次采样", samples.len());
    let scale = format!("0–{scale_max}%");
    let scale_len = columns(&scale) as u16;
    let count_width = footer.width.saturating_sub(scale_len + 1);
    text(
        frame,
        slice(footer, 0, 0, count_width, 1),
        &count,
        MUTED,
        false,
    );
    right(
        frame,
        slice(
            footer,
            footer.width.saturating_sub(scale_len),
            0,
            scale_len,
            1,
        ),
        &scale,
        MUTED,
        false,
    );
}

fn cores_and_history(frame: &mut Frame, area: Rect, stats: &SystemStats) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let columns = core_columns(area.width);
    let ideal_rows = stats.cores.len().max(1).div_ceil(columns);
    let show_history = area.height as usize >= 1 + ideal_rows + HISTORY_HEIGHT;
    let rows = ideal_rows.min(area.height.saturating_sub(1) as usize);
    let shown = stats.cores.len().min(rows * columns);
    let suffix = if shown < stats.cores.len() {
        format!("{shown}/{}", stats.cores.len())
    } else {
        "逐核占用".into()
    };
    section(frame, row(area, 0), "CPU 核心", &suffix);
    if stats.cores.is_empty() && rows > 0 {
        text(frame, row(area, 1), "采样中", MUTED, false);
    }
    let gap = if columns == 2 { 2 } else { 0 };
    let cell_width = area.width.saturating_sub(gap) / columns as u16;
    let column_rows = shown.div_ceil(columns).max(1);
    for (index, value) in stats.cores.iter().take(shown).enumerate() {
        let x = (index / column_rows) as u16 * (cell_width + gap);
        let y = (index % column_rows) as u16 + 1;
        core(
            frame,
            slice(area, x, y, cell_width, 1),
            index,
            stats.cores.len(),
            *value,
        );
    }
    if show_history {
        history(
            frame,
            slice(area, 0, rows as u16 + 1, area.width, area.height),
            stats,
        );
    }
}

pub(super) fn draw_system(frame: &mut Frame, area: Rect, stats: &SystemStats) {
    let title = if stats.error.is_some() {
        " 系统资源 · 读取失败 "
    } else if area.width >= 28 {
        " 系统资源 · 已用 "
    } else {
        " 系统资源 "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title)
        .title_style(
            Style::default()
                .fg(if stats.error.is_some() { AMBER } else { CYAN })
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(MUTED));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let pad = if inner.width >= 26 { 1 } else { 0 };
    let inner = slice(
        inner,
        pad,
        0,
        inner.width.saturating_sub(pad * 2),
        inner.height,
    );
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let cpu_detail = if percentage(stats.cpu).is_none() {
        "采样中".into()
    } else if stats.cores.is_empty() {
        "逻辑核未知".into()
    } else {
        format!("{} 逻辑核", stats.cores.len())
    };
    metric(frame, row(inner, 0), "CPU", stats.cpu, &cpu_detail, GREEN);
    let memory = if stats.memory_total == 0 {
        "采样中".into()
    } else {
        format!(
            "{:.1}/{:.1} GiB",
            gib(stats.memory_used),
            gib(stats.memory_total)
        )
    };
    metric(
        frame,
        row(inner, 1),
        "MEM",
        used_percent(stats.memory_used, stats.memory_total),
        &memory,
        CYAN,
    );
    let swap = if stats.swap_total == 0 {
        "未启用".into()
    } else {
        format!(
            "{:.1}/{:.1} GiB",
            gib(stats.swap_used),
            gib(stats.swap_total)
        )
    };
    metric(
        frame,
        row(inner, 2),
        "SWP",
        used_percent(stats.swap_used, stats.swap_total),
        &swap,
        AMBER,
    );
    let mut y = 3;
    io_row(
        frame,
        row(inner, y),
        "NET",
        [
            ("↓", stats.network_rx_per_sec),
            ("↑", stats.network_tx_per_sec),
        ],
        CYAN,
    );
    y += 1;
    io_row(
        frame,
        row(inner, y),
        "DSK",
        [
            ("R", stats.disk_read_per_sec),
            ("W", stats.disk_write_per_sec),
        ],
        AMBER,
    );
    y += 1;
    load_row(frame, row(inner, y), &stats.load);
    y += 1;
    if let Some(error) = &stats.error {
        text(
            frame,
            row(inner, y),
            &truncate(error, inner.width as usize),
            AMBER,
            false,
        );
        y += 1;
    }
    cores_and_history(frame, slice(inner, 0, y, inner.width, inner.height), stats);
}

#[cfg(test)]
pub(super) fn print_preview_fixtures() {
    tests::print_previews();
}
