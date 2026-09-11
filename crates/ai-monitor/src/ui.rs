use crate::{
    model::{Bucketed, ModelUsage, Page, SourceState, UsageStats, UsageTotal, countdown},
    system::{SystemStats, gib},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Gauge, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Sparkline,
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
const DAILY_QUOTA: u64 = 300_000_000;
const WEEKLY_QUOTA: u64 = 2_000_000_000;
const MONTHLY_QUOTA: u64 = 8_000_000_000;

#[derive(Default)]
pub struct View {
    pub scroll: usize,
    pub max_scroll: usize,
    pub page_size: usize,
    pub page: Page,
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
    TogglePage,
    ScrollUp,
    ScrollDown,
}

impl FooterAction {
    /// The hints for a footer with `room` usable columns, left to right. Narrow
    /// panes drop the least load-bearing hints; the switch key outlives the
    /// scroll reminder, which only appears when there is something to scroll.
    fn row(room: usize, scrollable: bool) -> Vec<(&'static str, FooterAction)> {
        let mut hints = if room >= 30 {
            vec![
                (" r 刷新   ", FooterAction::Refresh),
                ("t 切换token   ", FooterAction::TogglePage),
            ]
        } else if room >= 20 {
            vec![
                (" r 刷新  ", FooterAction::Refresh),
                ("t 切换  ", FooterAction::TogglePage),
            ]
        } else if room >= 10 {
            vec![
                (" r  ", FooterAction::Refresh),
                ("t  ", FooterAction::TogglePage),
            ]
        } else {
            vec![]
        };
        if room >= 30 && scrollable {
            hints.push(("↑↓ 滚动   ", FooterAction::ScrollUp));
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
            Paragraph::new("ai-monitor\n请拉宽或拉高窗格\nq 退出").style(Style::default().fg(CYAN)),
            area,
        );
        return;
    }
    // The token page is a full-pane view: it replaces the system band rather
    // than sharing the pane with it, so `t` swaps the whole body and the CPU,
    // memory and network rows do not linger above a table that never uses them.
    let version = concat!("v", env!("CARGO_PKG_VERSION"));
    let footer_row = match view.page {
        Page::Quotas => {
            let desired_system_height = (system.cores.len() as u16)
                .saturating_add(8)
                .max(area.height / 3);
            // Keep enough room for the quota panel while allowing a common
            // 12-thread machine to show every logical CPU beside the chart.
            let system_height = desired_system_height.min(area.height.saturating_sub(9));
            let parts = Layout::vertical([
                Constraint::Length(system_height),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
            draw_system(frame, parts[0], system);
            draw_quotas(frame, parts[1], states, usage, view, now);
            parts[2]
        }
        Page::Tokens => {
            let parts = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
            draw_tokens(frame, parts[0], usage, view, now);
            parts[1]
        }
    };
    // The footer shares its row with the version stamp, so hints are dropped as
    // the pane narrows. The switch key outlives the scroll reminder, which only
    // appears when there is something to scroll. The same table drives both the
    // rendered text and the clickable regions, so they cannot drift apart.
    let room = (area.width as usize).saturating_sub(version.len() + 1);
    let hints = FooterAction::row(room, view.max_scroll > 0);
    let mut footer = String::from(" ");
    view.footer_hits.clear();
    for (label, action) in hints {
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
    }
    // The exit hint stays keyboard-only: it is always last, so clicking it would
    // be an accidental quit, which is the one action a stray click must not fire.
    footer.push_str("q 退出");
    let bar = Layout::horizontal([Constraint::Min(0), Constraint::Length(version.len() as u16)])
        .split(footer_row);
    // Everything above the footer is the page body, so a click or wheel inside
    // it can be routed to the current page without re-deriving the layout.
    view.footer_row = Some(footer_row.y);
    view.body = Some(Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: footer_row.y.saturating_sub(area.y),
    });
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(MUTED)),
        bar[0],
    );
    frame.render_widget(
        Paragraph::new(version)
            .style(Style::default().fg(MUTED))
            .right_aligned(),
        bar[1],
    );
}

fn draw_system(frame: &mut Frame, area: Rect, stats: &SystemStats) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 系统资源 · 已用 ")
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

fn draw_quotas(
    frame: &mut Frame,
    area: Rect,
    states: &[SourceState],
    usage: &UsageStats,
    view: &mut View,
    now: i64,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" AI 额度 · 剩余 ")
        .border_style(Style::default().fg(CYAN));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = quota_lines(states, inner.width, now, true);
    if lines.len() > inner.height as usize {
        lines = quota_lines(states, inner.width, now, false);
    }
    // Lifetime local consumption, the one figure this page shares with the token
    // page. It is intentionally not coloured by amount: this money is already
    // spent, so the red/amber/green "remaining quota" scale does not apply.
    lines.push(local_usage_line(usage));
    view.page_size = inner.height as usize;
    view.max_scroll = lines.len().saturating_sub(view.page_size);
    view.scroll = view.scroll.min(view.max_scroll);
    frame.render_widget(
        Paragraph::new(lines.clone()).scroll((view.scroll.min(u16::MAX as usize) as u16, 0)),
        inner,
    );
    if view.max_scroll > 0 {
        let mut scroll = ScrollbarState::new(lines.len())
            .position(view.scroll)
            .viewport_content_length(view.page_size);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_style(Style::default().fg(TRACK))
                .thumb_style(Style::default().fg(MUTED)),
            area,
            &mut scroll,
        );
    }
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
// Token page
// ---------------------------------------------------------------------------

