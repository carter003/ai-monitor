//! Compact quota groups. All meters share column widths; narrow panes drop
//! decoration before data. This module only renders existing source state.
use super::{AMBER, CYAN, GREEN, INK, MUTED, age, columns, percent, quota_color, truncate};
use crate::model::{Card, Meter, SourceState, countdown};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};

#[cfg(test)]
mod tests;

const GROUP_BG: Color = Color::Rgb(233, 240, 244);
const BALANCE_BG: Color = Color::Rgb(234, 244, 240);
const BAR_TRACK: Color = Color::Rgb(216, 226, 231);

#[derive(Clone, Copy)]
struct Columns {
    margin: usize,
    width: usize,
    label: usize,
    value: usize,
    reset: usize,
    bar: usize,
    stacked: bool,
}

impl Columns {
    fn new(width: u16, states: &[SourceState], now: i64) -> Self {
        let margin = usize::from(width >= 3);
        let width = (width as usize).saturating_sub(2 * margin);
        let meters = || states.iter().flat_map(|s| &s.cards).flat_map(|c| &c.meters);
        let label = meters()
            .map(|m| columns(&m.label))
            .max()
            .unwrap_or(0)
            .clamp(4, 8);
        let value = 7; // Includes 100.00%; never drop official decimal places.
        let reset = meters()
            .map(|m| columns(&reset_text(m, now)))
            .max()
            .unwrap_or(0)
            .max(7);
        let stacked = width < label + value + reset + 2;
        let room = width.saturating_sub(label + value + reset + 3);
        let bar = if width >= 34 && room >= 8 { room } else { 0 };
        Self {
            margin,
            width,
            label,
            value,
            reset,
            bar,
            stacked,
        }
    }

    fn line(self, mut spans: Vec<Span<'static>>) -> Line<'static> {
        spans.insert(0, Span::raw(" ".repeat(self.margin)));
        spans.push(Span::raw(" ".repeat(self.margin)));
        Line::from(spans)
    }

    fn row(
        self,
        label: &str,
        value: &str,
        bar: Vec<Span<'static>>,
        reset: &str,
        value_style: Style,
    ) -> Line<'static> {
        let muted = Style::default().fg(MUTED);
        let mut spans = vec![
            cell(label, self.label, Alignment::Center, muted),
            Span::raw(" "),
            cell(value, self.value, Alignment::Right, value_style),
        ];
        if self.bar > 0 {
            spans.push(Span::raw(" "));
            spans.extend(bar);
            spans.push(Span::raw(" "));
        } else {
            let gap = self
                .width
                .saturating_sub(self.label + 1 + self.value + self.reset);
            spans.push(Span::raw(" ".repeat(gap)));
        }
        spans.push(cell(reset, self.reset, Alignment::Right, muted));
        self.line(spans)
    }
}

// Use terminal columns, not bytes or scalar counts. Keep untrusted labels and
// errors on their assigned row (including when they contain control characters).
fn cell(text: &str, width: usize, alignment: Alignment, style: Style) -> Span<'static> {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = truncate(&text, width);
    let padding = width.saturating_sub(columns(&text));
    let left = match alignment {
        Alignment::Right => padding,
        Alignment::Center => padding / 2,
        Alignment::Left => 0,
    };
    Span::styled(
        format!("{}{}{}", " ".repeat(left), text, " ".repeat(padding - left)),
        style,
    )
}

fn reset_text(meter: &Meter, now: i64) -> String {
    if meter.expired(now) {
        "待刷新".into()
    } else if let Some(at) = meter.resets_at {
        countdown(at.saturating_sub(now))
    } else if meter.available {
        "可用".into()
    } else {
        "—".into()
    }
}

