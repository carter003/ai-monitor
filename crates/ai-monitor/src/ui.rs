use crate::{
    model::{SourceState, UsageStats, countdown},
    system::{SystemStats, gib},
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Gauge, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Sparkline,
    },
};

#[cfg(test)]
mod tests;
mod token;

const CYAN: Color = Color::Rgb(11, 93, 107);
const MUTED: Color = Color::Rgb(74, 84, 95);
const GREEN: Color = Color::Rgb(10, 92, 40);
const AMBER: Color = Color::Rgb(124, 67, 0);
const RED: Color = Color::Rgb(163, 22, 22);
const TRACK: Color = Color::Rgb(156, 167, 179);
const INK: Color = Color::Rgb(17, 24, 39);
const LIGHT_INK: Color = Color::Rgb(245, 248, 250);

#[derive(Default)]
pub struct View {
    pub scroll: usize,
    pub max_scroll: usize,
    pub page_size: usize,
    /// Clickable footer hints, `(first column, last column, action)`, in the
    /// footer row. Filled by `draw` so the layout and the hit test cannot drift.
    pub footer_hits: Vec<(u16, u16, FooterAction)>,
    /// The screen row containing the clickable footer hints.
    pub footer_row: Option<u16>,
    /// The scrollable body, so a click or wheel inside it can be routed to the
    /// page. `None` when the pane is too small to draw.
    pub body: Option<Rect>,
}

/// What a footer hint or a mouse gesture asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterAction {
    Refresh,
    ScrollUp,
    ScrollDown,
}

impl FooterAction {
    /// Each hint is `(label, key start, key length, action)`.
    fn row(room: usize, scrollable: bool) -> Vec<(&'static str, usize, usize, FooterAction)> {
        let mut hints = if room >= 30 {
            vec![(" r 刷新   ", 1, 1, FooterAction::Refresh)]
        } else if room >= 20 {
            vec![(" r 刷新  ", 1, 1, FooterAction::Refresh)]
        } else if room >= 10 {
            vec![(" r  ", 1, 1, FooterAction::Refresh)]
        } else {
            vec![]
        };
        if room >= 30 && scrollable {
            hints.push(("↑↓ 滚动   ", 0, 2, FooterAction::ScrollUp));
        }
        hints
    }
}