/// Lifetime local consumption, one line, shared by both pages so the two cannot
/// disagree. A database that cannot be read says so rather than showing zeros.
fn local_usage_line(usage: &UsageStats) -> Line<'static> {
    if let Some(error) = &usage.error {
        let _ = error;
        return Line::from(vec![
            Span::styled(" 本地消耗  ", Style::default().fg(MUTED)),
            Span::styled("未连接".to_string(), Style::default().fg(MUTED)),
        ]);
    }
    Line::from(vec![
        Span::styled(" 本地消耗  ", Style::default().fg(MUTED)),
        Span::styled(
            format!("{} tokens  ", compact(usage.all_total.tokens)),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            money(usage.all_total.cost),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn draw_tokens(frame: &mut Frame, area: Rect, usage: &UsageStats, view: &mut View, now: i64) {
    let _ = now;
    let block = Block::default()
        .borders(Borders::ALL)
        // Deliberately not "额度": the four local clients and the six cloud quota
        // sources are different things, and the title is the only thing stopping
        // the two sets of numbers from being read as one.
        .title(" 本地消耗 · Token (24H) ")
        .border_style(Style::default().fg(CYAN));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        view.page_size = 0;
        view.max_scroll = 0;
        view.scroll = 0;
        return;
    }
    if usage.error.is_some() {
        view.page_size = inner.height as usize;
        view.max_scroll = 0;
        view.scroll = 0;
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(" 未连接 usage.db", Style::default().fg(MUTED)),
                Line::styled(" 采集未运行或数据库不存在", Style::default().fg(MUTED)),
            ]),
            inner,
        );
        return;
    }

    // `inner` is already the bordered interior, so these widths are exact.
    let lines = token_lines(usage, inner.width, inner.height as usize);
    view.page_size = inner.height as usize;
    view.max_scroll = lines.len().saturating_sub(view.page_size);
    view.scroll = view.scroll.min(view.max_scroll);
    frame.render_widget(
        Paragraph::new(lines.clone()).scroll((view.scroll.min(u16::MAX as usize) as u16, 0)),
        inner,
    );
    if view.max_scroll > 0 {
        let mut scroll = ScrollbarState::new(lines.len())
            .position(view.scroll)
            .viewport_content_length(view.page_size);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_style(Style::default().fg(TRACK))
                .thumb_style(Style::default().fg(MUTED)),
            area,
            &mut scroll,
        );
    }
}

/// Assemble the token page body. `caveat` is listed first so that, when the pane
/// is short, the disappearing rows are the least load-bearing ones — the totals
/// line is what the page exists for.
fn token_lines(usage: &UsageStats, width: u16, height: usize) -> Vec<Line<'static>> {
    // The total is the anchor of this page, so the sections are assembled
    // largest-first and the ones that do not fit are left out; the pane is only
    // allowed to scroll when even the minimum set overflows.
    let caveat = (width as usize >= 46).then(|| {
        Line::styled(
            " 模型表为近24小时；总计为历史全量；日/周/月为各自区间",
            Style::default().fg(MUTED),
        )
    });
    let total = total_line(&usage.all_total, width);
    let summary = interval_summary(usage, width);

    // Ordered by what to give up first: the caveat and the year histogram matter
    // least, the summary rows and the total matter most. `bars` is the histogram
    // height in character rows: taller charts are tried first and only shrink
    // when the pane cannot hold them alongside the rest.
    for (models, histograms, bars, summary_rows, caveat_shown) in [
        (6usize, 3usize, 4usize, true, true),
        (6, 3, 4, true, false),
        (6, 3, 3, true, false),
        (6, 2, 3, true, false),
        (6, 2, 2, true, false),
        (6, 1, 2, true, false),
        (6, 1, 1, true, false),
        (6, 0, 0, true, false),
        (4, 0, 0, true, false),
        (3, 0, 0, false, false),
        (2, 0, 0, false, false),
        (1, 0, 0, false, false),
    ] {
        let mut lines: Vec<Line<'static>> = vec![];
        if models > 0 {
            lines.extend(model_table(&usage.models, width, models));
            lines.push(Line::raw(""));
        }
        if histograms > 0 {
            lines.extend(histograms_section(usage, width, histograms, bars));
            lines.push(Line::raw(""));
        }
        if summary_rows {
            lines.extend(summary.iter().cloned());
            lines.push(Line::raw(""));
        }
        if let (true, Some(caveat)) = (caveat_shown, &caveat) {
            lines.push(caveat.clone());
        }
        lines.push(total.clone());
        if lines.len() <= height.max(1) {
            return lines;
        }
    }
    // Even the minimum set overflows: keep the total and let the pane scroll.
    let mut lines = vec![];
    if let Some(caveat) = &caveat {
        lines.push(caveat.clone());
    }
    lines.push(total);
    lines
}

fn model_table(models: &[ModelUsage], width: u16, limit: usize) -> Vec<Line<'static>> {
    if models.is_empty() {
        return vec![Line::styled(" 暂无用量记录", Style::default().fg(MUTED))];
    }
    // One layout for every width: the stacked three-row block. Narrow panes shrink
    // the even slots and drop columns from the right (see `model_block`); they
    // never fall back to the old single-row shape.
    let taken: Vec<&ModelUsage> = models.iter().take(limit).collect();
    let mut lines = vec![];
    for model in &taken {
        lines.extend(model_block(model, width));
    }
    lines
}

