use crate::{
    model::{ModelUsage, SourceState, UsageStats, UsageTotal, countdown},
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

const CYAN: Color = Color::Rgb(11, 93, 107);
const MUTED: Color = Color::Rgb(74, 84, 95);
const GREEN: Color = Color::Rgb(10, 92, 40);
const AMBER: Color = Color::Rgb(124, 67, 0);
const RED: Color = Color::Rgb(163, 22, 22);
const TRACK: Color = Color::Rgb(156, 167, 179);
const INK: Color = Color::Rgb(17, 24, 39);
const LIGHT_INK: Color = Color::Rgb(245, 248, 250);
/// Columns reserved for the histogram Y-axis labels, including the leading
/// space. Sized for the widest label `axis_label` can produce (`999M`).
const AXIS_WIDTH: usize = 6;
/// Model-name columns the ranking keeps before it starts sacrificing value
/// columns; below this a name is unreadable and the columns are worth less.
const NAME_MIN: usize = 10;
/// Terminal columns between the model name and its first value.
const NAME_GAP: usize = 1;
/// Terminal columns between two right-aligned value columns, so `OUT` and
/// `THINK` never read as one word.
const VALUE_GAP: usize = 1;
/// Terminal columns between the model ranking and the summary column, so the
/// last value on the left never runs into the first label on the right.
const BLOCK_GAP: usize = 2;
/// Narrowest token pane that still fits the ranking and the summary side by
/// side. Below it the two stack, each using the full pane.
const SPLIT_MIN_WIDTH: u16 = 64;
/// Widest frame drawn around the historical total: on a very wide summary
/// column an unbounded frame would leave the numbers floating in it.
const TOTAL_BOX_MAX: usize = 34;
/// Fixed Y-axis ceilings for the local-burn charts: the hourly chart against
/// 200M tokens, the daily chart against 2B, and the money chart against $400.
/// Bars are drawn as a share of these, which is also what picks their colour.
const HOURLY_CEILING: f64 = 200_000_000.;
const DAILY_CEILING: f64 = 2_000_000_000.;
const COST_CEILING: f64 = 400.;
/// Bar colours by share of the ceiling: light green with headroom, blue in the
/// middle, dark blue near the top, red at the ceiling.
const BAR_LOW: Color = Color::Rgb(76, 175, 80);
const BAR_MID: Color = Color::Rgb(33, 150, 243);
const BAR_HIGH: Color = Color::Rgb(13, 71, 161);

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
    /// The hints for a footer with `room` usable columns, left to right. Narrow
    /// panes drop the scroll reminder, which only appears when there is
    /// something to scroll.
    /// Each hint is `(label, key start, key length, action)`: `key start` and
    /// `key length` slice the label's chars into the key itself (`r`, `↑↓`),
    /// which `draw` renders in the accent ink so the keyboard part of a hint
    /// stands out from its description.
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
        // Nothing is clickable at this size, so drop the stale regions rather
        // than let a click from the previous layout act on the new one.
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

    // One page, two columns: the machine on the left, the tokens it burned on
    // the right. The cloud quota list shares the left column with the gauges,
    // scrolling beneath them rather than pushing them off screen.
    let columns = Layout::horizontal([
        Constraint::Length(sidebar_width(area.width)),
        Constraint::Min(0),
    ])
    .split(body);
    let desired_system_height = (system.cores.len() as u16)
        .saturating_add(8)
        .max(body.height / 3);
    // Keep room for the quota list under the gauges even on a short pane.
    let system_height = desired_system_height.min(body.height.saturating_sub(8));
    let sidebar =
        Layout::vertical([Constraint::Length(system_height), Constraint::Min(0)]).split(columns[0]);
    draw_system(frame, sidebar[0], system);

    let quota_area = sidebar[1];
    let token_area = columns[1];
    let quota_inner = panel(frame, quota_area, " AI 额度 · 剩余 ");
    let token_inner = panel(frame, token_area, " 本地消耗 · Token (24H) ");

    // The three local-burn charts own the bottom 60% of the token panel, an
    // equal fifth each, and are drawn straight into their rects so they never
    // scroll; the ranking and the summary keep the top 40% and scroll within
    // it. The split is computed in whole rows — four tenths for the list, the
    // rest cut into three exactly equal charts, any leftover row going back
    // to the list — because percentage constraints round each chunk on its
    // own and would hand the three charts unequal heights. A chart needs a
    // title, one bar row, the baseline and its tick row; a pane too short for
    // that hands the whole panel back to the list.
    let inner_h = token_inner.height as usize;
    let chart_h = ((inner_h - inner_h * 4 / 10) / 3) as u16;
    let token_parts = Layout::vertical([
        Constraint::Length(token_inner.height.saturating_sub(3 * chart_h)),
        Constraint::Length(chart_h),
        Constraint::Length(chart_h),
        Constraint::Length(chart_h),
    ])
    .split(token_inner);
    let charts_drawn = chart_h >= 4 && token_parts[1].width >= (AXIS_WIDTH + 3) as u16;
    let token_top = if charts_drawn {
        token_parts[0]
    } else {
        token_inner
    };

    let quota = quota_panel_lines(states, quota_inner, now);
    let token = token_panel_lines(usage, token_top);

    // Both lists share one offset: a wheel or an arrow key moves the page, and
    // neither column needs its own focus. Each clamps to its own end, so the
    // shorter list simply stops while the longer one keeps going. The charts
    // sit outside that bargain: they never scroll, whatever the lists do.
    let quota_max = quota.len().saturating_sub(quota_inner.height as usize);
    let token_max = token.len().saturating_sub(token_top.height as usize);
    view.page_size = (quota_inner.height as usize).max(token_top.height as usize);
    view.max_scroll = quota_max.max(token_max);
    view.scroll = view.scroll.min(view.max_scroll);
    let scroll = view.scroll;
    render_panel_lines(frame, quota_area, quota_inner, quota, scroll.min(quota_max));
    render_panel_lines(frame, token_top, token_top, token, scroll.min(token_max));
    if charts_drawn {
        draw_charts(frame, &token_parts[1..], usage);
    }

    // The footer shares its row with the version stamp, so hints are dropped as
    // the pane narrows. The same table drives both the rendered text and the
    // clickable regions, so they cannot drift apart.
    let room = (area.width as usize).saturating_sub(version.len() + 1);
    let hints = FooterAction::row(room, view.max_scroll > 0);
    let mut footer = String::from(" ");
    let mut hint_spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let muted = Style::default().fg(MUTED);
    let key = Style::default().fg(CYAN).add_modifier(Modifier::BOLD);
    view.footer_hits.clear();
    for (label, key_start, key_len, action) in hints {
        // Hit columns are absolute in the footer row, so account for the lead.
        let start = footer.chars().count();
        // A multi-character glyph is one terminal column here: the labels are
        // ASCII plus `↑↓`, which crossterm reports as single columns.
        let width = label.chars().count();
        footer.push_str(label);
        view.footer_hits.push((
            start as u16,
            (start + width).saturating_sub(1) as u16,
            action,
        ));
        // The label is re-cut around the key so the key itself reads in the
        // accent ink while the description stays muted.
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
    // The exit hint stays keyboard-only: it is always last, so clicking it would
    // be an accidental quit, which is the one action a stray click must not fire.
    footer.push_str("q 退出");
    hint_spans.push(Span::styled("q", key));
    hint_spans.push(Span::styled(" 退出", muted));
    let bar = Layout::horizontal([Constraint::Min(0), Constraint::Length(version.len() as u16)])
        .split(parts[1]);
    // Everything above the footer is the page body, so a click or wheel inside
    // it can be routed to the current page without re-deriving the layout.
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

/// Left-column width: two fifths of the pane, kept in a band where the gauges
/// and the quota bars stay readable, and never more than half so the token
/// panel keeps at least as much room. Below 70 columns the two columns share
/// the pane evenly and both degrade.
fn sidebar_width(width: u16) -> u16 {
    (width * 2 / 5).clamp(28, 48).min(width / 2)
}

/// Draw a bordered panel and return the area inside its border.
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

/// The quota list, with its generous spacing dropped when the pane is short.
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

/// The token list, or the "not connected" notice when the database is missing.
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
    token_lines(usage, inner.width, inner.height as usize)
}

/// Paint one panel's lines at `scroll`, with a scrollbar when they overflow.
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
    // The gauge draws its label with the fill colors swapped, so the glyphs
    // straddling the fill boundary would sit light-on-light or dark-on-dark.
    // Repaint them per column: light ink over the fill, dark ink over the track.
    restyle_gauge_label(frame.buffer_mut(), cols[1], color);
}