pub fn draw(
    frame: &mut Frame,
    system: &SystemStats,
    states: &[SourceState],
    usage: &UsageStats,
    view: &mut View,
    now: i64,
) {
    let area = frame.area();
    if area.width < 26 || area.height < 12 {
        view.footer_hits.clear();
        view.footer_row = None;
        view.body = None;
        frame.render_widget(
            Paragraph::new("ai-monitor\n请拉宽或拉高窗格\nq 退出")
                .style(Style::default().fg(CYAN))
                .alignment(Alignment::Center),
            area,
        );
        return;
    }
    let version = concat!("v", env!("CARGO_PKG_VERSION"));
    let parts = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let body = parts[0];

    // The resource/quota sidebar is unchanged; only the local token panel gets
    // a content-driven layout. Each list still shares the page's scroll offset.
    let columns = Layout::horizontal([
        Constraint::Length(sidebar_width(area.width)),
        Constraint::Min(0),
    ])
    .split(body);
    let desired_system_height = (system.cores.len() as u16)
        .saturating_add(8)
        .max(body.height / 3);
    let system_height = desired_system_height.min(body.height.saturating_sub(8));
    let sidebar =
        Layout::vertical([Constraint::Length(system_height), Constraint::Min(0)]).split(columns[0]);
    draw_system(frame, sidebar[0], system);

    let quota_area = sidebar[1];
    let token_area = columns[1];
    let quota_inner = panel(frame, quota_area, " AI 额度 · 剩余 ");
    // Only the model ranking is rolling 24H. The charts and summary identify
    // their own calendar periods instead of inheriting a misleading 24H title.
    let token_inner = panel(frame, token_area, " 本地消耗 · Token ");
    let (token_parts, charts_drawn) = token_rects(token_inner, usage);
    let token_top = token_parts[0];

    let quota = quota_panel_lines(states, quota_inner, now);
    let token = token_panel_lines(usage, token_top);
    let quota_max = quota.len().saturating_sub(quota_inner.height as usize);
    let token_max = token.len().saturating_sub(token_top.height as usize);
    view.page_size = (quota_inner.height as usize).max(token_top.height as usize);
    view.max_scroll = quota_max.max(token_max);
    view.scroll = view.scroll.min(view.max_scroll);
    let scroll = view.scroll;
    render_panel_lines(frame, quota_area, quota_inner, quota, scroll.min(quota_max));
    render_panel_lines(frame, token_top, token_top, token, scroll.min(token_max));
    if charts_drawn {
        token::draw_charts(frame, &token_parts[1..], usage);
    }

    let room = (area.width as usize).saturating_sub(version.len() + 1);
    let hints = FooterAction::row(room, view.max_scroll > 0);
    let mut footer = String::from(" ");
    let mut hint_spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let muted = Style::default().fg(MUTED);
    let key = Style::default().fg(CYAN).add_modifier(Modifier::BOLD);
    view.footer_hits.clear();
    for (label, key_start, key_len, action) in hints {
        let start = footer.chars().count();
        let width = label.chars().count();
        footer.push_str(label);
        view.footer_hits.push((
            start as u16,
            (start + width).saturating_sub(1) as u16,
            action,
        ));
        let chars: Vec<char> = label.chars().collect();
        let (head, key_part, tail) = (
            &chars[..key_start],
            &chars[key_start..key_start + key_len],
            &chars[key_start + key_len..],
        );
        hint_spans.push(Span::styled(head.iter().collect::<String>(), muted));
        hint_spans.push(Span::styled(key_part.iter().collect::<String>(), key));
        hint_spans.push(Span::styled(tail.iter().collect::<String>(), muted));
    }
    footer.push_str("q 退出");
    hint_spans.push(Span::styled("q", key));
    hint_spans.push(Span::styled(" 退出", muted));
    let bar = Layout::horizontal([Constraint::Min(0), Constraint::Length(version.len() as u16)])
        .split(parts[1]);
    view.footer_row = Some(parts[1].y);
    view.body = Some(Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: parts[1].y.saturating_sub(area.y),
    });
    frame.render_widget(Paragraph::new(Line::from(hint_spans)), bar[0]);
    frame.render_widget(
        Paragraph::new(version)
            .style(Style::default().fg(MUTED))
            .right_aligned(),
        bar[1],
    );
}

/// The summary and full-width ranking take only the rows they actually need.
/// Charts share the remainder (within one row). Short panes show as many
/// readable charts as fit, rather than wasting space when all three cannot.
fn token_rects(inner: Rect, usage: &UsageStats) -> ([Rect; 4], bool) {
    let content_height = token_panel_lines(
        usage,
        Rect {
            height: u16::MAX,
            ..inner
        },
    )
    .len()
    .min(u16::MAX as usize) as u16;
    let remaining = inner.height.saturating_sub(content_height);
    let chart_count = if usage.error.is_none()
        && (!usage.hours.buckets.is_empty() || !usage.month.buckets.is_empty())
        && inner.width >= token::MIN_CHART_WIDTH
    {
        (remaining / token::MIN_CHART_HEIGHT).min(3)
    } else {
        0
    };
    let charts_drawn = chart_count > 0;
    let top_height = if charts_drawn {
        content_height
    } else {
        inner.height
    };
    let parts = Layout::vertical([
        Constraint::Length(top_height),
        if chart_count >= 1 {
            Constraint::Fill(1)
        } else {
            Constraint::Length(0)
        },
        if chart_count >= 2 {
            Constraint::Fill(1)
        } else {
            Constraint::Length(0)
        },
        if chart_count >= 3 {
            Constraint::Fill(1)
        } else {
            Constraint::Length(0)
        },
    ])
    .split(inner);
    ([parts[0], parts[1], parts[2], parts[3]], charts_drawn)
}

fn sidebar_width(width: u16) -> u16 {
    (width * 2 / 5).clamp(28, 48).min(width / 2)
}