/// Three rows per model, the only model-table shape: the name on its own line,
/// then a header band and the values. The value columns (`IN OUT THINK 合计 COST`)
/// tile the full pane edge-to-edge, each right-aligned in its slot so every label
/// sits above its value.
///
/// Degradation is by column, not by layout: each slot is at least its content
/// width, any spare width is shared out evenly, and when the pane is too narrow
/// the rightmost columns are dropped (`COST`, then `合计`, then `THINK`) until it
/// fits. `IN` and `OUT` always survive — they are what the page is for.
fn model_block(model: &ModelUsage, width: u16) -> Vec<Line<'static>> {
    let in_val = model.input_display();
    let cost = match model.cost {
        Some(c) => (money(c), GREEN),
        None => ("—".into(), MUTED),
    };
    // Left to right; `COST` is the first to go, `合计` and `THINK` follow, `IN`
    // and `OUT` are kept.
    let mut cols: Vec<(&str, usize, String, Color)> = vec![
        ("IN", columns(&in_val).max(2), in_val, INK),
        ("OUT", 5, compact(model.output), INK),
        ("THINK", 5, compact(model.reasoning), MUTED),
        ("合计", 6, compact(model.total_tokens()), INK),
        ("COST", 7, cost.0, cost.1),
    ];
    // Drop the rightmost columns until the minimum set fits the pane.
    while cols.len() > 2 && (1 + cols.iter().map(|c| c.1).sum::<usize>()) > width as usize {
        cols.pop();
    }
    let mins: usize = cols.iter().map(|c| c.1).sum();
    let available = width as usize;
    let leftover = available.saturating_sub(mins);
    let k = cols.len();
    let base = leftover / k;
    let extra = leftover % k;
    let widths: Vec<usize> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| c.1 + base + (i < extra) as usize)
        .collect();

    // Row 1: the model name on its own line, left-aligned and truncated.
    let name = truncate(&model.model, available.saturating_sub(1).max(1));
    let name_row = Line::from(vec![Span::styled(
        format!(" {name}"),
        Style::default().fg(CYAN),
    )]);

    // Header band and value band: right-align each cell in its slot and tile the
    // whole pane, so the band is flush to both edges.
    let band = |cells: Vec<(String, Color)>| -> Line<'static> {
        let mut spans = vec![];
        for (index, (text, color)) in cells.into_iter().enumerate() {
            let slack = widths[index].saturating_sub(columns(&text));
            spans.push(Span::styled(
                format!("{}{}", " ".repeat(slack), text),
                Style::default().fg(color),
            ));
        }
        Line::from(spans)
    };
    let header = band(
        cols.iter()
            .map(|(label, _, _, _)| (label.to_string(), MUTED))
            .collect(),
    );
    let values = band(
        cols.iter()
            .map(|(_, _, val, color)| (val.clone(), *color))
            .collect(),
    );
    vec![name_row, header, values]
}

/// One block per group: the plot, the axis, the tick line, then the period name.
///
/// The chart runs left to right across the pane against a labelled Y axis, and
/// the period it covers (`本年` / `当月` / `当日`) sits *below* it, where the eye
/// lands after reading the shape. `rows` is the bar height in character rows.
fn histograms_section(
    usage: &UsageStats,
    width: u16,
    groups: usize,
    rows: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![];
    let available = [
        ("当日", &usage.day, DAILY_QUOTA),
        ("本周", &usage.week, WEEKLY_QUOTA),
        ("当月", &usage.month, MONTHLY_QUOTA),
    ];
    let keep = groups.min(available.len());
    for (label, data, quota) in &available[..keep] {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.extend(histogram(label, data, *quota, width, rows));
    }
    if lines.is_empty() {
        lines.push(Line::styled(" 暂无用量记录", Style::default().fg(MUTED)));
    }
    lines
}

