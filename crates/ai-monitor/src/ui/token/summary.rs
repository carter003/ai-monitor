//! Equal-width summary cards. Only presentation changes; totals and money
//! formatting still come from the existing usage model and token panel.

use super::{CYAN, GAP, INK, MUTED, columns, compact, money, truncate};
use crate::model::{UsageStats, UsageTotal};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

#[cfg(test)]
mod tests;

const MIN_CARD_WIDTH: usize = 22;
const OUTER_PADDING: usize = 2;
const INNER_PADDING: usize = 2;
const CARD_HEIGHT: usize = 6;
const UNIT_WIDTH: usize = 6;
const BORDER: Color = Color::Rgb(183, 201, 209);
const BACKGROUND: Color = Color::White;

pub(super) fn lines(usage: &UsageStats, width: usize) -> Vec<Line<'static>> {
    // Below this width a complete Chinese title and frame cannot coexist.
    if width < 18 {
        return super::summary_lines(usage, width);
    }
    let totals = [
        ("当日", &usage.day_total),
        ("本周", &usage.week_total),
        ("本月", &usage.month_total),
        ("历史累计", &usage.all_total),
    ];
    // One shared value column keeps TOKENS/COST aligned even when the four
    // periods have different magnitudes. Do not hard-code dollar widths.
    let value_width = totals
        .iter()
        .map(|(_, total)| columns(&compact(total.tokens)).max(columns(&money(total.cost))))
        .max()
        .unwrap_or(0)
        .max(7);
    let needed = MIN_CARD_WIDTH.max(2 + INNER_PADDING * 2 + value_width + GAP + UNIT_WIDTH);
    let available = width - OUTER_PADDING * 2;
    let count = [4usize, 2, 1]
        .into_iter()
        .find(|count| needed * count + GAP * (count - 1) <= available)
        .unwrap_or(1);
    let card_width = (available - GAP * (count - 1)) / count;
    let used = card_width * count + GAP * (count - 1);
    let left = (width - used) / 2;
    let right = width - used - left;
    let mut result = Vec::new();
    for group in totals.chunks(count) {
        if !result.is_empty() {
            result.push(Line::raw(""));
        }
        let cards: Vec<_> = group
            .iter()
            .map(|(label, total)| card(label, total, card_width, value_width))
            .collect();
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

fn card(
    label: &str,
    total: &UsageTotal,
    width: usize,
    value_width: usize,
) -> [Line<'static>; CARD_HEIGHT] {
    let border = Style::default().fg(BORDER).bg(BACKGROUND);
    let title = truncate(label, width - 5);
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
            format!(" {}╮", "─".repeat(width - 5 - columns(&title))),
            border,
        ),
    ]);
    let blank = body_row(Vec::new(), width);
    [
        top,
        blank.clone(),
        metric(&compact(total.tokens), "TOKENS", width, value_width, INK),
        metric(&money(total.cost), "COST", width, value_width, CYAN),
        blank,
        // The grid combines spans, so edge styles must live on the span.
        Line::from(Span::styled(format!("╰{}╯", "─".repeat(width - 2)), border)),
    ]
}

fn metric(
    value: &str,
    unit: &str,
    width: usize,
    value_width: usize,
    color: Color,
) -> Line<'static> {
    let room = width - 2 - INNER_PADDING * 2;
    let shown = truncate(value, room);
    let mut spans = vec![Span::styled(
        shown.clone(),
        Style::default()
            .fg(color)
            .bg(BACKGROUND)
            .add_modifier(Modifier::BOLD),
    )];
    // Hide secondary labels before shortening the numbers on very narrow
    // cards. Any unavoidable truncation uses the existing explicit ellipsis.
    if value_width + GAP + UNIT_WIDTH <= room {
        spans.push(Span::styled(
            format!("{}{unit}", " ".repeat(value_width - columns(&shown) + GAP)),
            Style::default().fg(MUTED).bg(BACKGROUND),
        ));
    }
    body_row(spans, width)
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
        format!("{}│", " ".repeat(width - 2 - INNER_PADDING - used)),
        border,
    ));
    Line::from(spans)
}