fn panel(frame: &mut Frame, area: Rect, title: &str) -> Rect {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title)
        .title_style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD))
        .border_style(Style::default().fg(CYAN));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn quota_panel_lines(states: &[SourceState], inner: Rect, now: i64) -> Vec<Line<'static>> {
    if inner.width == 0 || inner.height == 0 {
        return vec![];
    }
    let mut lines = quota_lines(states, inner.width, now, true);
    if lines.len() > inner.height as usize {
        lines = quota_lines(states, inner.width, now, false);
    }
    lines
}

fn token_panel_lines(usage: &UsageStats, inner: Rect) -> Vec<Line<'static>> {
    if inner.width == 0 || inner.height == 0 {
        return vec![];
    }
    if usage.error.is_some() {
        return vec![
            Line::styled(" 未连接 usage.db", Style::default().fg(MUTED)),
            Line::styled(" 采集未运行或数据库不存在", Style::default().fg(MUTED)),
        ];
    }
    token::lines(usage, inner.width, inner.height as usize)
}

fn render_panel_lines(
    frame: &mut Frame,
    area: Rect,
    inner: Rect,
    lines: Vec<Line<'static>>,
    scroll: usize,
) {
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(lines.clone()).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        inner,
    );
    let max = lines.len().saturating_sub(inner.height as usize);
    if max > 0 {
        let mut bar = ScrollbarState::new(lines.len())
            .position(scroll)
            .viewport_content_length(inner.height as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_style(Style::default().fg(TRACK))
                .thumb_style(Style::default().fg(MUTED)),
            area,
            &mut bar,
        );
    }
}

fn draw_system(frame: &mut Frame, area: Rect, stats: &SystemStats) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" 系统资源 · 已用 ")
        .title_style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD))
        .border_style(Style::default().fg(MUTED));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);
    metric(
        frame,
        rows[0],
        "CPU",
        stats.cpu.map(|n| n / 100.).unwrap_or(0.),
        stats
            .cpu
            .map(|n| format!("{n:.1}%"))
            .unwrap_or_else(|| "采样中".into()),
        GREEN,
    );
    metric(
        frame,
        rows[1],
        "MEM",
        ratio(stats.memory_used, stats.memory_total),
        format!(
            "{:.1}/{:.1} GiB",
            gib(stats.memory_used),
            gib(stats.memory_total)
        ),
        CYAN,
    );
    metric(
        frame,
        rows[2],
        "SWP",
        ratio(stats.swap_used, stats.swap_total),
        if stats.swap_total == 0 {
            "未启用".into()
        } else {
            format!(
                "{:.1}/{:.1} GiB",
                gib(stats.swap_used),
                gib(stats.swap_total)
            )
        },
        AMBER,
    );
    io_metric(
        frame,
        rows[3],
        "NET",
        "↓",
        stats.network_rx_per_sec,
        "↑",
        stats.network_tx_per_sec,
        CYAN,
    );
    io_metric(
        frame,
        rows[4],
        "DSK",
        "R",
        stats.disk_read_per_sec,
        "W",
        stats.disk_write_per_sec,
        AMBER,
    );
    if rows[5].height > 0 {
        let line = stats
            .error
            .clone()
            .unwrap_or_else(|| format!(" Load {}", stats.load));
        frame.render_widget(
            Paragraph::new(line).style(Style::default().fg(if stats.error.is_some() {
                AMBER
            } else {
                MUTED
            })),
            rows[5],
        );
    }
    if rows[6].height > 0 {
        draw_cpu_history(frame, rows[6], stats);
    }
}