fn restyle_gauge_label(buf: &mut ratatui::buffer::Buffer, area: Rect, fill: Color) {
    if area.height == 0 {
        return;
    }
    let row = area.top() + area.height / 2;
    // `Gauge` fills the bar with `█` as well, so only the columns whose glyph
    // changed from the block character carry label text.
    for x in area.left()..area.right() {
        let cell = &mut buf[(x, row)];
        if cell.symbol() == "█" {
            continue;
        }
        if cell.symbol().trim().is_empty() {
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
    if area.height == 0 {
        return;
    }
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
        // 周额度用尽后停轮询等重置：数据不是旧的，不标旧；标题直接显示还要等多久。
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

// ---------------------------------------------------------------------------
// Token panel
// ---------------------------------------------------------------------------

/// The model ranking and the summary column, side by side.
///
/// The top block pairs the ranking (left) with the interval summary and the
/// historical total (right), so the numbers a reader compares sit next to each
/// other. The three histograms are drawn straight into the panel's bottom 60%
/// (see `draw_charts`) and never scroll, so this block owns the top 40% alone.
/// When that share cannot hold the block the model count gives way first —
/// the total is the anchor and is the last thing to give way.
fn token_lines(usage: &UsageStats, width: u16, height: usize) -> Vec<Line<'static>> {
    let top = |models: usize| -> Vec<Line<'static>> {
        match top_block_widths(width) {
            Some((left, right)) => {
                let table = (models > 0).then(|| {
                    model_table(&usage.models, left.saturating_sub(BLOCK_GAP as u16), models)
                });
                join_columns(
                    table.unwrap_or_default(),
                    stats_column(usage, right),
                    left as usize,
                )
            }
            // Too narrow for two columns: the ranking spans the pane and the
            // summary follows it, each keeping its full width.
            None => {
                let mut lines = vec![];
                if models > 0 {
                    lines.extend(model_table(&usage.models, width, models));
                    lines.push(Line::raw(""));
                }
                lines.extend(stats_column(usage, width));
                lines
            }
        }
    };
    for models in [6usize, 4, 3, 2, 1, 0] {
        let lines = top(models);
        if lines.len() <= height.max(1) {
            return lines;
        }
    }
    // Even the summary column overflows: keep the total, the one figure the pane
    // exists for, and let the pane scroll.
    total_card(&usage.all_total, width as usize)
}

/// Widths of the top block's two columns: the model ranking takes 70% of the
/// pane and the summary column the remaining 30%. `None` below
/// `SPLIT_MIN_WIDTH`, where both would be too cramped to read and the two
/// stack instead.
fn top_block_widths(width: u16) -> Option<(u16, u16)> {
    (width >= SPLIT_MIN_WIDTH).then(|| {
        let left = width * 7 / 10;
        (left, width - left)
    })
}

/// Paste two line blocks side by side. The left block is padded to exactly
/// `left_width` terminal columns, so every right-hand line starts in the same
/// column; a block that runs out of rows simply leaves blanks.
fn join_columns(
    left: Vec<Line<'static>>,
    right: Vec<Line<'static>>,
    left_width: usize,
) -> Vec<Line<'static>> {
    let rows = left.len().max(right.len());
    (0..rows)
        .map(|row| {
            let mut spans: Vec<Span<'static>> = left
                .get(row)
                .map(|line| line.spans.clone())
                .unwrap_or_default();
            let used: usize = spans.iter().map(|span| columns(&span.content)).sum();
            if used < left_width {
                spans.push(Span::raw(" ".repeat(left_width - used)));
            }
            if let Some(line) = right.get(row) {
                spans.extend(line.spans.iter().cloned());
            }
            Line::from(spans)
        })
        .collect()
}

fn model_table(models: &[ModelUsage], width: u16, limit: usize) -> Vec<Line<'static>> {
    if models.is_empty() {
        return vec![Line::from(Span::styled(
            " 暂无用量记录",
            Style::default().fg(MUTED),
        ))];
    }
    let taken: Vec<&ModelUsage> = models.iter().take(limit).collect();
    // One row per model: the name opens the row and the values follow in fixed
    // slots, so a reader reads across instead of pairing two rows. The name
    // column takes whatever the value columns leave; the IN slot must hold the
    // widest cell shown (the hit ratio rides inside it), the rest are
    // fixed-width numbers. Narrow panes drop columns from the right (`COST`,
    // then `合计`, then `THINK`) rather than shrinking the name past
    // `NAME_MIN`; `IN` and `OUT` always survive.
    let available = width as usize;
    let mut cols: Vec<(&str, usize)> = vec![
        (
            "IN",
            taken
                .iter()
                .map(|m| columns(&m.input_display()))
                .max()
                .unwrap_or(2)
                .max(2),
        ),
        ("OUT", 5),
        ("THINK", 5),
        ("合计", 6),
        ("COST", 7),
    ];
    // A value column's width is the width of its widest cell; `VALUE_GAP`
    // separates the right-aligned columns so two numbers never run together
    // (`OUTTHINK`), and the name keeps `NAME_GAP` before the first value.
    let numeric_width = |cols: &[(&str, usize)]| {
        cols.iter().map(|c| c.1).sum::<usize>() + cols.len().saturating_sub(1) * VALUE_GAP
    };
    while cols.len() > 2 && (NAME_MIN + NAME_GAP + numeric_width(&cols)) > available {
        cols.pop();
    }
    // The single gutter between the name and the first value column: every
    // value column is right-aligned in its slot, so the name never touches a
    // number.
    let name_width = available.saturating_sub(numeric_width(&cols) + NAME_GAP);
    // A row is the name, padded to the name column plus its gutter, then each
    // value right-aligned in its own slot and separated by a gap. `slack` is
    // measured in terminal columns, so the wide `合计` label lines up with its
    // value.
    let row = |name: (&str, Color), cells: Vec<(String, Color)>| -> Line<'static> {
        let count = cells.len();
        let mut spans = vec![Span::styled(
            format!(
                "{}{}",
                name.0,
                " ".repeat((name_width + NAME_GAP).saturating_sub(columns(name.0)))
            ),
            Style::default().fg(name.1),
        )];
        for (index, (text, color)) in cells.into_iter().enumerate() {
            let slack = cols[index].1.saturating_sub(columns(&text));
            spans.push(Span::styled(
                format!("{}{}", " ".repeat(slack), text),
                Style::default().fg(color),
            ));
            if index + 1 < count {
                spans.push(Span::raw(" ".repeat(VALUE_GAP)));
            }
        }
        Line::from(spans)
    };
    let mut lines = vec![row(
        ("模型", MUTED),
        cols.iter()
            .map(|(label, _)| (label.to_string(), MUTED))
            .collect(),
    )];
    for model in &taken {
        let cost = match model.cost {
            Some(c) => (money(c), GREEN),
            None => ("—".into(), MUTED),
        };
        lines.push(row(
            (&truncate(&model.model, name_width), CYAN),
            vec![
                (model.input_display(), INK),
                (compact(model.output), INK),
                (compact(model.reasoning), MUTED),
                (compact(model.total_tokens()), INK),
                (cost.0, cost.1),
            ]
            .into_iter()
            .take(cols.len())
            .collect(),
        ));
    }
    lines
}

/// What a chart plots: the token series, or the money series over the same
/// buckets. Both are held by reference so a frame never copies them.
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

    /// One bucket's value in the chart's own unit (tokens or dollars).
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

/// One local-burn chart: what it plots, the fixed ceiling it is drawn against,
/// and the X-axis scale beneath it.
struct Chart<'a> {
    /// Panel label, e.g. `每小时`.
    label: &'static str,
    series: Series<'a>,
    /// Bars are drawn as a share of this, and coloured by the same share, so
    /// height and colour always agree on how full a bucket is.
    ceiling: f64,
    /// One bar's span in seconds before any merging.
    span_seconds: u64,
    /// Base buckets per X-axis tick: four quarter hours make an hour, four six-
    /// hour blocks make a day.
    tick_buckets: usize,
    /// Value the leftmost tick names — hour 0, or day 1.
    first_tick: u32,
}

/// A bar's colour by its share of the chart ceiling: light green with headroom,
/// blue in the middle, dark blue near the top and red at the ceiling.
fn bar_color(share: f64) -> Color {
    if share >= 0.95 {
        RED
    } else if share >= 0.80 {
        BAR_HIGH
    } else if share >= 0.30 {
        BAR_MID
    } else {
        BAR_LOW
    }
}

/// `15分钟`, `6小时`, `1天`: what one drawn bar covers once buckets merge.
fn span_label(seconds: u64) -> String {
    match seconds {
        s if s % 86_400 == 0 => format!("{}天", s / 86_400),
        s if s % 3_600 == 0 => format!("{}小时", s / 3_600),
        s if s % 60 == 0 => format!("{}分钟", s / 60),
        s => format!("{s}秒"),
    }
}

/// The three local-burn charts, largest interval first: the hourly chart, the
/// daily chart and the money chart over the same six-hour buckets as the
/// daily one. Each is drawn straight into its own rect, so the charts never
/// scroll and always keep the equal shares the layout gave them.
fn local_charts(usage: &UsageStats) -> [Chart<'_>; 3] {
    [
        Chart {
            label: "每小时",
            series: Series::Tokens(&usage.hours.buckets),
            ceiling: HOURLY_CEILING,
            span_seconds: 15 * 60,
            tick_buckets: 4,
            first_tick: 0,
        },
        Chart {
            label: "每天",
            series: Series::Tokens(&usage.month.buckets),
            ceiling: DAILY_CEILING,
            span_seconds: 6 * 3_600,
            tick_buckets: 4,
            first_tick: 1,
        },
        Chart {
            label: "金额",
            series: Series::Money(&usage.month.costs),
            ceiling: COST_CEILING,
            span_seconds: 6 * 3_600,
            tick_buckets: 4,
            first_tick: 1,
        },
    ]
}

/// Render the three local-burn charts into the panel's bottom three rects.
/// A chart whose series is missing draws nothing; the layout's equal shares
/// and the blank tail row of each block keep the three apart.
fn draw_charts(frame: &mut Frame, areas: &[Rect], usage: &UsageStats) {
    for (chart, area) in local_charts(usage).into_iter().zip(areas.iter()) {
        let lines = histogram(&chart, area.width, chart_rows(area.height));
        if !lines.is_empty() {
            frame.render_widget(Paragraph::new(lines), *area);
        }
    }
}

/// Bar rows a chart gets in a rect `room` rows tall: the title, the baseline
/// and the tick row take three, and the rect's last row stays blank as the
/// gap to the next chart. The count snaps to 8, 5, 4 or 2 so the row
/// boundaries land on round shares of the fixed ceiling — quarters, fifths,
/// halves — and every Y-axis label reads as a round number, never `188M`.
fn chart_rows(room: u16) -> usize {
    let available = room.saturating_sub(4) as usize;
    for nice in [8usize, 5, 4, 2] {
        if available >= nice {
            return nice;
        }
    }
    available.min(1)
}

