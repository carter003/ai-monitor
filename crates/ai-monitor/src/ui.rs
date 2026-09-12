use crate::{
    model::{SourceState, UsageStats},
    system::SystemStats,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
};

mod quota;
mod resources;
#[cfg(test)]
mod tests;
mod token;

use resources::draw_system;

const CYAN: Color = Color::Rgb(11, 93, 107);
const MUTED: Color = Color::Rgb(74, 84, 95);
const GREEN: Color = Color::Rgb(10, 92, 40);
const AMBER: Color = Color::Rgb(124, 67, 0);
const RED: Color = Color::Rgb(163, 22, 22);
const TRACK: Color = Color::Rgb(156, 167, 179);
const INK: Color = Color::Rgb(17, 24, 39);

#[derive(Default)]
pub struct View {
    /// Actual address of the web listener owned by this process.
    pub web_address: Option<std::net::SocketAddr>,
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

    // Keep sidebar widths and the local token layout independent of the
    // resource panel's responsive core grid. Lists retain their shared scroll.
    let columns = Layout::horizontal([
        Constraint::Length(sidebar_width(area.width)),
        Constraint::Min(0),
    ])
    .split(body);
    let system_height = resources::height(body.height, columns[0].width, system.cores.len());
    let sidebar =
        Layout::vertical([Constraint::Length(system_height), Constraint::Min(0)]).split(columns[0]);
    draw_system(frame, sidebar[0], system);

    let quota_area = sidebar[1];
    let token_area = columns[1];
    let quota_inner = quota::panel(frame, quota_area, states, now);
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
        token::draw_charts(frame, &token_parts[1..], usage, now);
    }

    let web_url = view.web_address.map(|address| format!("http://{address}"));
    // Preserve usable quit/refresh hints on narrow terminals; never truncate
    // the URL into a misleading address.
    let web_url = web_url.filter(|url| area.width as usize >= url.len() + version.len() + 16);
    let web_width = web_url.as_ref().map_or(0, |url| url.len() + 2);
    let room = (area.width as usize).saturating_sub(version.len() + web_width + 1);
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
    let bar = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(web_width as u16),
        Constraint::Length(version.len() as u16),
    ])
    .split(parts[1]);
    view.footer_row = Some(parts[1].y);
    view.body = Some(Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: parts[1].y.saturating_sub(area.y),
    });
    frame.render_widget(Paragraph::new(Line::from(hint_spans)), bar[0]);
    if let Some(url) = web_url {
        frame.render_widget(Paragraph::new(url).style(Style::default().fg(CYAN)), bar[1]);
    }
    frame.render_widget(
        Paragraph::new(version)
            .style(Style::default().fg(MUTED))
            .right_aligned(),
        bar[2],
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
    // Group backgrounds separate accounts without spending extra screen rows.
    quota::lines(states, inner.width, now)
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
