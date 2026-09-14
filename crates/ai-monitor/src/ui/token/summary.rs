//! Compact summary cards for local token usage.

use super::{CYAN, GAP, INK, columns, compact, money, truncate};
use crate::model::UsageStats;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

#[cfg(test)]
mod tests;

const MIN_CARD_WIDTH: usize = 20;
const OUTER_PADDING: usize = 2;
const INNER_PADDING: usize = 2;
const CARD_HEIGHT: usize = 4;
const BORDER: Color = Color::Rgb(183, 201, 209);
const BACKGROUND: Color = Color::White;

#[derive(Clone)]
struct SummaryItem {
    label: String,
    tokens: String,
    cost: String,
}

fn items(usage: &UsageStats) -> [SummaryItem; 5] {
    let days = usage.running_days;
    let average_tokens = usage.all_total.tokens.checked_div(days).unwrap_or(0);
    let average_cost = if days == 0 {
        0
    } else {
        (usage.all_total.cost / days as f64).round().max(0.0) as u64
    };

    [
        SummaryItem {
            label: "当日".into(),
            tokens: compact(usage.day_total.tokens),
            cost: money(usage.day_total.cost),
        },
        SummaryItem {
            label: "本周".into(),
            tokens: compact(usage.week_total.tokens),
            cost: money(usage.week_total.cost),
        },
        SummaryItem {
            label: "本月".into(),
            tokens: compact(usage.month_total.tokens),
            cost: money(usage.month_total.cost),
        },
        SummaryItem {
            label: format!("累计({days}天)"),
            tokens: compact(usage.all_total.tokens),
            cost: money(usage.all_total.cost),
        },
        SummaryItem {
            label: "平均".into(),
            tokens: compact(average_tokens),
            cost: format!("${average_cost}"),
        },
    ]
}

pub(super) fn lines(usage: &UsageStats, width: usize) -> Vec<Line<'static>> {
    if width < 18 {
        return super::summary_lines(usage, width);
    }

    let totals = items(usage);
    let value_width = totals
        .iter()
        .map(|item| columns(&item.tokens).max(columns(&item.cost)))
        .max()
        .unwrap_or(0);
    let title_width = totals
        .iter()
        .map(|item| columns(&item.label))
        .max()
        .unwrap_or(0);
    let needed = MIN_CARD_WIDTH
        .max(value_width + INNER_PADDING * 2 + 2)
        .max(title_width + 5);
    let available = width.saturating_sub(OUTER_PADDING * 2);
    let count = [5usize, 3, 2, 1]
        .into_iter()
        .find(|count| needed * count + GAP * (count - 1) <= available)
        .unwrap_or(1);
    let card_width = needed.min(available.max(1));
    let used = card_width * count + GAP * (count - 1);
    let left = width.saturating_sub(used) / 2;
    let right = width.saturating_sub(used + left);

    let mut result = Vec::new();
    for group in totals.chunks(count) {
        if !result.is_empty() {
            result.push(Line::raw(""));
        }
        let cards: Vec<_> = group.iter().map(|item| card(item, card_width)).collect();
        for row in 0..CARD_HEIGHT {
            let mut spans = vec![Span::raw(" ".repeat(left))];
            for (column, card) in cards.iter().enumerate() {
                if column > 0 {
                    spans.push(Span::raw(" ".repeat(GAP)));
                }
                spans.extend(card[row].spans.clone());
            }
            spans.push(Span::raw(" ".repeat(right)));
            result.push(Line::from(spans));
        }
    }
    result
}

fn card(item: &SummaryItem, width: usize) -> [Line<'static>; CARD_HEIGHT] {
    let border = Style::default().fg(BORDER).bg(BACKGROUND);
    let title = truncate(&item.label, width.saturating_sub(5));
    let top = Line::from(vec![
        Span::styled("╭─ ", border),
        Span::styled(
            title.clone(),
            Style::default()
                .fg(CYAN)
                .bg(BACKGROUND)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " {}╮",
                "─".repeat(width.saturating_sub(5 + columns(&title)))
            ),
            border,
        ),
    ]);
    [
        top,
        metric(&item.tokens, width, INK),
        metric(&item.cost, width, CYAN),
        Line::from(Span::styled(
            format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
            border,
        )),
    ]
}

fn metric(value: &str, width: usize, color: Color) -> Line<'static> {
    let room = width.saturating_sub(2 + INNER_PADDING * 2);
    let shown = truncate(value, room);
    body_row(
        vec![Span::styled(
            shown,
            Style::default()
                .fg(color)
                .bg(BACKGROUND)
                .add_modifier(Modifier::BOLD),
        )],
        width,
    )
}

fn body_row(content: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let used: usize = content.iter().map(Span::width).sum();
    let border = Style::default().fg(BORDER).bg(BACKGROUND);
    let mut spans = vec![Span::styled(
        format!("│{}", " ".repeat(INNER_PADDING)),
        border,
    )];
    spans.extend(content);
    spans.push(Span::styled(
        format!(
            "{}│",
            " ".repeat(width.saturating_sub(2 + INNER_PADDING + used))
        ),
        border,
    ));
    Line::from(spans)
}