/// Suffix and divisor for the axis scale that keeps the labels shortest:
/// `1.2B` reads better than `1234M`, but `900M` reads better than `0.9B`.
fn axis_scale(max: f64) -> (f64, &'static str) {
    if max >= 1_000_000_000. {
        (1e9, "B")
    } else if max >= 1_000_000. {
        (1e6, "M")
    } else if max >= 1_000. {
        (1e3, "K")
    } else {
        (1., "")
    }
}

/// An axis label in the chosen scale: one decimal below ten, integers above, so
/// the widest label any scale produces fits `AXIS_WIDTH`.
fn axis_label(value: f64, divisor: f64, suffix: &str) -> String {
    if value == 0.0 || value.abs() < f64::EPSILON {
        return "0".to_string();
    }
    let scaled = value / divisor;
    if suffix.is_empty() {
        format!("{scaled:.0}")
    } else if (scaled - scaled.round()).abs() < 1e-6 || scaled >= 10. {
        format!("{scaled:.0}{suffix}")
    } else {
        format!("{scaled:.1}{suffix}")
    }
}

/// Draw one chart: bar rows against a labelled Y axis, then a baseline carrying
/// the X-axis ticks and the numbered scale beneath it.
///
/// Buckets are one column wide, so a chart with more buckets than columns — 96
/// quarter hours on a narrow pane — sums adjacent buckets until they fit: the
/// shape stays honest and the plot always reaches the pane edge.
///
/// The Y axis ticks at row boundaries, so every graded label names the value a
/// bar holds when it fills its row and everything below it: the top label is
/// the ceiling, the baseline is zero, and a tick with no number leaves nothing
/// to judge by.
fn histogram(chart: &Chart, width: u16, rows: usize) -> Vec<Line<'static>> {
    let base_len = chart.series.len();
    // The plot is what is left of the pane after the axis gutter (`AXIS_WIDTH`
    // columns), its trailing space and the `┤` the bars stand against: the last
    // bar column must land on the pane's edge instead of being clipped by it.
    let plot = (width as usize).saturating_sub(AXIS_WIDTH + 2);
    if base_len == 0 || plot == 0 {
        return vec![];
    }
    // Merge until the bucket count fits the plot.
    let mut factor = 1usize;
    while base_len.div_ceil(factor) > plot {
        factor += 1;
    }
    let merged: Vec<f64> = (0..base_len.div_ceil(factor))
        .map(|index| {
            let start = index * factor;
            let end = (start + factor).min(base_len);
            (start..end).map(|bucket| chart.series.value(bucket)).sum()
        })
        .collect();
    let max = chart.ceiling;
    let (divisor, suffix) = axis_scale(max);
    let label_of = |value: f64| {
        if chart.series.money() {
            format!("${}", value.round() as i64)
        } else {
            axis_label(value, divisor, suffix)
        }
    };
    let height = rows.max(1);
    // The glyph grid: `height` rows of eight sub-rows each.
    let total_subrows = height * 8;
    let eighths: Vec<usize> = merged
        .iter()
        .map(|value| {
            if max <= 0.0 || *value <= 0.0 {
                0
            } else {
                (((*value / max) * total_subrows as f64).round() as usize).min(total_subrows)
            }
        })
        .collect();
    let colours: Vec<Color> = merged
        .iter()
        .map(|value| bar_color(if max > 0.0 { value / max } else { 0.0 }))
        .collect();
    // One rhythm across the row: every bar gets the same width and the leftover
    // columns are shared out as gaps. Letting bars absorb the remainder (some
    // one cell wide, the next two) made the spacing — and with it every hourly
    // tick — drift in and out, which is what read as uneven.
    //
    // A whole-cell gap needs two cells per bar. When the plot is too narrow
    // for that the bars shrink to one cell and lose their gap entirely; the
    // body stays the full block rather than a three-quarter block, whose
    // leftover sliver repeated down every bar is what reads as a bristly
    // edge. Adjacent full blocks tile the plot cleanly.
    let columns = merged.len().max(1);
    let pitch = plot / columns;
    let (bar_width, body) = if pitch >= 2 {
        (pitch - 1, '█')
    } else {
        (1, '█')
    };
    let gaps = columns.saturating_sub(1);
    let gap_cells = plot.saturating_sub(columns * bar_width);
    let gap_after = |index: usize| {
        if gaps == 0 {
            return 0;
        }
        (index + 1) * gap_cells / gaps - index * gap_cells / gaps
    };
    // Where each bar starts, and the centre a tick names it by.
    let mut starts = Vec::with_capacity(columns);
    let mut cursor = 0usize;
    for index in 0..columns {
        starts.push(cursor);
        cursor += bar_width;
        if index < gaps {
            cursor += gap_after(index);
        }
    }
    let trailing = plot.saturating_sub(cursor);
    let mut ticks: Vec<(usize, String)> = vec![];
    for unit in 0..base_len / chart.tick_buckets {
        let merged_index = unit * chart.tick_buckets / factor;
        let start = starts.get(merged_index).copied().unwrap_or(0);
        ticks.push((
            start + bar_width / 2,
            format!("{}", chart.first_tick as usize + unit),
        ));
    }

    // The value a bar holds when its top reaches a sub-row, so a tick can name
    // the height it marks instead of standing there unlabelled.
    let value_at = |subrows: usize| max * subrows as f64 / total_subrows as f64;
    let mut out: Vec<Line<'static>> = Vec::with_capacity(height + 3);

    let title = format!(
        " {} · 每柱 {}",
        chart.label,
        span_label(chart.span_seconds * factor as u64)
    );
    // A title longer than the pane would be clipped mid-glyph at the border;
    // cutting it here keeps the clip deliberate and marked.
    out.push(Line::styled(
        truncate(&title, width as usize),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    ));

    for row in 0..height {
        // Sub-rows still available to this row, counted from the baseline up.
        let base = (height - 1 - row) * 8;
        // `chart_rows` snaps the row count to 8, 5, 4 or 2, so every boundary
        // lands on a round share of the ceiling and labelling each of them
        // keeps every number worth reading — quarters, fifths, halves, never
        // `188M`. Tall charts grade to every other boundary so the labels
        // never crowd.
        let step = if height <= 5 { 1 } else { 2 };
        let graded = row % step == 0;
        let head = if max > 0.0 && (row == 0 || graded) {
            // The value at this row's *top* boundary, so the topmost label is
            // the ceiling and no graded row repeats the baseline's zero.
            label_of(value_at((height - row) * 8))
        } else {
            String::new()
        };
        let mut spans = vec![
            Span::styled(
                format!(" {head:>pad$} ", pad = AXIS_WIDTH - 1),
                Style::default().fg(MUTED),
            ),
            Span::styled("┤", Style::default().fg(TRACK)),
        ];
        for (index, filled) in eighths.iter().enumerate() {
            let remaining = filled.saturating_sub(base);
            let used = remaining.min(8);
            // Full cells are solid; the topmost partial cell keeps the
            // eighth-block glyph, which is the only way to show a bar whose top
            // does not land on a cell boundary.
            let glyph = match used {
                0 => ' ',
                8 => body,
                n => ['▁', '▂', '▃', '▄', '▅', '▆', '▇'][n - 1],
            };
            let colour = if glyph == ' ' { TRACK } else { colours[index] };
            // The bar repeats the glyph across its cells; the gap columns that
            // follow belong to the next bar's spacing, not to this bar.
            spans.push(Span::styled(
                glyph.to_string().repeat(bar_width),
                Style::default().fg(colour),
            ));
            let gap = if index < gaps { gap_after(index) } else { 0 };
            if gap > 0 {
                spans.push(Span::styled(" ".repeat(gap), Style::default().fg(TRACK)));
            }
        }
        if trailing > 0 {
            spans.push(Span::styled(
                " ".repeat(trailing),
                Style::default().fg(TRACK),
            ));
        }
        out.push(Line::from(spans));
    }
    // Baseline: a zero label, the corner under the axis, then the rule the bars
    // stand on, with a tick under every hour (or day) boundary.
    let mut rule = vec!['─'; plot];
    for (column, _) in &ticks {
        if let Some(cell) = rule.get_mut(*column) {
            *cell = '┴';
        }
    }
    out.push(Line::from(vec![
        Span::styled(
            format!(" {:>pad$} ", label_of(0.0), pad = AXIS_WIDTH - 1),
            Style::default().fg(MUTED),
        ),
        Span::styled("└", Style::default().fg(TRACK)),
        Span::styled(
            rule.into_iter().collect::<String>(),
            Style::default().fg(TRACK),
        ),
    ]));
    out.push(tick_labels(&ticks, plot));
    out
}

/// The numbered X-axis scale under a chart's baseline.
///
/// Each number is centred under the mark it names. A number that would collide
/// with the one before it is dropped — a half-overwritten tick is worse than a
/// missing one — and the last number is pinned to the plot edge when centring
/// it would run it past, so the axis still reads to its end.
fn tick_labels(ticks: &[(usize, String)], plot: usize) -> Line<'static> {
    let mut row = String::new();
    let mut cursor = 0usize;
    let mut placed_last = false;
    for (index, (centre, text)) in ticks.iter().enumerate() {
        let start = centre
            .saturating_sub(text.len() / 2)
            .min(plot.saturating_sub(text.len()));
        if start < cursor {
            continue;
        }
        if row.len() < start {
            row.push_str(&" ".repeat(start - row.len()));
        }
        row.push_str(text);
        cursor = start + text.len() + 1;
        placed_last = index + 1 == ticks.len();
    }
    if !placed_last && let Some((_, text)) = ticks.last() {
        let start = plot.saturating_sub(text.len());
        if start >= cursor {
            if row.len() < start {
                row.push_str(&" ".repeat(start - row.len()));
            }
            row.push_str(text);
        }
    }
    Line::styled(
        format!("{:width$}{row}", "", width = AXIS_WIDTH + 1),
        Style::default().fg(MUTED),
    )
}