/// Suffix and divisor for the axis scale that keeps the labels shortest:
/// `1.2B` reads better than `1234M`, but `900M` reads better than `0.9B`.
fn axis_scale(max: u64) -> (f64, &'static str) {
    if max >= 1_000_000_000 {
        (1e9, "B")
    } else if max >= 1_000_000 {
        (1e6, "M")
    } else if max >= 1_000 {
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

/// Draw one histogram: bar rows against a labelled Y axis, then a zero
/// baseline, centred bucket ticks and the period name.
///
/// Buckets are one column wide, so a 24-hour day needs 24 columns. Rather than
/// dropping bars, adjacent buckets are summed: the shape stays honest and the
/// plot always reaches the pane edge.
///
/// The Y axis ticks at row boundaries, so every graded label names the value a
/// bar holds when it fills its row and everything below it: the top label is
/// the maximum, the baseline is zero, and a tick with no number leaves nothing
/// to judge by.
fn histogram(
    label: &str,
    data: &Bucketed,
    fixed_quota: u64,
    width: u16,
    rows: usize,
) -> Vec<Line<'static>> {
    let raw = &data.buckets;
    // The plot is what is left of the pane after the axis and its gutter.
    let plot = (width as usize).saturating_sub(AXIS_WIDTH + 1);
    if raw.is_empty() || plot == 0 {
        return vec![];
    }
    // Merge until the bucket count fits the plot.
    let mut factor = 1usize;
    while raw.len().div_ceil(factor) > plot {
        factor += 1;
    }
    let merged: Vec<u64> = raw
        .chunks(factor)
        .map(|chunk| chunk.iter().copied().sum())
        .collect();
    let max = fixed_quota;
    let (divisor, suffix) = axis_scale(max);
    let current = current_bucket(data, factor);
    let height = rows.max(1);
    // The glyph grid: `height` rows of eight sub-rows each.
    let total_subrows = height * 8;
    let eighths: Vec<usize> = merged
        .iter()
        .map(|value| {
            if max == 0 || *value == 0 {
                0
            } else {
                (((*value as f64 / max as f64) * total_subrows as f64).round() as usize)
                    .min(total_subrows)
            }
        })
        .collect();
    // Stretch the buckets across the whole plot so the chart is edge to edge no
    // matter how many buckets the period has: a 12-month year on a wide pane
    // must not huddle in the left corner. Columns are shared out evenly, with
    // the remainder spread over the leading buckets so the right edge still
    // lands on the pane edge.
    let columns = merged.len().max(1);
    let widths: Vec<usize> = if columns >= plot {
        vec![1; columns]
    } else {
        let base = plot / columns;
        let extra = plot % columns;
        (0..columns)
            .map(|index| base + usize::from(index < extra))
            .collect()
    };

    // The value a bar holds when its top reaches a sub-row, so a tick can name
    // the height it marks instead of standing there unlabelled.
    let value_at = |subrows: usize| max as f64 * subrows as f64 / total_subrows as f64;
    let mut out: Vec<Line<'static>> = Vec::with_capacity(height + 3);

    let month_days = format!("{}天", data.buckets.len());
    let fallback_items = format!("{}项", data.buckets.len());
    let (duration_str, unit_str) = match label {
        "当日" => ("24小时", "小时"),
        "本周" => ("7天", "天"),
        "当月" => (month_days.as_str(), "天"),
        _ => (fallback_items.as_str(), "项"),
    };
    let quota_str = if fixed_quota.is_multiple_of(100_000_000) {
        format!(" (配额 {}亿)", fixed_quota / 100_000_000)
    } else {
        format!(" (配额 {})", compact(fixed_quota))
    };
    let merge_str = if factor > 1 {
        format!(" · 每柱 {factor}{unit_str}")
    } else {
        String::new()
    };
    let title = format!(" {label} · {duration_str}{quota_str}{merge_str}");
    out.push(Line::styled(
        title,
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    ));

    for row in 0..height {
        // Sub-rows still available to this row, counted from the baseline up.
        let base = (height - 1 - row) * 8;
        // Every row boundary a bar top can land on gets a label, up to three
        // besides the baseline: reading mid-height bars off a single top label
        // is guesswork.
        let ticks = height.min(3);
        let step = (height - 1) / ticks.max(1).min(height - 1).max(1);
        let graded = ticks > 1 && step > 0 && row % step == 0 && row / step < ticks;
        let head = if max > 0 && (row == 0 || graded) {
            // The value at this row's *top* boundary, so the topmost label is
            // the maximum and no graded row repeats the baseline's zero —
            // naming the bottom boundary would print `0` again on short panes.
            axis_label(value_at((height - row) * 8), divisor, suffix)
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
            // Full rows are solid; the topmost partial row uses the eighth block
            // matching its height, so a bar's top edge moves smoothly.
            let glyph = match used {
                0 => ' ',
                8 => '█',
                n => ['▁', '▂', '▃', '▄', '▅', '▆', '▇'][n - 1],
            };
            let colour = if glyph == ' ' {
                TRACK
            } else if merged[index] > max {
                RED
            } else if Some(index) == current {
                GREEN
            } else {
                CYAN
            };
            // Every bucket owns `widths[index]` columns; the glyph is repeated
            // across them so a stretched bar stays a solid block.
            spans.push(Span::styled(
                glyph.to_string().repeat(widths[index]),
                Style::default().fg(colour),
            ));
        }
        out.push(Line::from(spans));
    }
    // Baseline: a zero label, the corner under the axis, then the rule the bars
    // stand on.
    let mut baseline = vec![
        Span::styled(
            format!(
                " {:>pad$} ",
                axis_label(0., divisor, suffix),
                pad = AXIS_WIDTH - 1
            ),
            Style::default().fg(MUTED),
        ),
        Span::styled("└", Style::default().fg(TRACK)),
    ];
    baseline.push(Span::styled("─".repeat(plot), Style::default().fg(TRACK)));
    out.push(Line::from(baseline));
    // Bucket numbers are indented to the plot and centred under their own bar,
    // through the same column widths the bars were drawn with.
    out.push(bucket_labels(data, factor, plot, &widths));
    out
}
/// Which merged bucket contains "now", so it can be highlighted.
fn current_bucket(data: &Bucketed, factor: usize) -> Option<usize> {
    current_bucket_at(data, factor, chrono::Local::now())
}

fn current_bucket_at(
    data: &Bucketed,
    factor: usize,
    now: chrono::DateTime<chrono::Local>,
) -> Option<usize> {
    use chrono::{Datelike, Timelike};
    let index = match data.buckets.len() {
        24 => now.hour() as usize,
        7 => now.weekday().num_days_from_monday() as usize,
        12 => now.month0() as usize,
        length => {
            let day = now.day().saturating_sub(1) as usize;
            if length >= 28 {
                day
            } else {
                return None;
            }
        }
    };
    (index < data.buckets.len()).then(|| index / factor.max(1))
}

/// The row of bucket numbers under a chart.
///
/// A tick names a bar and is centred over it: a number printed at the bar's left
/// edge reads as the boundary between two bars, so a reader counting along the
/// axis lands on the wrong one. The last tick is pinned to the plot edge when
/// centring would run it past — a clipped number is worse than one that sits a
/// column off centre.
fn bucket_labels(
    data: &Bucketed,
    factor: usize,
    available: usize,
    widths: &[usize],
) -> Line<'static> {
    let count = data.buckets.len().div_ceil(factor).min(available);
    let first = data.first_bucket as usize;
    // Tick at the first bucket, the middle and the last, plus one more when the
    // row is wide, so a 31-day row keeps an intermediate reference point.
    let ticks = if available >= 26 { 4 } else { 3 };
    // Where a bucket's columns start inside the stretched plot, and how many it
    // owns: centring needs both.
    let offset = |index: usize| widths.iter().take(index).sum::<usize>();
    let mut positions: Vec<(usize, String)> = vec![];
    for tick in 0..ticks {
        let position = if ticks == 1 {
            0
        } else {
            tick * (count.saturating_sub(1)) / (ticks - 1)
        };
        if position >= count {
            continue;
        }
        let text = format!("{}", first + position * factor);
        let start = if tick == 0 {
            0
        } else if tick == ticks - 1 {
            available.saturating_sub(text.len())
        } else {
            let span = widths.get(position).copied().unwrap_or(1);
            let centre = offset(position) + span / 2;
            centre
                .saturating_sub(text.len() / 2)
                .min(available.saturating_sub(text.len()))
        };
        positions.push((start, text));
    }
    // Emit left to right, skipping any label that would collide with the previous
    // one; a half-overwritten number is worse than a missing tick.
    let mut row = String::new();
    let mut cursor = 0usize;
    for (position, text) in positions {
        if position < cursor {
            continue;
        }
        if row.len() < position {
            row.push_str(&" ".repeat(position - row.len()));
        }
        row.push_str(&text);
        cursor = position + text.len() + 1;
    }
    Line::styled(
        format!("{:width$}{row}", "", width = AXIS_WIDTH + 1),
        Style::default().fg(MUTED),
    )
}