pub(super) fn panel(frame: &mut Frame, area: Rect, states: &[SourceState], now: i64) -> Rect {
    let title = " AI 额度 · 剩余 ";
    let count = format!(
        " {} 项 ",
        states.iter().map(|s| s.cards.len()).sum::<usize>()
    );
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title)
        .title_style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD))
        .border_style(Style::default().fg(CYAN));
    if area.width as usize >= columns(title) + columns(&count) + 4 {
        block = block.title(Line::styled(count, Style::default().fg(MUTED)).right_aligned());
    }
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    let cols = Columns::new(inner.width, states, now);
    // Keep headings visible while the existing shared list scrolls. On tiny
    // panes use every available row for data, not a clipped column legend.
    if inner.height >= 2 && !cols.stacked {
        let bar = vec![cell(
            "进度",
            cols.bar,
            Alignment::Center,
            Style::default().fg(MUTED),
        )];
        let heading = cols.row("周期", "剩余", bar, "距重置", Style::default().fg(MUTED));
        frame.render_widget(
            Paragraph::new(heading),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        inner.y += 1;
        inner.height -= 1;
    }
    inner
}

fn stale(state: &SourceState, now: i64) -> bool {
    let held = state.hold_until.is_some_and(|t| t > now);
    state.error.is_some()
        || (!held
            && state.fetched_at.is_some_and(|t| {
                now.saturating_sub(t).max(0) as u64
                    > state.refresh_interval.saturating_mul(2).as_secs()
            }))
}

fn heading(
    state: &SourceState,
    card: &Card,
    cols: Columns,
    now: i64,
    is_stale: bool,
) -> Line<'static> {
    let mut status = if state.refreshing {
        "刷新中".into()
    } else if let Some(until) = state.hold_until.filter(|t| *t > now) {
        format!("等{}", countdown(until.saturating_sub(now)))
    } else if let Some(at) = state.fetched_at {
        format!(
            "{}{}前",
            if is_stale { "旧 " } else { "" },
            age(now.saturating_sub(at))
        )
    } else {
        "未连接".into()
    };
    if columns(&status) + 5 > cols.width {
        status = if is_stale {
            "旧"
        } else if state.refreshing {
            "刷新"
        } else if state.hold_until.is_some_and(|t| t > now) {
            "等重置"
        } else if state.fetched_at.is_none() {
            "未连接"
        } else {
            ""
        }
        .into();
        if columns(&status) + 5 > cols.width {
            status.clear();
        }
    }
    let status_width = columns(&status);
    let name_width = cols
        .width
        .saturating_sub(status_width + usize::from(!status.is_empty()));
    let (name, detail) = if let Some((name, detail)) = card.title.split_once(" · ") {
        (name, format!(" · {detail}"))
    } else if let Some((name, detail)) = card
        .title
        .strip_suffix(')')
        .and_then(|s| s.split_once(" ("))
    {
        (name, format!(" · {detail}"))
    } else {
        (card.title.as_str(), String::new())
    };
    let primary_width = columns(name).min(name_width);
    let bg = Style::default().bg(GROUP_BG);
    cols.line(vec![
        cell(
            name,
            primary_width,
            Alignment::Left,
            bg.fg(INK).add_modifier(Modifier::BOLD),
        ),
        cell(
            &detail,
            name_width - primary_width,
            Alignment::Left,
            bg.fg(MUTED),
        ),
        Span::styled(if status.is_empty() { "" } else { " " }, bg),
        cell(
            &status,
            status_width,
            Alignment::Right,
            bg.fg(if is_stale { AMBER } else { MUTED }),
        ),
    ])
}

