use super::*;
use crate::model::{UsageStats, UsageTotal};
use ratatui::{Terminal, backend::TestBackend, layout::Rect, widgets::Paragraph};

fn usage() -> UsageStats {
    UsageStats {
        day_total: UsageTotal {
            tokens: 1_000_000_000,
            cost: 146.3,
        },
        week_total: UsageTotal {
            tokens: 4_200_000_000,
            cost: 548.8,
        },
        month_total: UsageTotal {
            tokens: 4_200_000_000,
            cost: 548.8,
        },
        all_total: UsageTotal {
            tokens: 4_200_000_000,
            cost: 548.8,
        },
        running_days: 4,
        ..UsageStats::default()
    }
}

fn text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn positions(row: &str, needle: &str) -> Vec<usize> {
    row.match_indices(needle)
        .map(|(byte, _)| columns(&row[..byte]))
        .collect()
}

#[test]
fn full_width_uses_five_compact_cards_without_unit_labels() {
    let rows = lines(&usage(), 120);
    let rendered = text(&rows);
    assert_eq!(rows.len(), CARD_HEIGHT);
    assert_eq!(rows[0].to_string().matches('╭').count(), 5);
    for label in ["当日", "本周", "本月", "累计(4天)", "平均"] {
        assert_eq!(rendered.matches(label).count(), 1, "{rendered}");
    }
    assert!(!rendered.contains("TOKENS"));
    assert!(!rendered.contains("COST"));
    assert!(!rendered.contains("历史累计"));
}

#[test]
fn average_is_derived_from_cumulative_total_and_running_days() {
    let rendered = text(&lines(&usage(), 120));
    // 4.2B / 4 = 1.05B -> existing compact formatter displays 1.1B.
    assert!(rendered.contains("1.1B"), "{rendered}");
    // $548.8 / 4 = $137.2; average cost is intentionally shown as an integer.
    assert!(rendered.contains("$137"), "{rendered}");
}

#[test]
fn zero_running_days_does_not_divide_by_zero() {
    let mut stats = usage();
    stats.running_days = 0;
    let rendered = text(&lines(&stats, 120));
    assert!(rendered.contains("累计(0天)"));
    assert!(rendered.contains("平均"));
    assert!(rendered.contains("$0"));
}

#[test]
fn cards_keep_compact_width_and_are_evenly_distributed() {
    let rows = lines(&usage(), 160);
    let top = rows[0].to_string();
    let starts = positions(&top, "╭");
    let ends = positions(&top, "╮");
    assert_eq!(starts.len(), 5);
    let widths: Vec<_> = starts.iter().zip(&ends).map(|(a, b)| b - a + 1).collect();
    assert!(widths.iter().all(|width| *width == MIN_CARD_WIDTH));

    let distances: Vec<_> = starts.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let min = distances.iter().copied().min().unwrap();
    let max = distances.iter().copied().max().unwrap();
    assert!(max - min <= 1, "starts={starts:?}");

    let left = starts[0];
    let right = 160 - ends.last().unwrap() - 1;
    assert!(left.abs_diff(right) <= 1);
}

#[test]
fn metric_values_are_centered_inside_each_card() {
    let rows = lines(&usage(), 120);
    let top = rows[0].to_string();
    let token_row = rows[1].to_string();
    let starts = positions(&top, "╭");
    let ends = positions(&top, "╮");
    let values = ["1.0B", "4.2B", "4.2B", "4.2B", "1.1B"];

    for ((start, end), value) in starts.iter().zip(&ends).zip(values) {
        let value_start = positions(&token_row, value)
            .into_iter()
            .find(|position| position > start && position < end)
            .unwrap();
        let left = value_start - start - 1;
        let right = end - value_start - columns(value);
        assert!(left.abs_diff(right) <= 1, "value={value}, left={left}, right={right}");
    }
}

#[test]
fn every_supported_width_stays_inside_the_terminal() {
    for width in 0..=240 {
        let rows = lines(&usage(), width);
        assert!(
            rows.iter().all(|row| row.width() <= width),
            "width={width}: {}",
            text(&rows)
        );
    }
}

#[test]
fn border_and_value_styles_remain_intact() {
    let width = 120u16;
    let rows = lines(&usage(), width as usize);
    let height = rows.len() as u16;
    let area = Rect::new(0, 0, width, height);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(rows.clone()), area))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let token_x = positions(&rows[1].to_string(), "1.0B")[0] as u16;
    let cost_x = positions(&rows[2].to_string(), "$146")[0] as u16;
    let token = &buffer[(token_x, 1)];
    let cost = &buffer[(cost_x, 2)];
    assert_eq!(token.fg, INK);
    assert_eq!(cost.fg, CYAN);
    assert!(token.modifier.contains(Modifier::BOLD));
    assert!(cost.modifier.contains(Modifier::BOLD));
}