fn interval_summary(usage: &UsageStats, width: u16) -> Vec<Line<'static>> {
    let rows = [
        ("当日", &usage.day_total),
        ("本周", &usage.week_total),
        ("当月", &usage.month_total),
    ];
    rows.iter()
        .map(|(label, total)| interval_line(label, total, width))
        .collect::<Vec<_>>()
}

fn interval_line(label: &str, total: &UsageTotal, width: u16) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!(" {label}  "), Style::default().fg(MUTED)),
        Span::styled(
            format!("{} tokens  ", compact(total.tokens)),
            Style::default().fg(INK),
        ),
        Span::styled(money(total.cost), Style::default().fg(INK)),
    ];
    // Below 100% the money is a lower bound, so say so rather than letting it
    // read as the full bill.
    if let Some(coverage) = total.coverage.filter(|coverage| *coverage < 1.0) {
        let text = format!("  计价 {:.0}%", coverage * 100.);
        if width as usize >= 44 {
            spans.push(Span::styled(text, Style::default().fg(AMBER)));
        }
    }
    Line::from(spans)
}

fn total_line(total: &UsageTotal, width: u16) -> Line<'static> {
    let mut spans = vec![
        Span::styled(" 总计  ", Style::default().fg(MUTED)),
        Span::styled(
            format!("{} tokens  ", compact(total.tokens)),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            money(total.cost),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
    ];
    // CJK glyphs take two terminal columns, so the fit must be measured in
    // columns rather than in characters.
    let used: usize = spans.iter().map(|span| columns(&span.content)).sum();
    let qualifier = format!("  计价 {:.0}%", total.coverage.unwrap_or(1.0) * 100.);
    if total.coverage.is_some_and(|coverage| coverage < 1.0)
        && used + columns(&qualifier) <= width as usize
    {
        spans.push(Span::styled(qualifier, Style::default().fg(AMBER)));
    }
    Line::from(spans)
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
        let mut terminal = Terminal::new(TestBackend::new(44, 42)).unwrap();
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
        Bucketed, Card, FetchError, Meter, ModelUsage, Page, Source, UsageStats, UsageTotal,
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
    // Token page
    // -----------------------------------------------------------------------

    /// Whether a symbol occupies two terminal columns (shared with the renderer
    /// so the test and the layout agree on what "wide" means).
    fn is_wide(symbol: &str) -> bool {
        symbol.chars().next().is_some_and(is_wide_char)
    }

    fn render(page: Page, usage: &UsageStats, width: u16, height: u16) -> (String, View) {
        render_with_system(page, usage, &SystemStats::default(), width, height)
    }

    fn render_with_system(
        page: Page,
        usage: &UsageStats,
        system: &SystemStats,
        width: u16,
        height: u16,
    ) -> (String, View) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
        let mut view = View {
            page,
            ..View::default()
        };
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
            day: Bucketed {
                buckets: {
                    let mut buckets = vec![0u64; 24];
                    buckets[10] = 150_000_000;
                    buckets[11] = 300_000_000;
                    buckets
                },
                first_bucket: 0,
            },
            week: Bucketed {
                buckets: {
                    let mut buckets = vec![0u64; 7];
                    buckets[2] = 1_000_000_000;
                    buckets[4] = 2_000_000_000;
                    buckets
                },
                first_bucket: 1,
            },
            month: Bucketed {
                buckets: {
                    let mut buckets = vec![0u64; 30];
                    buckets[4] = 4_000_000_000;
                    buckets[15] = 8_000_000_000;
                    buckets
                },
                first_bucket: 1,
            },
            year: Bucketed {
                buckets: {
                    let mut buckets = vec![0u64; 12];
                    buckets[8] = 7_000;
                    buckets
                },
                first_bucket: 1,
            },
            day_total: UsageTotal {
                tokens: 1_400_000,
                cost: 1.25,
                coverage: Some(1.0),
            },
            week_total: UsageTotal {
                tokens: 4_200_000,
                cost: 6.80,
                coverage: Some(0.98),
            },
            month_total: UsageTotal {
                tokens: 18_700_000_000,
                cost: 31.05,
                coverage: Some(0.92),
            },
            year_total: UsageTotal {
                tokens: 112_400_000_000,
                cost: 286.44,
                coverage: Some(1.0),
            },
            all_total: UsageTotal {
                tokens: 126_300_000_000,
                cost: 318.62,
                coverage: Some(0.97),
            },
            error: None,
        }
    }

    #[test]
    fn switching_pages_resets_the_scroll_offset() {
        let usage = sample_usage();
        // Enter the token page with a scroll offset left over from the quota
        // page: the rows mean something different, so the offset must not carry.
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("backend");
        let mut view = View {
            page: Page::Quotas,
            scroll: 9,
            max_scroll: 20,
            page_size: 5,
            ..View::default()
        };
        terminal
            .draw(|f| draw(f, &SystemStats::default(), &[], &usage, &mut view, 100))
            .expect("draw");
        // Emulate the `t` handler in main.
        view.page = Page::Tokens;
        view.scroll = 0;
        terminal
            .draw(|f| draw(f, &SystemStats::default(), &[], &usage, &mut view, 100))
            .expect("draw");
        assert_eq!(view.page, Page::Tokens);
        assert_eq!(
            view.scroll, 0,
            "a stale quota offset must not survive the switch"
        );
    }

    #[test]
    fn the_token_page_is_bounded_at_every_terminal_size() {
        let usage = sample_usage();
        for (width, height) in [(20, 10), (26, 12), (40, 24), (80, 24), (120, 60)] {
            let (_, view) = render(Page::Tokens, &usage, width, height);
            assert!(
                view.scroll <= view.max_scroll,
                "{width}x{height} left scroll {} above max {}",
                view.scroll,
                view.max_scroll
            );
        }
    }

    #[test]
    fn both_pages_advertise_the_page_switch_key() {
        let usage = sample_usage();
        for page in [Page::Quotas, Page::Tokens] {
            let (text, _) = render(page, &usage, 80, 24);
            assert!(
                text.contains("t 切换token"),
                "{page:?} footer is missing the switch hint: {text}"
            );
        }
    }

    #[test]
    fn a_missing_database_says_so_instead_of_showing_zeros() {
        let usage = UsageStats {
            error: Some("unable to open database file".into()),
            ..UsageStats::default()
        };
        let (quota_text, _) = render(Page::Quotas, &usage, 80, 24);
        assert!(
            quota_text.contains("本地消耗  未连接"),
            "homepage should report the gap: {quota_text}"
        );
        assert!(
            !quota_text.contains("$0.00"),
            "an unavailable database must not render as a real zero"
        );
        let (token_text, _) = render(Page::Tokens, &usage, 80, 24);
        assert!(token_text.contains("未连接 usage.db"), "{token_text}");
    }

    #[test]
    fn the_share_is_identical_on_the_homepage_row_and_the_token_total() {
        // The two places show one number; drift between them would read as a bug.
        let usage = sample_usage();
        let (quota_text, _) = render(Page::Quotas, &usage, 100, 30);
        let row = quota_text
            .lines()
            .find(|line| line.contains("本地消耗"))
            .unwrap_or_default();
        assert!(row.contains("126.3B tokens"), "{quota_text}");
        // One formatter serves both places, so the money text must be identical
        // rather than merely equal in value.
        let token_total = usage.all_total;
        assert!(row.contains(&money(token_total.cost)), "{quota_text}");
        let (token_text, _) = render(Page::Tokens, &usage, 100, 40);
        assert!(token_text.contains("126.3B tokens"), "{token_text}");
        assert!(
            token_text.contains(&money(token_total.cost)),
            "{token_text}"
        );
    }

    #[test]
    fn the_total_line_survives_the_shortest_pane() {
        let usage = sample_usage();
        // The caveat and the histogram labels are the rows that may go; the total
        // is the anchor of the page and must stay visible.
        let (text, _) = render(Page::Tokens, &usage, 80, 13);
        assert!(text.contains("总计"), "the total must not be cut: {text}");
        assert!(text.contains("126.3B tokens"), "{text}");
        assert!(text.contains("$318.6"), "{text}");
    }

    #[test]
    fn the_interval_rows_mark_partial_coverage() {
        let usage = sample_usage();
        let (text, _) = render(Page::Tokens, &usage, 100, 40);
        // month_total is 92% priced, so its money is a lower bound.
        assert!(
            text.contains("计价 92%"),
            "the 92%-priced interval must say so: {text}"
        );
        // The fully-priced rows must not carry a misleading qualifier.
        let day_row = text
            .lines()
            .find(|line| line.contains("当日") && line.contains("$1.25"))
            .unwrap_or_default();
        assert!(!day_row.contains("计价"), "{day_row}");
    }

    #[test]
    fn the_token_title_avoids_the_quota_word() {
        let usage = sample_usage();
        let (tokens, _) = render(Page::Tokens, &usage, 80, 24);
        assert!(tokens.contains("本地消耗 · Token"), "{tokens}");
        assert!(
            !tokens.contains("额度"),
            "the token page must not say 额度: {tokens}"
        );
        let (quotas, _) = render(Page::Quotas, &usage, 80, 24);
        assert!(quotas.contains("AI 额度 · 剩余"), "{quotas}");
    }

    #[test]
    fn a_narrow_pane_drops_columns_rather_than_wrapping() {
        let usage = sample_usage();
        // One layout at every width: the stacked three-row block. There is no
        // `IN(HIT)` label — the hit ratio rides inside the IN cell (`9.6M(88%)`).
        // The tool's minimum pane is 26 columns; below that it shows a prompt to
        // widen the window, so degradation is only observable at 26-34 columns.
        let (wide, _) = render(Page::Tokens, &usage, 100, 30);
        assert!(!wide.contains("CACHE"), "{wide}");
        assert!(wide.contains("IN"), "{wide}");
        assert!(wide.contains("OUT"), "{wide}");
        assert!(wide.contains("THINK"), "{wide}");
        assert!(wide.contains("合计"), "{wide}");
        assert!(wide.contains("COST"), "{wide}");
        // 40 columns hold all five columns (each at its minimum width, the
        // spare shared out evenly) — the block, not the old inline row.
        let (mid, _) = render(Page::Tokens, &usage, 40, 30);
        assert!(mid.contains("IN"), "{mid}");
        assert!(mid.contains("OUT"), "{mid}");
        assert!(mid.contains("THINK"), "{mid}");
        assert!(mid.contains("合计"), "{mid}");
        assert!(mid.contains("COST"), "{mid}");
        assert!(!mid.contains("IN(HIT)"), "{mid}");
        // 34 columns cannot hold COST; it drops from the right, leaving
        // IN / OUT / THINK / 合计. IN and OUT always survive.
        let (narrow, _) = render(Page::Tokens, &usage, 34, 30);
        assert!(narrow.contains("IN"), "{narrow}");
        assert!(narrow.contains("OUT"), "{narrow}");
        assert!(narrow.contains("THINK"), "{narrow}");
        assert!(narrow.contains("合计"), "{narrow}");
        assert!(!narrow.contains("COST"), "{narrow}");
        // 26 columns (the tool minimum) drop COST and 合计, keeping IN / OUT / THINK.
        let (tiny, _) = render(Page::Tokens, &usage, 26, 30);
        assert!(tiny.contains("IN"), "{tiny}");
        assert!(tiny.contains("OUT"), "{tiny}");
        assert!(tiny.contains("THINK"), "{tiny}");
        assert!(!tiny.contains("合计"), "{tiny}");
        assert!(!tiny.contains("COST"), "{tiny}");
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
    fn current_bucket_covers_24_hours_and_23_is_last_bucket() {
        use chrono::TimeZone;
        let data = crate::model::Bucketed {
            buckets: vec![0; 24],
            first_bucket: 0,
        };
        for hour in 0..24 {
            let now = chrono::Local
                .with_ymd_and_hms(2026, 6, 1, hour, 30, 0)
                .single()
                .expect("valid local time");
            assert_eq!(
                current_bucket_at(&data, 1, now),
                Some(hour as usize),
                "hour {hour} should map directly to bucket index {hour}"
            );
        }
        // At hour 23, it must be Some(23), not None
        let now_23 = chrono::Local
            .with_ymd_and_hms(2026, 6, 1, 23, 59, 59)
            .single()
            .expect("valid local time");
        assert_eq!(current_bucket_at(&data, 1, now_23), Some(23));

        // When merged with factor = 2 (12 bars total):
        assert_eq!(current_bucket_at(&data, 2, now_23), Some(11));
    }

    #[test]
    fn the_last_column_survives_a_pane_that_barely_fits_it() {
        // A 40-wide pane uses the same stacked block as wide panes (the header
        // label is `COST`, not `IN(HIT)`); at this width all five columns still
        // fit at their minima, so COST must neither be dropped nor clipped to "COS".
        let usage = sample_usage();
        let (text, _) = render(Page::Tokens, &usage, 40, 30);
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
            .find(|line| line.contains("9.6M(88%)"))
            .unwrap_or_default();
        assert!(row.contains("$3.50"), "the cost value was clipped: {row}");
    }

    #[test]
    fn header_labels_sit_above_their_values() {
        let usage = sample_usage();
        // Wide panes render the stacked block: the header band names `IN OUT THINK
        // 合计 COST`, and the value row beneath it shows the same cells right-aligned
        // in the same slots, so each label and its value share a right edge.
        let (text, _) = render(Page::Tokens, &usage, 100, 30);
        let header = text
            .lines()
            .find(|line| line.contains("IN") && line.contains("合计") && line.contains("COST"))
            .unwrap_or_default();
        let row = text
            .lines()
            .find(|line| line.contains("9.6M(88%)") && line.contains("400K") && line.contains("$3.50"))
            .unwrap_or_default();
        assert!(!header.is_empty(), "no header band: {text}");
        assert!(!row.is_empty(), "no value row: {text}");
        // Each cell is right-aligned in an evenly divided slot, so the label's end
        // and the value's end must fall in the same terminal column.
        let ends = |line: &str, needle: &str| {
            line.find(needle)
                .map(|byte| columns(&line[..byte]) + columns(needle))
        };
        for (label, value) in [("IN", "9.6M(88%)"), ("OUT", "400K"), ("合计", "10.0M"), ("COST", "$3.50")] {
            let header_end = ends(header, label).unwrap_or_default();
            let value_end = ends(row, value).unwrap_or_default();
            assert_eq!(
                header_end, value_end,
                "{label} and {value} do not share a right edge:\n{header}\n{row}"
            );
        }
    }

    #[test]
    fn the_token_page_replaces_the_system_band_instead_of_stacking_on_it() {
        // `t` switches the whole body: the token page owns the pane, so the
        // CPU/memory/network rows must be gone rather than survive above it.
        let usage = sample_usage();
        let system = SystemStats {
            cpu: Some(50.),
            cores: (0..12).map(|_| 50.).collect(),
            load: "1.00 1.00 1.00".into(),
            ..SystemStats::default()
        };
        let (quota, _) = render_with_system(Page::Quotas, &usage, &system, 80, 24);
        assert!(quota.contains("系统资源"), "{quota}");
        assert!(quota.contains("CPU01"), "{quota}");

        let (token, _) = render_with_system(Page::Tokens, &usage, &system, 80, 24);
        assert!(
            !token.contains("系统资源") && !token.contains("CPU01") && !token.contains("MEM"),
            "the token page must not keep the system band: {token}"
        );
        assert!(token.contains("本地消耗 · Token"), "{token}");
        // The footer row is still reserved for the switch key and version.
        assert!(token.contains("t 切换token"), "{token}");
        assert!(
            token.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
            "{token}"
        );
    }

    #[test]
    fn footer_hints_expose_clickable_regions_that_match_their_text() {
        let usage = sample_usage();
        let (text, view) = render(Page::Quotas, &usage, 80, 24);
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
                FooterAction::TogglePage => "t",
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
    fn clicking_the_footer_regions_switches_and_refreshes() {
        let usage = sample_usage();
        let (_, view) = render(Page::Quotas, &usage, 80, 24);
        // Find the region the renderer published for each action and confirm the
        // columns really do cover the letters the user reads.
        let region = |wanted: FooterAction| {
            view.footer_hits
                .iter()
                .find(|(_, _, action)| *action == wanted)
                .map(|(first, last, _)| (*first, *last))
                .unwrap_or_else(|| panic!("no region for {wanted:?}"))
        };
        let (first, last) = region(FooterAction::TogglePage);
        assert!(last > first, "the toggle hint must be more than one column");
        // A 40-column pane is narrow enough to drop the scroll reminder, but the
        // two keys that act on click must survive.
        let (_, narrow) = render(Page::Tokens, &usage, 40, 24);
        for action in [FooterAction::Refresh, FooterAction::TogglePage] {
            assert!(
                narrow.footer_hits.iter().any(|(_, _, a)| *a == action),
                "{action:?} must stay clickable on a 40-column pane"
            );
        }
    }

    #[test]
    fn current_bucket_matches_weekday_for_7_buckets() {
        use chrono::{Datelike, TimeZone};
        let data = Bucketed {
            buckets: vec![0; 7],
            first_bucket: 1,
        };
        // 2026-03-30 (Monday, 0) through 2026-04-05 (Sunday, 6)
        let dates = [
            (2026, 3, 30),
            (2026, 3, 31),
            (2026, 4, 1),
            (2026, 4, 2),
            (2026, 4, 3),
            (2026, 4, 4),
            (2026, 4, 5),
        ];
        for (year, month, day) in dates {
            let dt = chrono::Local
                .with_ymd_and_hms(year, month, day, 12, 0, 0)
                .single()
                .expect("valid");
            let expected = dt.weekday().num_days_from_monday() as usize;
            assert_eq!(
                current_bucket_at(&data, 1, dt),
                Some(expected),
                "mismatch for weekday {:?}",
                dt.weekday()
            );
        }
    }

    #[test]
    fn axis_origin_is_pure_zero_and_fixed_quota_ticks_are_integers() {
        assert_eq!(axis_label(0., 1e6, "M"), "0");
        assert_eq!(axis_label(0., 1e9, "B"), "0");
        assert_eq!(axis_label(0., 1e3, "K"), "0");
        assert_eq!(axis_label(0., 1., ""), "0");
        assert_eq!(axis_label(DAILY_QUOTA as f64, 1e6, "M"), "300M");
        assert_eq!(axis_label(WEEKLY_QUOTA as f64, 1e9, "B"), "2B");
        assert_eq!(axis_label(MONTHLY_QUOTA as f64, 1e9, "B"), "8B");
    }

    #[test]
    fn histogram_renders_top_title_and_flush_first_tick() {
        let data = Bucketed {
            buckets: vec![100; 24],
            first_bucket: 0,
        };
        let lines = histogram("当日", &data, DAILY_QUOTA, 80, 4);
        // 1 title + 4 bars + 1 baseline + 1 ticks = 7
        assert_eq!(lines.len(), 7);
        let title_line = lines[0].to_string();
        assert!(
            title_line.contains("当日 · 24小时 (配额 3亿)"),
            "title line mismatch: {title_line}"
        );
        let baseline = lines[5].to_string();
        assert!(baseline.contains('└') && baseline.contains('0'));
        let tick_line = lines[6].to_string();
        let gutter = " ".repeat(AXIS_WIDTH + 1);
        assert!(
            tick_line.starts_with(&format!("{gutter}0")),
            "tick line should have first tick flush without extra indentation: {tick_line:?}"
        );
    }

    #[test]
    fn the_axis_scale_follows_the_magnitude() {
        // The suffix is chosen so the label stays short at every magnitude.
        assert_eq!(axis_scale(900), (1., ""));
        assert_eq!(axis_scale(7_000), (1e3, "K"));
        assert_eq!(axis_scale(320_000_000), (1e6, "M"));
        assert_eq!(axis_scale(126_300_000_000), (1e9, "B"));
        // One decimal below ten, integers above; the `0` label has no decimals.
        assert_eq!(axis_label(1_200_000_000., 1e9, "B"), "1.2B");
        assert_eq!(axis_label(126_300_000_000., 1e9, "B"), "126B");
        assert_eq!(axis_label(320_000_000., 1e6, "M"), "320M");
        assert_eq!(axis_label(0., 1e3, "K"), "0");
    }

    #[test]
    fn a_period_chart_is_stretched_across_the_pane() {
        let usage = sample_usage();
        let (narrow, _) = render(Page::Tokens, &usage, 60, 44);
        let (wide, _) = render(Page::Tokens, &usage, 140, 44);
        // The year histogram owns 12 buckets; on a wider pane they must claim
        // more columns instead of hugging the left edge.
        let bar_run = |text: &str| {
            text.lines()
                .map(|line| line.chars().filter(|c| *c == '█').count())
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
    fn visual_render_check_token_page() {
        let usage = sample_usage();
        let (text, _) = render(Page::Tokens, &usage, 100, 44);
        println!("\n=== Rendered Token Page ===\n{text}\n===========================\n");
        assert!(text.contains("当日 · 24小时 (配额 3亿)"));
        assert!(text.contains("本周 · 7天 (配额 20亿)"));
        assert!(text.contains("当月 · 30天 (配额 80亿)"));
        assert!(text.contains("300M ┤"));
        assert!(text.contains("2B ┤"));
        assert!(text.contains("8B ┤"));
        assert!(text.contains("0 └"));
    }

    #[test]
    fn an_empty_bucket_is_drawn_as_empty_track() {
        let usage = UsageStats {
            day: Bucketed {
                buckets: vec![0; 24],
                first_bucket: 0,
            },
            ..UsageStats::default()
        };
        let (text, _) = render(Page::Tokens, &usage, 80, 24);
        assert!(
            !text.contains('█'),
            "no bucket has data, so no full bars: {text}"
        );
    }
    #[test]
    fn model_table_renders_in_hit_column_and_no_cache_column() {
        let usage = sample_usage();
        let (text, _) = render(Page::Tokens, &usage, 100, 30);
        // The stacked block headers are `IN OUT THINK 合计 COST`; the hit ratio
        // rides inside the IN value cell rather than a separate labelled column.
        assert!(text.contains("IN"), "{text}");
        assert!(!text.contains("CACHE"), "{text}");
        assert!(!text.contains("HIT%"), "{text}");
        assert!(text.contains("9.6M(88%)"), "{text}");
    }
}
