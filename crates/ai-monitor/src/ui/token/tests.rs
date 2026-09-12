use super::*;
use crate::model::{Bucketed, ModelUsage, UsageStats, UsageTotal};
use ratatui::{Terminal, backend::TestBackend};

fn usage() -> UsageStats {
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
            buckets: vec![1_000_000; 96],
            costs: vec![0.01; 96],
        },
        month: Bucketed {
            buckets: vec![1_000_000; 120],
            costs: vec![0.1; 120],
        },
        day_total: UsageTotal {
            tokens: 1_400_000,
            cost: 1.25,
        },
        week_total: UsageTotal {
            tokens: 4_200_000,
            cost: 6.8,
        },
        month_total: UsageTotal {
            tokens: 18_700_000_000,
            cost: 31.05,
        },
        all_total: UsageTotal {
            tokens: 126_300_000_000,
            cost: 318.62,
        },
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

#[test]
fn summaries_are_peers_above_a_full_width_model_table() {
    let usage = usage();
    let rendered = lines(&usage, 120, 40);
    let text = text(&rendered);
    let labels = rendered[0].to_string();
    for label in ["当日", "本周", "本月", "历史累计"] {
        assert!(labels.contains(label), "{labels}");
        assert_eq!(text.matches(label).count(), 1);
    }
    assert!(text.contains("126.3B") && text.contains("$318.6"));
    assert!(text.contains("deepseek/deepseek-v4-flash"));
}

#[test]
fn summaries_adapt_to_width_without_overflow() {
    let usage = usage();
    assert_eq!(summary_lines(&usage, 100).len(), 2);
    assert_eq!(summary_lines(&usage, 50).len(), 5);
    assert_eq!(summary_lines(&usage, 25).len(), 11);
    for width in 1..=220 {
        for row in summary_lines(&usage, width) {
            assert!(row.width() <= width, "{width}: {row}");
        }
    }
}

#[test]
fn model_table_keeps_total_and_cost_on_narrow_views() {
    let usage = usage();
    for width in 26..=40 {
        let rendered = text(&model_table(&usage.models, width, 6));
        assert!(rendered.contains("合计"), "{width}: {rendered}");
        assert!(rendered.contains("COST"), "{width}: {rendered}");
        assert!(rendered.contains("10.0M") && rendered.contains("$3.50"));
    }
}

#[test]
fn axis_labels_remain_compact_and_readable() {
    assert_eq!(axis_scale(900.), (1., ""));
    assert_eq!(axis_scale(7_000.), (1e3, "K"));
    assert_eq!(axis_scale(320_000_000.), (1e6, "M"));
    assert_eq!(axis_scale(126_300_000_000.), (1e9, "B"));
    assert_eq!(axis_label(1_200_000_000., 1e9, "B"), "1.2B");
    assert_eq!(value_label(0.25, 0.5, true), "$0.25");
    assert_eq!(span_label(15 * 60), "15分钟");
    assert_eq!(span_label(6 * 3600), "6小时");
    assert_eq!(span_label(86400), "1天");
}

#[test]
fn today_uses_sparse_clock_ticks_instead_of_every_bucket() {
    let usage = usage();
    let charts = local_charts(&usage);
    let ticks = tick_indices(&charts[0], 96);
    let labels: Vec<_> = ticks.iter().map(|(_, label)| label.as_str()).collect();
    assert_eq!(labels, ["0", "3", "6", "9", "12", "15", "18", "21", "23"]);
    assert_eq!(ticks[0].0, 0);
    assert_eq!(ticks.last().unwrap().0, 92);
}

#[test]
fn month_uses_sparse_date_ticks() {
    let usage = usage();
    let charts = local_charts(&usage);
    let ticks = tick_indices(&charts[1], 120);
    let labels: Vec<_> = ticks.iter().map(|(_, label)| label.as_str()).collect();
    assert_eq!(labels, ["1", "5", "10", "15", "20", "25", "30"]);
    assert_eq!(ticks[0].0, 0);
    assert_eq!(ticks.last().unwrap().0, 116);
}

#[test]
fn monthly_money_is_aggregated_to_one_point_per_day() {
    let usage = usage();
    let charts = local_charts(&usage);
    let values = chart_values(&charts[2], 12);
    assert_eq!(values.len(), 12);
    assert!(values.iter().all(|value| (*value - 0.4).abs() < f64::EPSILON));
}

#[test]
fn charts_render_as_braille_lines_without_histogram_glyphs() {
    let usage = usage();
    let charts = local_charts(&usage);
    let mut terminal = Terminal::new(TestBackend::new(140, 15)).unwrap();
    terminal
        .draw(|frame| draw_line_chart(frame, frame.area(), &charts[0], 12))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert!(buffer.content.iter().any(|cell| {
        cell.symbol()
            .chars()
            .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
    }));
    assert!(!buffer.content.iter().any(|cell| {
        cell.symbol()
            .chars()
            .any(|ch| "█▌▐▁▂▃▄▅▆▇".contains(ch))
    }));
}

#[test]
fn chart_rows_fit_across_common_terminal_widths() {
    let usage = usage();
    let charts = local_charts(&usage);
    for width in [40u16, 60, 80, 100, 140, 180] {
        let mut terminal = Terminal::new(TestBackend::new(width, 15)).unwrap();
        terminal
            .draw(|frame| draw_line_chart(frame, frame.area(), &charts[0], 12))
            .unwrap();
    }
}