fn draw_cpu_history(frame: &mut Frame, area: Rect, stats: &SystemStats) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let show_axis = !stats.cores.is_empty() && area.width >= 20;
    let (axis, graph) = if show_axis {
        let axis_width = if area.width >= 34 { 12 } else { 9 };
        let cols =
            Layout::horizontal([Constraint::Length(axis_width), Constraint::Min(0)]).split(area);
        (Some(cols[0]), cols[1])
    } else {
        (None, area)
    };

    if let Some(axis) = axis {
        let symbols = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let lines = stats
            .cores
            .iter()
            .take(axis.height as usize)
            .enumerate()
            .map(|(index, value)| {
                let symbol = symbols[((value / 100. * 7.).round().clamp(0., 7.)) as usize];
                let color = if *value > 85. { AMBER } else { GREEN };
                let label = if axis.width >= 11 {
                    format!("CPU{:02} {value:>3.0}% ", index + 1)
                } else {
                    format!("{:02} {value:>3.0}% ", index + 1)
                };
                Line::from(vec![
                    Span::styled(label, Style::default().fg(MUTED)),
                    Span::styled(symbol.to_string(), Style::default().fg(color)),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), axis);
    }

    if graph.width > 0 {
        let history: Vec<u64> = stats
            .cpu_history
            .iter()
            .rev()
            .take(graph.width as usize)
            .rev()
            .copied()
            .collect();
        frame.render_widget(
            Sparkline::default()
                .data(&history)
                .max(100)
                .style(Style::default().fg(CYAN)),
            graph,
        );
    }
}

fn ratio(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.
    } else {
        (used as f64 / total as f64).clamp(0., 1.)
    }
}

fn metric(frame: &mut Frame, area: Rect, title: &str, ratio: f64, label: String, color: Color) {
    if area.height == 0 {
        return;
    }
    let cols = Layout::horizontal([Constraint::Length(5), Constraint::Min(0)]).split(area);
    frame.render_widget(
        Paragraph::new(format!(" {title}")).style(Style::default().fg(MUTED)),
        cols[0],
    );
    let label = Span::styled(label, Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(
        Gauge::default()
            .ratio(ratio.clamp(0., 1.))
            .label(label)
            .gauge_style(Style::default().fg(color).bg(TRACK)),
        cols[1],
    );
    restyle_gauge_label(frame.buffer_mut(), cols[1], color);
}

fn restyle_gauge_label(buf: &mut ratatui::buffer::Buffer, area: Rect, fill: Color) {
    if area.height == 0 {
        return;
    }
    let row = area.top() + area.height / 2;
    for x in area.left()..area.right() {
        let cell = &mut buf[(x, row)];
        if cell.symbol() == "█" || cell.symbol().trim().is_empty() {
            continue;
        }
        let over_fill = cell.bg == fill;
        cell.fg = if over_fill { LIGHT_INK } else { INK };
    }
}

#[allow(clippy::too_many_arguments)]
fn io_metric(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    first_label: &str,
    first: Option<f64>,
    second_label: &str,
    second: Option<f64>,
    color: Color,
) {
    let value = format!(
        " {first_label} {}  {second_label} {}",
        first.map(throughput).unwrap_or_else(|| "---K/s".into()),
        second.map(throughput).unwrap_or_else(|| "---K/s".into())
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {title}"), Style::default().fg(MUTED)),
            Span::styled(value, Style::default().fg(color)),
        ])),
        area,
    );
}

fn throughput(bytes_per_sec: f64) -> String {
    const KIB: f64 = 1024.0;
    let mut value = bytes_per_sec.max(0.0) / KIB;
    let units = ['K', 'M', 'G', 'T'];
    let mut unit = 0;
    while value > 999.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let integer = value.round().clamp(0.0, 999.0) as u16;
    format!("{integer:03}{}/s", units[unit])
}