/// The summary column: the three calendar intervals, then the historical total
/// as a framed headline.
///
/// Every row here is built to fit the column width the top block hands it, so
/// the numbers never spill into the model ranking on their left.
fn stats_column(usage: &UsageStats, width: u16) -> Vec<Line<'static>> {
    let room = width as usize;
    let rows = [
        ("当日", &usage.day_total),
        ("本周", &usage.week_total),
        ("当月", &usage.month_total),
    ];
    let label_width = rows
        .iter()
        .map(|(label, _)| columns(&format!(" {label}")))
        .max()
        .unwrap_or(0);
    // One unit word is shared by all three rows so their columns line up: the
    // full word when the pane allows it, `tok` when it is tight, nothing when
    // the numbers must stand alone.
    let (unit, amount_width, cost_width) = ["tokens", "tok", ""]
        .into_iter()
        .map(|unit| {
            let amount = rows
                .iter()
                .map(|(_, total)| columns(&amount_text(total.tokens, unit)))
                .max()
                .unwrap_or(0);
            let cost = rows
                .iter()
                .map(|(_, total)| money(total.cost).len())
                .max()
                .unwrap_or(0);
            (unit, amount, cost)
        })
        .find(|(_, amount, cost)| label_width + 1 + amount + 2 + cost <= room)
        .unwrap_or(("", 0, 0));
    // The interval rows and the total share one block: the rows stretch to
    // the block's width so their right edge and the total's rule line up.
    let block_width = room.min(TOTAL_BOX_MAX);
    let natural = label_width + 1 + amount_width + 2 + cost_width;
    let amount_width = amount_width + block_width.saturating_sub(natural);
    let mut lines = vec![];
    for (label, total) in rows {
        lines.extend(interval_card(
            label,
            total,
            label_width,
            amount_width,
            cost_width,
            unit,
            room,
        ));
    }
    lines.push(Line::raw(""));
    lines.extend(total_card(&usage.all_total, block_width));
    lines
}

/// One interval: the label, its token count and its money, right-aligned in the
/// column's slots. The money drops to its own row when the pane is too narrow
/// for it to share.
fn interval_card(
    label: &str,
    total: &UsageTotal,
    label_width: usize,
    amount_width: usize,
    cost_width: usize,
    unit: &str,
    room: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![];
    let amount = amount_text(total.tokens, unit);
    let cost = money(total.cost);
    // The label is padded rather than right-aligned, so `当日` and `当月` share
    // a left edge and their numbers share a right edge.
    let label = format!(" {label}");
    let head = format!(
        "{label}{}",
        " ".repeat(label_width.saturating_sub(columns(&label)))
    );
    let row = format!("{head} {amount:>amount_width$}  {cost:>cost_width$}");
    if columns(&row) <= room {
        lines.push(Line::from(vec![
            Span::styled(head, Style::default().fg(MUTED)),
            Span::raw(" "),
            Span::styled(format!("{amount:>amount_width$}"), Style::default().fg(INK)),
            Span::raw("  "),
            Span::styled(format!("{cost:>cost_width$}"), Style::default().fg(INK)),
        ]));
    } else {
        // Too narrow for the money to share the row: give it its own row under
        // the count instead of clipping it away.
        lines.push(Line::from(vec![
            Span::styled(head.clone(), Style::default().fg(MUTED)),
            Span::raw(" "),
            Span::styled(
                truncate(&amount, room.saturating_sub(columns(&head) + 1)),
                Style::default().fg(INK),
            ),
        ]));
        if columns(&cost) <= room {
            lines.push(Line::from(Span::styled(
                format!(" {cost}"),
                Style::default().fg(INK),
            )));
        }
    }
    lines
}

/// The historical total, separated from the intervals by a thin rule rather
/// than a frame, so it reads as the last row of the summary column instead of
/// a card of its own. The block shrinks with the column and is dropped
/// entirely when the pane cannot hold it.
fn total_card(total: &UsageTotal, room: usize) -> Vec<Line<'static>> {
    let cost = money(total.cost);
    // The frame is capped so a very wide column does not leave the numbers
    // floating inside it; it never exceeds the column it sits in. A column
    // narrower than the smallest frame falls through to the unframed total.
    let box_width = room.min(TOTAL_BOX_MAX);
    // The widest frame buildable: the full unit word first, then the short one,
    // then none.
    let framed = ["tokens", "tok", ""]
        .into_iter()
        .map(|unit| amount_text(total.tokens, unit))
        .find(|amount| {
            box_width
                >= 2 + [columns("总计"), columns(amount), columns(&cost)]
                    .into_iter()
                    .max()
                    .unwrap_or(0)
        });
    let Some(amount) = framed else {
        // Narrower than a frame: keep the label and numbers, unframed.
        let headline = Style::default().fg(CYAN).add_modifier(Modifier::BOLD);
        let mut lines = vec![Line::from(Span::styled(" 总计", headline))];
        lines.push(Line::from(Span::styled(
            format!(" {}", amount_text(total.tokens, "")),
            headline,
        )));
        lines.push(Line::from(Span::styled(format!(" {cost}"), headline)));
        return lines;
    };
    let inner = box_width - 2;
    // A single thin rule in the muted track separates the total from the
    // intervals above; the rows share the intervals' left edge and need no
    // frame to hold them together. The frame is what made the total read as
    // a card of its own — a separate visual object competing with the panel
    // it sits in — so it is dropped in favour of the rule.
    let rule = Style::default().fg(TRACK);
    let label = Style::default().fg(CYAN).add_modifier(Modifier::BOLD);
    let value = Style::default().fg(INK).add_modifier(Modifier::BOLD);
    let cost_style = Style::default().fg(INK);
    let mut lines = vec![Line::from(vec![
        Span::raw(" "),
        Span::styled("─".repeat(inner), rule),
    ])];
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled("总计", label),
        Span::raw(" ".repeat(inner.saturating_sub(columns("总计") + 1))),
    ]));
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(amount.clone(), value),
        Span::raw(" ".repeat(inner.saturating_sub(columns(&amount) + 1))),
    ]));
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(cost.clone(), cost_style),
        Span::raw(" ".repeat(inner.saturating_sub(columns(&cost) + 1))),
    ]));
    lines
}
/// A token count with its unit word, e.g. `126.3B tokens`.
fn amount_text(tokens: u64, unit: &str) -> String {
    if unit.is_empty() {
        compact(tokens)
    } else {
        format!("{} {unit}", compact(tokens))
    }
}

pub use crate::model::compact;

/// Money, keeping cents visible until they stop mattering.
fn money(value: f64) -> String {
    if value >= 1_000. {
        format!("${:.0}", value)
    } else if value >= 100. {
        format!("${:.1}", value)
    } else {
        format!("${:.2}", value)
    }
}
/// Terminal columns a string occupies: East-Asian wide glyphs count as two.
fn columns(text: &str) -> usize {
    text.chars()
        .map(|character| if is_wide_char(character) { 2 } else { 1 })
        .sum()
}

/// Whether a character occupies two terminal columns.
fn is_wide_char(character: char) -> bool {
    matches!(character as u32,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFD)
}