fn meter_lines(meter: &Meter, cols: Columns, now: i64, is_stale: bool) -> Vec<Line<'static>> {
    let remaining = meter.remaining.filter(|v| v.is_finite());
    let value = remaining
        .map(|v| percent(v, meter.decimals))
        .unwrap_or_else(|| "—".into());
    let reset = reset_text(meter, now);
    let color = if is_stale || meter.expired(now) {
        MUTED
    } else {
        quota_color(remaining)
    };
    let value_style = Style::default().fg(color).add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(MUTED);
    if cols.stacked {
        // At the smallest supported terminal sizes, a reset gets its own row
        // rather than being clipped or allowed to overwrite the percentage.
        let value_width = columns(&value).min(cols.width);
        let mut rows = Vec::new();
        if cols.width > columns(&meter.label) + value_width {
            rows.push(cols.line(vec![
                cell(
                    &meter.label,
                    cols.width - value_width,
                    Alignment::Left,
                    muted,
                ),
                cell(&value, value_width, Alignment::Right, value_style),
            ]));
        } else {
            rows.push(cols.line(vec![cell(&meter.label, cols.width, Alignment::Left, muted)]));
            rows.push(cols.line(vec![cell(
                &value,
                cols.width,
                Alignment::Right,
                value_style,
            )]));
        }
        let reset = if columns(&reset) <= cols.width {
            reset
        } else {
            "重置…".into()
        };
        rows.push(cols.line(vec![cell(&reset, cols.width, Alignment::Right, muted)]));
        return rows;
    }
    let filled = remaining
        .map_or(0, |v| {
            (v.clamp(0., 100.) * cols.bar as f64 / 100.).floor() as usize
        })
        .min(cols.bar);
    let bar = vec![
        Span::styled("━".repeat(filled), Style::default().fg(color)),
        Span::styled(
            "━".repeat(cols.bar - filled),
            Style::default().fg(BAR_TRACK),
        ),
    ];
    vec![cols.row(&meter.label, &value, bar, &reset, value_style)]
}

fn balance_line(balance: f64, cols: Columns, is_stale: bool) -> Line<'static> {
    let color = if is_stale || !balance.is_finite() {
        MUTED
    } else if balance < 1. {
        AMBER
    } else {
        GREEN
    };
    let mut amount = if balance.is_finite() {
        format!("${balance:.2}")
    } else {
        "—".into()
    };
    if columns(&amount) > cols.width {
        amount = format!("${balance:.2e}");
    }
    if columns(&amount) > cols.width {
        amount = "—".into();
    }
    let currency = if cols.width >= columns(&amount) + 4 {
        " USD"
    } else {
        ""
    };
    let left = cols
        .width
        .saturating_sub(columns(&amount) + columns(currency));
    let label = if left >= 9 {
        "可用余额"
    } else if left >= 5 {
        "余额"
    } else {
        ""
    };
    let bg = Style::default().bg(BALANCE_BG);
    cols.line(vec![
        cell(label, left, Alignment::Left, bg.fg(MUTED)),
        Span::styled(amount, bg.fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(currency, bg.fg(MUTED)),
    ])
}

pub(super) fn lines(states: &[SourceState], width: u16, now: i64) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let cols = Columns::new(width, states, now);
    let mut lines = Vec::new();
    for state in states {
        let is_stale = stale(state, now);
        for card in &state.cards {
            lines.push(heading(state, card, cols, now, is_stale));
            if let Some(error) = &state.error {
                lines.push(cols.line(vec![cell(
                    error,
                    cols.width,
                    Alignment::Left,
                    Style::default().fg(AMBER),
                )]));
            } else if card.meters.is_empty() && card.balance.is_none() && card.note.is_none() {
                let message = if state.refreshing {
                    "正在读取额度…"
                } else {
                    "暂无可用额度"
                };
                lines.push(cols.line(vec![cell(
                    message,
                    cols.width,
                    Alignment::Left,
                    Style::default().fg(MUTED),
                )]));
            }
            for meter in &card.meters {
                lines.extend(meter_lines(meter, cols, now, is_stale));
            }
            if let Some(balance) = card.balance {
                lines.push(balance_line(balance, cols, is_stale));
            }
            if let Some(note) = &card.note {
                lines.push(cols.line(vec![cell(
                    note,
                    cols.width,
                    Alignment::Left,
                    Style::default().fg(MUTED),
                )]));
            }
        }
    }
    lines
}

#[cfg(test)]
pub(super) fn print_preview_fixtures() {
    tests::print_preview_fixtures();
}