fn quota_lines(states: &[SourceState], width: u16, now: i64, gaps: bool) -> Vec<Line<'static>> {
    let mut lines = vec![];
    for state in states {
        // 周额度用尽后停轮询等重置：数据不是旧的，不标旧。
        let held = state.hold_until.is_some_and(|t| t > now);
        let stale = state.error.is_some()
            || (!held
                && state.fetched_at.is_some_and(|t| {
                    now.saturating_sub(t).max(0) as u64
                        > state.refresh_interval.saturating_mul(2).as_secs()
                }));
        for card in &state.cards {
            let status = if state.refreshing {
                "刷新中".into()
            } else if let Some(until) = state.hold_until.filter(|t| *t > now) {
                format!("等{}", countdown(until - now))
            } else if let Some(at) = state.fetched_at {
                format!("{}{}", if stale { "旧 " } else { "" }, age(now - at))
            } else {
                "未连接".into()
            };
            let title = format!(" {}", card.title);
            let used = Line::raw(title.clone()).width() + Line::raw(status.clone()).width();
            let padding = (width as usize).saturating_sub(used).max(1);
            lines.push(Line::from(vec![
                Span::styled(
                    title,
                    Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" ".repeat(padding)),
                Span::styled(
                    status,
                    Style::default().fg(if stale { AMBER } else { MUTED }),
                ),
            ]));
            if let Some(error) = &state.error {
                lines.push(Line::styled(
                    format!("  {error}"),
                    Style::default().fg(AMBER),
                ));
            } else if card.meters.is_empty() && card.balance.is_none() && card.note.is_none() {
                lines.push(Line::styled(
                    if state.refreshing {
                        "  正在读取额度…"
                    } else {
                        "  暂无可用额度"
                    },
                    Style::default().fg(MUTED),
                ));
            }
            for meter in &card.meters {
                let expired = meter.expired(now);
                let suffix = if expired {
                    "待刷新".into()
                } else if let Some(at) = meter.resets_at {
                    countdown(at - now)
                } else if meter.available {
                    "可用".into()
                } else {
                    "—".into()
                };
                let value = meter
                    .remaining
                    .map(|v| percent(v, meter.decimals))
                    .unwrap_or_else(|| "—".into());
                let color = if stale || expired {
                    MUTED
                } else {
                    quota_color(meter.remaining)
                };
                let label = format!("  {} ", meter.label);
                let tail = format!(" {value:>6}  {suffix}");
                let reserved = Line::raw(label.clone()).width() + Line::raw(tail.clone()).width();
                let bar_width = (width as usize).saturating_sub(reserved).min(16);
                let mut spans = vec![Span::styled(label, Style::default().fg(MUTED))];
                if bar_width >= 3 {
                    let filled = meter
                        .remaining
                        .map(|v| (v * bar_width as f64 / 100.).floor() as usize)
                        .unwrap_or(0)
                        .min(bar_width);
                    spans.push(Span::styled("━".repeat(filled), Style::default().fg(color)));
                    spans.push(Span::styled(
                        "━".repeat(bar_width - filled),
                        Style::default().fg(TRACK),
                    ));
                }
                spans.push(Span::styled(tail, Style::default().fg(color)));
                lines.push(Line::from(spans));
            }
            if let Some(balance) = card.balance {
                lines.push(Line::from(vec![
                    Span::styled("  余额  ", Style::default().fg(MUTED)),
                    Span::styled(
                        format!("${balance:.2}"),
                        Style::default()
                            .fg(if stale {
                                MUTED
                            } else if balance < 1. {
                                AMBER
                            } else {
                                GREEN
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            if let Some(note) = &card.note {
                lines.push(Line::styled(
                    format!("  {note}"),
                    Style::default().fg(MUTED),
                ));
            }
            if gaps {
                lines.push(Line::raw(""));
            }
        }
    }
    lines
}

fn age(seconds: i64) -> String {
    if seconds < 60 {
        format!("{}s", seconds.max(0))
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3600)
    }
}

pub use crate::model::compact;

/// Use the same terminal-width implementation as Ratatui's renderer.
fn columns(text: &str) -> usize {
    Line::raw(text).width()
}

fn truncate(text: &str, width: usize) -> String {
    if columns(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let target_width = width.saturating_sub(1);
    let mut cut = String::new();
    for character in text.chars() {
        let previous_len = cut.len();
        cut.push(character);
        if columns(&cut) > target_width {
            cut.truncate(previous_len);
            break;
        }
    }
    cut.push('…');
    cut
}

fn percent(value: f64, decimals: u8) -> String {
    let min = decimals.min(2) as usize;
    let trimmed = format!("{value:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned();
    let mut number = trimmed;
    if min > 0 {
        match number.find('.') {
            Some(dot) => {
                let have = number.len() - dot - 1;
                number.extend(std::iter::repeat_n('0', min.saturating_sub(have)));
            }
            None => {
                number.push('.');
                number.extend(std::iter::repeat_n('0', min));
            }
        }
    }
    format!("{number}%")
}

fn quota_color(value: Option<f64>) -> Color {
    match value {
        Some(v) if v <= 10. => RED,
        Some(v) if v <= 25. => AMBER,
        Some(_) => GREEN,
        None => MUTED,
    }
}