/// Cut a model name to `width`, marking the cut so it does not read as the whole
/// name.
fn truncate(text: &str, width: usize) -> String {
    if columns(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let target_width = width.saturating_sub(1);
    let mut cut = String::new();
    let mut current_cols = 0;
    for c in text.chars() {
        let char_cols = if is_wide_char(c) { 2 } else { 1 };
        if current_cols + char_cols > target_width {
            break;
        }
        cut.push(c);
        current_cols += char_cols;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Card, Meter, Source};
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::Instant;

    // The dashboard runs on white terminals, so every ink must clear WCAG AA
    // (4.5:1) against the panel background and stay readable on the gauge
    // track. Colors are concrete RGB so this guard is exact.
    const WHITE: (u8, u8, u8) = (255, 255, 255);
    const INK_RGB: (u8, u8, u8) = (17, 24, 39);
    const LIGHT_INK_RGB: (u8, u8, u8) = (245, 248, 250);
    const TRACK_RGB: (u8, u8, u8) = (156, 167, 179);

    fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (hi, lo) = (
            relative_luminance(a).max(relative_luminance(b)),
            relative_luminance(a).min(relative_luminance(b)),
        );
        (hi + 0.05) / (lo + 0.05)
    }

    fn rgb(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("expected a concrete RGB ink, got {other:?}"),
        }
    }

    #[test]
    fn inks_clear_wcag_aa_on_a_white_terminal() {
        for (name, color) in [
            ("CYAN", CYAN),
            ("MUTED", MUTED),
            ("GREEN", GREEN),
            ("AMBER", AMBER),
            ("RED", RED),
        ] {
            let ratio = contrast(rgb(color), WHITE);
            assert!(
                ratio >= 4.5,
                "{name} reads {ratio:.2}:1 on white, below the 4.5:1 AA floor"
            );
        }
    }

    #[test]
    fn gauge_fill_and_label_stay_readable_on_the_track() {
        for (name, color) in [("CYAN", CYAN), ("GREEN", GREEN), ("AMBER", AMBER)] {
            let fill = rgb(color);
            assert!(
                contrast(fill, TRACK_RGB) >= 3.0,
                "{name} fill is only {:.2}:1 against the track",
                contrast(fill, TRACK_RGB)
            );
            assert!(
                contrast(INK_RGB, TRACK_RGB) >= 4.5,
                "label ink is only {:.2}:1 against the track",
                contrast(INK_RGB, TRACK_RGB)
            );
            assert!(
                contrast(LIGHT_INK_RGB, fill) >= 4.5,
                "label ink is only {:.2}:1 against the {name} fill",
                contrast(LIGHT_INK_RGB, fill)
            );
        }
    }

    #[test]
    fn gauge_label_is_inked_per_column_across_the_fill_boundary() {
        let stats = SystemStats {
            cpu: Some(63.0),
            memory_used: 9_000_000_000,
            memory_total: 20_000_000_000,
            ..SystemStats::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(44, 20)).unwrap();
        let mut view = View::default();
        terminal
            .draw(|f| draw(f, &stats, &[], &UsageStats::default(), &mut view, 100))
            .unwrap();
        let buffer = terminal.backend().buffer();
        // CPU is the first gauge row; its 63% label straddles the fill edge.
        let row = 1;
        let label_cells = (0..buffer.area.width)
            .map(|x| &buffer[(x, row)])
            .filter(|cell| cell.symbol() != "█" && !cell.symbol().trim().is_empty())
            .filter(|cell| cell.fg == INK || cell.fg == LIGHT_INK)
            .count();
        assert!(
            label_cells >= 5,
            "expected the gauge label to be re-inked, found {label_cells} cells"
        );
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "█" {
                    assert_ne!(
                        cell.fg, LIGHT_INK,
                        "bar block at ({x}, {y}) was repainted as label ink"
                    );
                }
            }
        }
    }

    #[test]
    fn all_terminal_sizes_render_and_scrolling_is_bounded() {
        let states: Vec<_> = Source::ALL
            .into_iter()
            .map(|source| SourceState::new(source, std::time::Duration::from_secs(60)))
            .collect();
        for (width, height) in [
            (1, 1),
            (20, 10),
            (26, 12),
            (32, 24),
            (40, 40),
            (52, 48),
            (80, 60),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut view = View {
                scroll: usize::MAX,
                ..View::default()
            };
            terminal
                .draw(|f| {
                    draw(
                        f,
                        &SystemStats::default(),
                        &states,
                        &UsageStats::default(),
                        &mut view,
                        100,
                    )
                })
                .unwrap();
            if width >= 26 && height >= 12 {
                assert!(view.scroll <= view.max_scroll);
            }
        }
    }
    #[test]
    fn low_monthly_balance_and_reset_wait_are_visible() {
        let mut state = SourceState::new(Source::Go, std::time::Duration::from_secs(60));
        state.begin("a".into());
        state.finish(
            Ok(vec![Card {
                meters: vec![Meter::from_used("月", 98., Some(99), 0).unwrap()],
                ..Card::empty("OpenCode Go")
            }]),
            98,
            Instant::now(),
        );
        let text = quota_lines(&[state], 40, 100, false)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("2%"));
        assert!(text.contains("待刷新"));
        assert!(!text.contains("100%"));
    }
    #[test]
    fn exhausted_weekly_source_shows_wait_instead_of_stale() {
        let mut state = SourceState::new(Source::Agy, std::time::Duration::from_secs(60));
        state.begin("a".into());
        state.finish(
            Ok(vec![Card {
                meters: vec![Meter::from_used("周", 100., Some(100 + 2 * 86400), 1).unwrap()],
                ..Card::empty("AGY")
            }]),
            100,
            Instant::now(),
        );
        // 距上次成功已 300 秒：普通来源此时标旧，等重置的不标旧，直接显示等待。
        let text = quota_lines(&[state], 40, 100 + 300, false)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("等1d 23h"));
        assert!(!text.contains("旧"));
    }
    #[test]
    fn percentage_keeps_near_full_precision() {
        assert_eq!(percent(99.97, 0), "99.97%");
        assert_eq!(percent(100., 0), "100%");
        assert_eq!(percent(0., 0), "0%");
    }

    #[test]
    fn percentage_keeps_official_decimal_places() {
        // 有小数位的最少保留该位数；无小数的保持整数。
        assert_eq!(percent(100., 1), "100.0%");
        assert_eq!(percent(68.8, 1), "68.8%");
        assert_eq!(percent(0., 1), "0.0%");
        assert_eq!(percent(67.24, 2), "67.24%");
        assert_eq!(percent(82., 0), "82%");
    }

    #[test]
    fn footer_shows_version_at_bottom_right() {
        let states: Vec<_> = Source::ALL
            .into_iter()
            .map(|source| SourceState::new(source, std::time::Duration::from_secs(60)))
            .collect();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut view = View::default();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &SystemStats::default(),
                    &states,
                    &UsageStats::default(),
                    &mut view,
                    100,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let last = buffer.area.height - 1;
        let line: String = (0..buffer.area.width)
            .map(|x| buffer[(x, last)].symbol())
            .collect();
        let version = concat!("v", env!("CARGO_PKG_VERSION"));
        assert!(
            line.trim_end().ends_with(version),
            "expected the footer to end with {version}, got {line:?}"
        );
    }

    #[test]
    fn the_readme_changelog_matches_the_crate_version() {
        // `Cargo.toml` is the single source of truth: the footer, the HTTP user
        // agent and this changelog heading all read the same number, so a bump
        // that forgets the README fails here instead of drifting silently.
        let readme = include_str!("../README.md");
        let expected = format!("## {} 更新", env!("CARGO_PKG_VERSION"));
        let first = readme
            .lines()
            .find(|line| line.starts_with("## ") && line.ends_with(" 更新"))
            .unwrap_or_else(|| panic!("no changelog heading in README.md"));
        assert_eq!(
            first.trim(),
            expected,
            "README's newest changelog section must match Cargo.toml's version"
        );
        assert!(
            readme.contains(concat!("`v", env!("CARGO_PKG_VERSION"), "`")),
            "README must reference the released tag"
        );
    }

    #[test]
    fn throughput_is_fixed_width_and_promotes_after_999() {
        assert_eq!(throughput(0.0), "000K/s");
        assert_eq!(throughput(72.4 * 1024.0), "072K/s");
        assert_eq!(throughput(999.0 * 1024.0), "999K/s");
        assert_eq!(throughput(999.1 * 1024.0), "001M/s");
        assert_eq!(throughput(999.1 * 1024.0 * 1024.0), "001G/s");
        assert_eq!(throughput(999.1 * 1024.0 * 1024.0 * 1024.0), "001T/s");
        for value in [0.0, 1024.0, 999.1 * 1024.0, 2.0 * 1024.0 * 1024.0] {
            assert_eq!(throughput(value).len(), 6);
        }
    }

    #[test]
    fn twelve_logical_cpus_form_a_vertical_axis_beside_history() {
        let states: Vec<_> = Source::ALL
            .into_iter()
            .map(|source| SourceState::new(source, std::time::Duration::from_secs(60)))
            .collect();
        let stats = SystemStats {
            cpu: Some(32.0),
            cores: (1..=12).map(|n| n as f64 * 7.0).collect(),
            cpu_history: [10, 30, 80, 40].into(),
            ..SystemStats::default()
        };
        // 110 columns give the sidebar a 44-column column, so the CPU axis keeps
        // its `CPU01` labels (the short form appears only below 34 columns).
        let mut terminal = Terminal::new(TestBackend::new(110, 42)).unwrap();
        let mut view = View::default();
        terminal
            .draw(|f| draw(f, &stats, &states, &UsageStats::default(), &mut view, 100))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let lines = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let first = lines
            .iter()
            .position(|line| line.contains("CPU01"))
            .unwrap();
        let last = lines
            .iter()
            .position(|line| line.contains("CPU12"))
            .unwrap();
        assert_eq!(last - first, 11);
        assert!(lines[first..=last].iter().all(|line| line.contains('%')));
        assert!(lines[first..=last].iter().all(|line| {
            ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█']
                .iter()
                .any(|symbol| line.contains(*symbol))
        }));
        assert!(view.page_size > 0);
        assert!(last < 20);
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use crate::model::{
        Bucketed, Card, FetchError, Meter, ModelUsage, Source, UsageStats, UsageTotal,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::{Duration, Instant};
    fn text(state: &SourceState, now: i64) -> String {
        quota_lines(std::slice::from_ref(state), 80, now, false)
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.to_string())
            .collect::<String>()
    }
    #[test]
    fn failed_manual_refresh_marks_held_data_stale() {
        let mut state = SourceState::new(Source::Agy, std::time::Duration::from_secs(60));
        state.begin("a".into());
        state.finish(
            Ok(vec![Card {
                meters: vec![Meter::from_used("周", 100., Some(10000), 0).unwrap()],
                ..Card::empty("A")
            }]),
            100,
            Instant::now(),
        );
        state.finish(Err(FetchError::new("连接超时")), 200, Instant::now());
        let rendered = text(&state, 200);
        assert!(rendered.contains("旧"), "{rendered}");
    }
    #[test]
    fn configured_ten_minute_refresh_is_not_stale_after_two_minutes() {
        let mut state = SourceState::new(Source::Go, Duration::from_secs(600));
        state.begin("a".into());
        state.finish(
            Ok(vec![Card {
                meters: vec![Meter::from_used("周", 20., Some(10000), 0).unwrap()],
                ..Card::empty("Go")
            }]),
            100,
            Instant::now() + Duration::from_secs(600),
        );
        let rendered = text(&state, 221);
        assert!(!rendered.contains("旧"), "{rendered}");
        assert!(!text(&state, 1300).contains("旧"));
        assert!(text(&state, 1301).contains("旧"));
        state.finish(Err(FetchError::new("连接超时")), 222, Instant::now());
        assert!(text(&state, 222).contains("旧"));
    }

    // -----------------------------------------------------------------------
    // Token panel
    // -----------------------------------------------------------------------

    /// Whether a symbol occupies two terminal columns (shared with the renderer
    /// so the test and the layout agree on what "wide" means).
    fn is_wide(symbol: &str) -> bool {
        symbol.chars().next().is_some_and(is_wide_char)
    }

    fn render(usage: &UsageStats, width: u16, height: u16) -> (String, View) {
        render_with_system(usage, &SystemStats::default(), width, height)
    }

    fn render_with_system(
        usage: &UsageStats,
        system: &SystemStats,
        width: u16,
        height: u16,
    ) -> (String, View) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
        let mut view = View::default();
        terminal
            .draw(|f| {
                draw(
                    f,
                    system,
                    &[],
                    usage,
                    &mut view,
                    chrono::Utc::now().timestamp(),
                )
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        // A wide glyph owns a cell plus a continuation cell, and `TestBackend`
        // renders that continuation as a blank. Dropping blanks that immediately
        // follow a wide character reconstructs the text the terminal actually
        // shows, while keeping the spaces an author typed.
        let text = (0..buffer.area.height)
            .map(|y| {
                let mut row = String::new();
                for x in 0..buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    if symbol == " " && x > 0 && is_wide(buffer[(x - 1, y)].symbol()) {
                        continue;
                    }
                    row.push_str(symbol);
                }
                row.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (text, view)
    }

    /// The token panel's body text on its own, independent of the sidebar, so
    /// the column-dropping rules can be exercised at exact widths.
    fn token_text(usage: &UsageStats, width: u16, height: usize) -> String {
        token_lines(usage, width, height)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// The foreground colours of every bar glyph on the rendered dashboard,
    /// scanned off the test backend's buffer.
    fn rendered_bar_colours(usage: &UsageStats, width: u16, height: u16) -> Vec<Color> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
        let mut view = View::default();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &SystemStats::default(),
                    &[],
                    usage,
                    &mut view,
                    chrono::Utc::now().timestamp(),
                )
            })
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol().chars().any(|c| "▊█▁▂▃▄▅▆▇".contains(c)))
            .map(|cell| cell.fg)
            .collect()
    }

    fn sample_usage() -> UsageStats {
        UsageStats {
            models: vec![ModelUsage {
                model: "deepseek/deepseek-v4-flash".into(),
                input_total: 9_600_000,
                cache_read: 8_400_000,
                output: 400_000,
                reasoning: 120_000,
                cost: Some(3.5),
            }],
            hours: Bucketed {
                buckets: {
                    let mut buckets = vec![0u64; 96];
                    // Six full hours at the ceiling: a wide pane visibly
                    // stretches these same bars, and they exercise the red band.
                    for bucket in &mut buckets[..24] {
                        *bucket = HOURLY_CEILING as u64;
                    }
                    buckets[40] = 90_000_000; // 45% of the ceiling: blue
                    buckets[44] = 150_000_000; // 75%: blue too, taller
                    buckets
                },
                costs: vec![0.0; 96],
            },
            month: Bucketed {
                // 30 days, four six-hour buckets each.
                buckets: {
                    let mut buckets = vec![0u64; 120];
                    buckets[4] = 1_500_000_000;
                    buckets[60] = DAILY_CEILING as u64;
                    buckets
                },
                costs: {
                    let mut costs = vec![0f64; 120];
                    costs[4] = COST_CEILING;
                    costs[60] = 60.0;
                    costs
                },
            },
            day_total: UsageTotal {
                tokens: 1_400_000,
                cost: 1.25,
            },
            week_total: UsageTotal {
                tokens: 4_200_000,
                cost: 6.80,
            },
            month_total: UsageTotal {
                tokens: 18_700_000_000,
                cost: 31.05,
            },
            year_total: UsageTotal {
                tokens: 112_400_000_000,
                cost: 286.44,
            },
            all_total: UsageTotal {
                tokens: 126_300_000_000,
                cost: 318.62,
            },
            error: None,
        }
    }

    #[test]
    fn the_dashboard_is_bounded_at_every_terminal_size() {
        let usage = sample_usage();
        for (width, height) in [(20, 10), (26, 12), (40, 24), (80, 24), (120, 60)] {
            let (_, view) = render(&usage, width, height);
            assert!(
                view.scroll <= view.max_scroll,
                "{width}x{height} left scroll {} above max {}",
                view.scroll,
                view.max_scroll
            );
        }
    }

    #[test]
    fn the_footer_no_longer_offers_a_page_switch() {
        let usage = sample_usage();
        let (text, _) = render(&usage, 80, 24);
        let footer = text.lines().last().unwrap_or_default();
        assert!(footer.contains("r 刷新"), "{footer}");
        assert!(
            !footer.contains("t 切换"),
            "the page switch is gone, so its hint must be too: {footer}"
        );
    }

    #[test]
    fn a_missing_database_says_so_instead_of_showing_zeros() {
        let usage = UsageStats {
            error: Some("unable to open database file".into()),
            ..UsageStats::default()
        };
        let (text, _) = render(&usage, 80, 24);
        assert!(text.contains("未连接 usage.db"), "{text}");
        assert!(
            !text.contains("$0.00"),
            "an unavailable database must not render as a real zero"
        );
    }

    #[test]
    fn the_total_line_survives_the_shortest_pane() {
        let usage = sample_usage();
        // The caveat and the histogram labels are the rows that may go; the total
        // is the anchor of the page and must stay visible.
        let (text, _) = render(&usage, 80, 13);
        assert!(text.contains("总计"), "the total must not be cut: {text}");
        assert!(text.contains("126.3B tokens"), "{text}");
        assert!(text.contains("$318.6"), "{text}");
    }

    #[test]
    fn no_row_carries_a_priced_share_badge() {
        let usage = sample_usage();
        // The coverage qualifier is gone from every card, framed or not.
        for height in [13usize, 24, 38] {
            let text = token_text(&usage, 100, height);
            assert!(
                !text.contains("计价"),
                "the priced-share badge must be gone ({height} rows): {text}"
            );
        }
        let text = token_text(&usage, 100, 38);
        let day_row = text
            .lines()
            .find(|line| line.contains("当日") && line.contains("$1.25"))
            .expect("the interval row must still show its money");
        assert!(day_row.contains("tokens"), "{day_row}");
    }

    #[test]
    fn the_two_panels_carry_distinct_titles() {
        let usage = sample_usage();
        let (text, _) = render(&usage, 80, 24);
        // Both sets of numbers share the screen now, so the titles are the only
        // thing keeping the local burn separate from the cloud remainder.
        assert!(text.contains("本地消耗 · Token"), "{text}");
        assert!(text.contains("AI 额度 · 剩余"), "{text}");
        assert!(text.contains("系统资源 · 已用"), "{text}");
    }

    #[test]
    fn a_narrow_pane_drops_columns_rather_than_wrapping() {
        let usage = sample_usage();
        // One row per model: the name opens the row and the value columns follow
        // it, the hit ratio riding inside the IN cell (there is no `IN(HIT)`
        // label). Wide panes show all five columns; a pane that cannot hold them
        // drops them from the right (`COST`, then `合计`, then `THINK`), and `IN`
        // and `OUT` always survive. The tool's minimum pane is 26 columns; below
        // that it shows a prompt to widen the window, so degradation is only
        // observable at 26 columns and above.
        let wide = token_text(&usage, 100, 30);
        assert!(!wide.contains("CACHE"), "{wide}");
        for label in ["IN", "OUT", "THINK", "合计", "COST"] {
            assert!(wide.contains(label), "{label} missing from: {wide}");
        }
        // 50 columns are the least that hold all five value columns beside a
        // `NAME_MIN`-wide name plus the gaps between them.
        let tight = token_text(&usage, 50, 30);
        assert!(tight.contains("COST"), "{tight}");
        assert!(tight.contains("deepseek/"), "{tight}");
        // 42 columns hold IN / OUT / THINK / 合计; COST is the first to go.
        let mid = token_text(&usage, 42, 30);
        for label in ["IN", "OUT", "THINK", "合计"] {
            assert!(mid.contains(label), "{label} missing from: {mid}");
        }
        assert!(!mid.contains("COST"), "{mid}");
        assert!(!mid.contains("IN(HIT)"), "{mid}");
        // 35 columns cannot hold 合计 either, leaving IN / OUT / THINK.
        let narrow = token_text(&usage, 35, 30);
        assert!(narrow.contains("IN"), "{narrow}");
        assert!(narrow.contains("OUT"), "{narrow}");
        assert!(narrow.contains("THINK"), "{narrow}");
        assert!(!narrow.contains("合计"), "{narrow}");
        assert!(!narrow.contains("COST"), "{narrow}");
        // 29 columns are the least that keep IN and OUT beside a readable name;
        // 26 (the tool minimum) still holds both values, with a shorter name.
        let tiny = token_text(&usage, 26, 30);
        assert!(tiny.contains("IN"), "{tiny}");
        assert!(tiny.contains("OUT"), "{tiny}");
        assert!(!tiny.contains("THINK"), "{tiny}");
        assert!(!tiny.contains("合计"), "{tiny}");
        assert!(!tiny.contains("COST"), "{tiny}");
    }

    #[test]
    fn no_token_row_is_wider_than_its_pane() {
        // A row wider than the pane is clipped at the panel's right border, and
        // the clipped cell is the last one — the money, or the closing rule of
        // the total's frame. `format!` pads by character count, so a CJK glyph
        // counts as one where the terminal gives it two: building every row at
        // every width is the only way to catch that class of bug.
        let usage = sample_usage();
        let empty = UsageStats::default();
        for usage in [&usage, &empty] {
            for width in 26..=160u16 {
                for line in token_lines(usage, width, 40) {
                    let text: String = line
                        .spans
                        .iter()
                        .map(|span| span.content.to_string())
                        .collect();
                    let used = columns(&text);
                    assert!(
                        used <= width as usize,
                        "a {used}-column row overflowed a {width}-column pane: {text:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_summary_column_sits_to_the_right_of_the_ranking() {
        let usage = sample_usage();
        // The top block is one row per model on the left and the interval
        // summary on the right: both appear on the same row, the model first.
        let text = token_text(&usage, 100, 30);
        let row = text
            .lines()
            .find(|line| line.contains("deepseek/deepseek-v4"))
            .unwrap_or_default();
        let name = row.find("deepseek/deepseek-v4").unwrap_or(usize::MAX);
        let interval = row.find("本周").unwrap_or(usize::MAX);
        assert!(name < interval, "the ranking must open the row: {row}");
        // The total stays with the summary, separated by a rule, and is never
        // repeated below the summary rows.
        assert!(text.contains("总计"), "{text}");
        assert!(text.contains("─"), "the total is separated by a rule: {text}");
        assert!(
            !text.contains("模型表为近24小时"),
            "the explanatory caveat is gone: {text}"
        );
    }

    #[test]
    fn long_model_names_are_truncated_not_wrapped() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("deepseek/deepseek-v4-flash", 12), "deepseek/de…");
        assert_eq!(truncate("abcdef", 0), "");
    }

    #[test]
    fn truncate_and_padding_support_wide_characters() {
        // "智谱-GLM": 2 wide chars (2*2=4 cols) + 4 ascii chars (4 cols) = 8 cols total
        assert_eq!(columns("智谱-GLM"), 8);
        assert_eq!(truncate("智谱-GLM", 8), "智谱-GLM");
        assert_eq!(truncate("智谱-GLM", 10), "智谱-GLM");

        // Truncation with wide chars:
        // width 5: 1 col for '…', 4 cols left for prefix: "智谱" (4 cols) + "…" = 5 cols
        assert_eq!(truncate("智谱-GLM", 5), "智谱…");
        assert_eq!(columns(&truncate("智谱-GLM", 5)), 5);

        // width 4: 1 col for '…', 3 cols left: "智" (2 cols) + "…" (1 col) = 3 cols (cannot fit "谱")
        assert_eq!(truncate("智谱-GLM", 4), "智…");
        assert_eq!(columns(&truncate("智谱-GLM", 4)), 3);

        // When padding is added:
        let name_width = 12;
        let name = truncate("智谱-GLM", name_width);
        let padding = name_width.saturating_sub(columns(&name));
        let formatted = format!(" {name}{}", " ".repeat(padding));
        // Leading space (1) + name_width (12) = 13 cols total
        assert_eq!(columns(&formatted), 13);
    }

    #[test]
    fn the_last_column_survives_a_pane_that_barely_fits_it() {
        // 52 columns are the least that hold all five value columns beside the
        // narrowest name column; at that width COST must neither be dropped nor
        // clipped to "COS".
        let usage = sample_usage();
        let text = token_text(&usage, 52, 30);
        let header = text
            .lines()
            .find(|line| line.contains("COST"))
            .unwrap_or_default();
        assert!(
            header.contains("COST"),
            "the cost header was clipped: {text}"
        );
        let row = text
            .lines()
            .find(|line| line.contains("9.6M(87.50%)"))
            .unwrap_or_default();
        assert!(row.contains("$3.50"), "the cost value was clipped: {row}");
    }

    #[test]
    fn header_labels_sit_above_their_values() {
        let usage = sample_usage();
        // Wide panes render the stacked block: the header band names `IN OUT THINK
        // 合计 COST`, and the value row beneath it shows the same cells right-aligned
        // in the same slots, so each label and its value share a right edge.
        let text = token_text(&usage, 100, 30);
        let header = text
            .lines()
            .find(|line| line.contains("IN") && line.contains("合计") && line.contains("COST"))
            .unwrap_or_default();
        let row = text
            .lines()
            .find(|line| {
                line.contains("9.6M(87.50%)") && line.contains("400K") && line.contains("$3.50")
            })
            .unwrap_or_default();
        assert!(!header.is_empty(), "no header band: {text}");
        assert!(!row.is_empty(), "no value row: {text}");
        // Each cell is right-aligned in an evenly divided slot, so the label's end
        // and the value's end must fall in the same terminal column.
        let ends = |line: &str, needle: &str| {
            line.find(needle)
                .map(|byte| columns(&line[..byte]) + columns(needle))
        };
        for (label, value) in [
            ("IN", "9.6M(87.50%)"),
            ("OUT", "400K"),
            ("合计", "10.0M"),
            ("COST", "$3.50"),
        ] {
            let header_end = ends(header, label).unwrap_or_default();
            let value_end = ends(row, value).unwrap_or_default();
            assert_eq!(
                header_end, value_end,
                "{label} and {value} do not share a right edge:\n{header}\n{row}"
            );
        }
    }

    #[test]
    fn the_system_and_token_panels_share_one_screen() {
        // The gauges and the local burn are columns of one page now: neither
        // replaces the other, and the footer no longer offers a switch.
        let usage = sample_usage();
        let system = SystemStats {
            cpu: Some(50.),
            cores: (0..12).map(|_| 50.).collect(),
            load: "1.00 1.00 1.00".into(),
            ..SystemStats::default()
        };
        let (text, view) = render_with_system(&usage, &system, 110, 34);
        assert!(text.contains("系统资源 · 已用"), "{text}");
        assert!(text.contains("CPU01"), "{text}");
        assert!(text.contains("AI 额度 · 剩余"), "{text}");
        assert!(text.contains("本地消耗 · Token"), "{text}");
        assert!(
            text.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
            "{text}"
        );
        let footer = text.lines().last().unwrap_or_default();
        assert!(!footer.contains("t 切换"), "{footer}");
        assert!(view.body.is_some());
    }

    #[test]
    fn footer_hints_expose_clickable_regions_that_match_their_text() {
        let usage = sample_usage();
        let (text, view) = render(&usage, 80, 24);
        let footer = text.lines().last().unwrap_or_default();
        // Every recorded region must land inside the footer row and point at the
        // label it rendered, so a click cannot act on what the user did not see.
        assert!(!view.footer_hits.is_empty(), "no clickable hints: {text}");
        for (first, last, action) in &view.footer_hits {
            assert!(
                first <= last,
                "inverted region {first}..{last} for {action:?}"
            );
            let slice: String = footer
                .chars()
                .skip(*first as usize)
                .take((last - first + 1) as usize)
                .collect();
            let expected = match action {
                FooterAction::Refresh => "r",
                FooterAction::ScrollUp => "↑",
                FooterAction::ScrollDown => "↓",
            };
            assert!(
                slice.contains(expected),
                "region {first}..{last} is {slice:?}, which does not name {action:?}"
            );
        }
    }

    #[test]
    fn clicking_the_footer_refresh_region_works_on_a_narrow_pane() {
        let usage = sample_usage();
        let (_, view) = render(&usage, 80, 24);
        // Find the region the renderer published and confirm the columns really
        // do cover the letters the user reads.
        let region = |wanted: FooterAction| {
            view.footer_hits
                .iter()
                .find(|(_, _, action)| *action == wanted)
                .map(|(first, last, _)| (*first, *last))
                .unwrap_or_else(|| panic!("no region for {wanted:?}"))
        };
        let (first, last) = region(FooterAction::Refresh);
        assert!(last >= first, "the refresh hint must be clickable");
        // A 40-column pane drops the scroll reminder, but the refresh key must
        // survive as a click target.
        let (_, narrow) = render(&usage, 40, 24);
        assert!(
            narrow
                .footer_hits
                .iter()
                .any(|(_, _, a)| *a == FooterAction::Refresh),
            "refresh must stay clickable on a 40-column pane"
        );
    }

    #[test]
    fn axis_origin_is_zero_and_the_ceiling_labels_stay_short() {
        assert_eq!(axis_label(0., 1e6, "M"), "0");
        assert_eq!(axis_label(0., 1e9, "B"), "0");
        assert_eq!(axis_label(0., 1e3, "K"), "0");
        assert_eq!(axis_label(0., 1., ""), "0");
        assert_eq!(axis_label(HOURLY_CEILING, 1e6, "M"), "200M");
        assert_eq!(axis_label(DAILY_CEILING, 1e9, "B"), "2B");
        // The money chart names its ceiling in whole dollars.
        assert_eq!(format!("${}", COST_CEILING.round() as i64), "$400");
    }

    #[test]
    fn bar_colours_follow_the_fixed_bands() {
        // Below 30% light green, 30-80% blue, 80-95% dark blue, 95% up red.
        assert_eq!(bar_color(0.0), BAR_LOW);
        assert_eq!(bar_color(0.29), BAR_LOW);
        assert_eq!(bar_color(0.30), BAR_MID);
        assert_eq!(bar_color(0.79), BAR_MID);
        assert_eq!(bar_color(0.80), BAR_HIGH);
        assert_eq!(bar_color(0.94), BAR_HIGH);
        assert_eq!(bar_color(0.95), RED);
        assert_eq!(bar_color(1.0), RED);
    }
    #[test]
    fn chart_bars_carry_the_band_colour_of_their_share() {
        let mut buckets = vec![0u64; 96];
        buckets[4] = HOURLY_CEILING as u64 / 5; // 20%: light green
        buckets[8] = HOURLY_CEILING as u64 / 2; // 50%: blue
        buckets[12] = HOURLY_CEILING as u64 * 9 / 10; // 90%: dark blue
        buckets[16] = HOURLY_CEILING as u64; // 100%: red
        let usage = UsageStats {
            hours: Bucketed {
                buckets,
                costs: vec![0.0; 96],
            },
            ..UsageStats::default()
        };
        // The drawn bar glyphs must carry the band colour their bucket falls
        // in, read off the rendered dashboard itself.
        let drawn = rendered_bar_colours(&usage, 160, 40);
        for (name, colour) in [
            ("light green", BAR_LOW),
            ("blue", BAR_MID),
            ("dark blue", BAR_HIGH),
            ("red", RED),
        ] {
            assert!(drawn.contains(&colour), "{name} bar missing: {drawn:?}");
        }
    }

    #[test]
    fn bar_layout_is_uniform() {
        let data = vec![HOURLY_CEILING as u64; 96];
        let chart = Chart {
            label: "每小时",
            series: Series::Tokens(&data),
            ceiling: HOURLY_CEILING,
            span_seconds: 15 * 60,
            tick_buckets: 4,
            first_tick: 0,
        };
        // Strip the Y-axis gutter: what is left is the plot itself.
        let plot_of = |line: &str| {
            line.split_once('┤')
                .map(|(_, plot)| plot.to_string())
                .unwrap_or_default()
        };
        // Dense: one cell per bar, no spare columns to spend on gaps, so the
        // bars tile the plot edge to edge with full blocks. Adjacent full
        // blocks read as one solid bar; the three-quarter block that used to
        // stand in here left a sliver down every bar, which is what made the
        // edges look bristly.
        let dense = plot_of(&histogram(&chart, 104, 4)[1].to_string());
        assert_eq!(dense.matches('█').count(), 96, "{dense}");
        assert!(!dense.contains(' '), "{dense}");
        // Roomy: bars stay one cell, the cells that are left over become gaps,
        // and every bar is the same width — no bar is twice its neighbour.
        let roomy = plot_of(&histogram(&chart, 200, 4)[1].to_string());
        assert_eq!(roomy.matches('█').count(), 96, "{roomy}");
        assert_eq!(roomy.matches(' ').count(), 96, "{roomy}");
        let gaps: Vec<usize> = roomy
            .split('█')
            .filter(|run| !run.is_empty())
            .map(|run| run.len())
            .collect();
        assert_eq!(gaps.len(), 95, "one gap between each pair of bars");
        assert!(
            gaps.iter().all(|gap| (1..=2).contains(gap)),
            "each gap is a cell or two, never a run: {gaps:?}"
        );
    }

    #[test]
    fn histogram_draws_its_ceiling_and_hourly_ticks() {
        let data = vec![100u64; 96];
        let chart = Chart {
            label: "每小时",
            series: Series::Tokens(&data),
            ceiling: HOURLY_CEILING,
            span_seconds: 15 * 60,
            tick_buckets: 4,
            first_tick: 0,
        };
        let lines = histogram(&chart, 140, 4);
        // 1 title + 4 bars + 1 baseline + 1 tick row.
        assert_eq!(lines.len(), 7);
        let title_line = lines[0].to_string();
        assert!(
            title_line.contains("每小时 · 每柱 15分钟"),
            "title line mismatch: {title_line}"
        );
        let baseline = lines[5].to_string();
        assert!(baseline.contains('└') && baseline.contains('0'));
        // A tick mark under every hour boundary: 24 quarter-hour groups a day.
        assert_eq!(
            baseline.matches('┴').count(),
            24,
            "one mark per hour: {baseline}"
        );
        let tick_line = lines[6].to_string();
        assert!(
            tick_line.trim_start().starts_with("0 "),
            "the first tick names hour 0: {tick_line:?}"
        );
        for hour in ["0", "6", "12", "18", "23"] {
            assert!(
                tick_line.split_whitespace().any(|tick| tick == hour),
                "missing hour {hour}: {tick_line:?}"
            );
        }
    }

    #[test]
    fn the_daily_and_money_charts_tick_days_and_label_dollars() {
        let tokens = vec![0u64; 120];
        let costs = vec![0f64; 120];
        let daily = Chart {
            label: "每天",
            series: Series::Tokens(&tokens),
            ceiling: DAILY_CEILING,
            span_seconds: 6 * 3_600,
            tick_buckets: 4,
            first_tick: 1,
        };
        let money = Chart {
            label: "金额",
            series: Series::Money(&costs),
            ceiling: COST_CEILING,
            span_seconds: 6 * 3_600,
            tick_buckets: 4,
            first_tick: 1,
        };
        let lines = histogram(&daily, 140, 3);
        assert!(lines[0].to_string().contains("每天 · 每柱 6小时"));
        assert_eq!(
            lines[4].to_string().matches('┴').count(),
            30,
            "one mark per day of the 30-day month"
        );
        assert!(
            lines[5]
                .to_string()
                .split_whitespace()
                .any(|tick| tick == "30"),
            "the day scale must reach the last day: {:?}",
            lines[5].to_string()
        );
        let money_lines = histogram(&money, 140, 3);
        assert!(money_lines[0].to_string().contains("金额 · 每柱 6小时"));
        assert!(
            money_lines[1].to_string().contains("$400 ┤"),
            "the money ceiling is $400: {:?}",
            money_lines[1].to_string()
        );
        assert!(
            money_lines[4].to_string().contains("$0 └"),
            "the money baseline is $0: {:?}",
            money_lines[4].to_string()
        );
    }

    #[test]
    fn the_axis_scale_follows_the_magnitude() {
        // The suffix is chosen so the label stays short at every magnitude.
        assert_eq!(axis_scale(900.), (1., ""));
        assert_eq!(axis_scale(7_000.), (1e3, "K"));
        assert_eq!(axis_scale(320_000_000.), (1e6, "M"));
        assert_eq!(axis_scale(126_300_000_000.), (1e9, "B"));
        // One decimal below ten, integers above; the `0` label has no decimals.
        assert_eq!(axis_label(1_200_000_000., 1e9, "B"), "1.2B");
        assert_eq!(axis_label(126_300_000_000., 1e9, "B"), "126B");
        assert_eq!(axis_label(320_000_000., 1e6, "M"), "320M");
        assert_eq!(axis_label(0., 1e3, "K"), "0");
    }

    #[test]
    fn a_period_chart_is_stretched_across_the_pane() {
        let usage = sample_usage();
        let narrow = render(&usage, 60, 44).0;
        let wide = render(&usage, 140, 44).0;
        // The hourly chart owns 96 quarter-hour buckets; on a wider pane each bar
        // claims more columns instead of the chart hugging the left edge.
        let bar_run = |text: &str| {
            text.lines()
                .map(|line| line.chars().filter(|c| *c == '▊' || *c == '█').count())
                .max()
                .unwrap_or(0)
        };
        assert!(
            bar_run(&wide) > bar_run(&narrow),
            "a wider pane must stretch the same data: narrow={} wide={}",
            bar_run(&narrow),
            bar_run(&wide)
        );
    }

    #[test]
    fn the_three_charts_share_the_bottom_sixty_percent_equally() {
        let usage = sample_usage();
        let (text, _) = render(&usage, 140, 46);
        let title_row = |label: &str| {
            text.lines()
                .enumerate()
                .find(|(_, line)| line.contains(label))
                .map(|(row, _)| row)
                .unwrap_or_else(|| panic!("{label} chart missing"))
        };
        let hourly = title_row("每小时 · ");
        let daily = title_row("每天 · ");
        let money = title_row("金额 · ");
        // The three charts split the bottom 60% of the token panel equally:
        // one pitch between neighbours, and the band starts on the 40% line.
        let step = daily - hourly;
        assert_eq!(money - daily, step, "all three charts share one pitch");
        // 46 rows: one footer, two panel borders → 43 inner rows.
        let inner = 43usize;
        assert!(
            (step as i64 - (inner * 2 / 10) as i64).abs() <= 1,
            "chart pitch {step} must be a fifth of the inner height"
        );
        assert!(
            ((hourly - 1) as i64 - (inner * 4 / 10) as i64).abs() <= 2,
            "the charts must start on the 40% line, hourly title at {hourly}"
        );
    }

    #[test]
    fn the_y_axis_labels_are_round_shares_of_their_ceilings() {
        let usage = sample_usage();
        let (text, _) = render(&usage, 140, 46);
        for label in [
            "200M ┤", "150M ┤", "100M ┤", "50M ┤", "2B ┤", "1.5B ┤", "1B ┤", "0.5B ┤", "$400 ┤",
            "$300 ┤", "$200 ┤", "$100 ┤",
        ] {
            assert!(text.contains(label), "{label} missing:\n{text}");
        }
        // The eighth-derived labels of the old grading are gone, and so is the
        // old 250M / 3B ceiling.
        assert!(!text.contains("188M"), "{text}");
        assert!(!text.contains("250M"), "{text}");
        assert!(!text.contains("3B ┤"), "{text}");
    }

    #[test]
    fn chart_rows_snap_to_round_divisions_of_the_ceiling() {
        assert_eq!(chart_rows(12), 8);
        assert_eq!(chart_rows(9), 5);
        assert_eq!(chart_rows(8), 4);
        assert_eq!(chart_rows(6), 2);
        assert_eq!(chart_rows(5), 1);
        assert_eq!(chart_rows(4), 0, "a rect that short cannot hold a chart");
    }

    #[test]
    fn a_pane_too_short_for_the_charts_hands_the_panel_back_to_the_list() {
        let usage = sample_usage();
        let (text, _) = render(&usage, 80, 16);
        assert!(!text.contains("每小时 · "), "{text}");
        assert!(text.contains("deepseek"), "{text}");
    }

    #[test]
    fn visual_render_check_token_panel() {
        let usage = sample_usage();
        let (text, _) = render(&usage, 140, 46);
        // The three local-burn charts, each against its fixed ceiling.
        assert!(text.contains("每小时 · "), "{text}");
        assert!(text.contains("每天 · "), "{text}");
        assert!(text.contains("金额 · "), "{text}");
        assert!(text.contains("200M ┤"), "{text}");
        assert!(text.contains("2B ┤"), "{text}");
        assert!(text.contains("0 └"), "{text}");
        assert!(!text.contains("配额"), "no quota wording is left: {text}");
        assert!(
            !text.contains("计价"),
            "no priced-share badge is left: {text}"
        );
    }

    #[test]
    fn an_empty_bucket_is_drawn_as_empty_track() {
        let usage = UsageStats {
            hours: Bucketed {
                buckets: vec![0; 96],
                costs: vec![0.0; 96],
            },
            ..UsageStats::default()
        };
        let (text, _) = render(&usage, 80, 24);
        assert!(
            !text.contains('█') && !text.contains('▊'),
            "no bucket has data, so no bars: {text}"
        );
    }
    #[test]
    fn model_table_renders_in_hit_column_and_no_cache_column() {
        let usage = sample_usage();
        let text = token_text(&usage, 100, 30);
        // The stacked block headers are `IN OUT THINK 合计 COST`; the hit ratio
        // rides inside the IN value cell rather than a separate labelled column,
        // and it carries two truncated decimals.
        assert!(text.contains("IN"), "{text}");
        assert!(!text.contains("CACHE"), "{text}");
        assert!(!text.contains("HIT%"), "{text}");
        assert!(text.contains("9.6M(87.50%)"), "{text}");
    }
}
