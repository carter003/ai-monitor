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
    assert!(text.contains("deepseek-v4-flash"));
    assert!(!text.contains("deepseek/"));
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

fn assert_even_tick_spacing(ticks: &[(usize, String)]) {
    let gaps: Vec<_> = ticks
        .windows(2)
        .map(|pair| pair[1].0.saturating_sub(pair[0].0))
        .collect();
    let min = *gaps.iter().min().unwrap();
    let max = *gaps.iter().max().unwrap();
    assert!(max - min <= 1, "uneven tick gaps: {gaps:?}");
}

#[test]
fn today_ticks_cover_every_hour_on_an_even_axis() {
    let usage = usage();
    let charts = local_charts(&usage, 30);
    let ticks = tick_positions(&charts[0], 120);
    let labels: Vec<_> = ticks.iter().map(|(_, label)| label.as_str()).collect();
    let expected: Vec<_> = (0..24).map(|hour| hour.to_string()).collect();
    assert_eq!(
        labels,
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_even_tick_spacing(&ticks);
}

#[test]
fn september_month_axes_are_even_and_end_at_day_30() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-12T12:00:00+00:00")
        .unwrap()
        .timestamp();
    assert_eq!(days_in_month(now), 30);

    let usage = usage();
    let charts = local_charts(&usage, days_in_month(now));
    let token_ticks = tick_positions(&charts[1], 120);
    assert_eq!(token_ticks.len(), 30);
    assert_eq!(token_ticks.first().unwrap().1, "1");
    assert_eq!(token_ticks.last().unwrap().1, "30");
    assert_even_tick_spacing(&token_ticks);

    let money_ticks = tick_positions(&charts[2], 120);
    assert_eq!(money_ticks.len(), 6);
    assert_eq!(money_ticks.first().unwrap().1, "5");
    assert_eq!(money_ticks.last().unwrap().1, "30");
    assert_even_tick_spacing(&money_ticks);
}

#[test]
fn month_length_tracks_the_calendar_instead_of_elapsed_data() {
    for (date, expected) in [
        ("2026-02-12T12:00:00+00:00", 28),
        ("2028-02-12T12:00:00+00:00", 29),
        ("2026-09-12T12:00:00+00:00", 30),
        ("2026-10-12T12:00:00+00:00", 31),
    ] {
        let now = chrono::DateTime::parse_from_rfc3339(date)
            .unwrap()
            .timestamp();
        assert_eq!(days_in_month(now), expected);
    }
}

#[test]
fn monthly_money_is_aggregated_to_one_point_per_elapsed_day() {
    let usage = usage();
    let charts = local_charts(&usage, 30);
    let values = chart_values(&charts[2], 12);
    assert_eq!(values.len(), 12);
    assert!(
        values
            .iter()
            .all(|value| (*value - 0.4).abs() < f64::EPSILON)
    );
    assert_eq!(charts[2].axis_units, 30);
}

#[test]
fn stems_are_straight_and_connect_directly_to_the_x_axis() {
    let mut usage = usage();
    usage.hours.buckets.fill(0);
    usage.hours.buckets[0] = 25_000_000;
    usage.hours.buckets[4] = 50_000_000;
    usage.hours.buckets[8] = 100_000_000;
    let charts = local_charts(&usage, 30);

    let width = 140u16;
    let height = 15u16;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw_stem_chart(frame, frame.area(), &charts[0], 12))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let baseline_y = height - 2;
    let mut stem_columns = 0;

    for x in 0..width {
        let rows: Vec<_> = (1..baseline_y)
            .filter(|y| buffer[(x, *y)].symbol() == "│")
            .collect();
        if rows.is_empty() {
            continue;
        }
        stem_columns += 1;
        let top = *rows.first().unwrap();
        let bottom = *rows.last().unwrap();
        assert_eq!(bottom, baseline_y - 1, "stem at x={x} floats above axis");
        for y in top..=bottom {
            assert_eq!(buffer[(x, y)].symbol(), "│", "broken stem at x={x}, y={y}");
        }
        assert_eq!(
            buffer[(x, baseline_y)].symbol(),
            "┴",
            "stem at x={x} is not joined"
        );
    }

    assert!(stem_columns >= 3);
    assert!(!buffer.content.iter().any(|cell| {
        cell.symbol()
            .chars()
            .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
    }));
    assert!(
        !buffer
            .content
            .iter()
            .any(|cell| cell.symbol().chars().any(|ch| "█▌▐▁▂▃▄▅▆▇".contains(ch)))
    );
}

#[test]
fn chart_rows_fit_across_common_terminal_widths() {
    let usage = usage();
    let charts = local_charts(&usage, 30);
    for width in [40u16, 60, 80, 100, 140, 180] {
        let mut terminal = Terminal::new(TestBackend::new(width, 15)).unwrap();
        terminal
            .draw(|frame| draw_stem_chart(frame, frame.area(), &charts[0], 12))
            .unwrap();
    }
}

#[test]
fn consecutive_daily_money_bars_have_no_gaps_across_all_month_lengths() {
    for days in [28usize, 29, 30, 31] {
        let mut usage = usage();
        usage.month.costs = vec![0.0; days * 4];
        // Simulate continuous usage on days 10, 11, 12 (4 blocks per day)
        usage.month.costs[9 * 4] = 45.0;
        usage.month.costs[10 * 4] = 400.0;
        usage.month.costs[11 * 4] = 600.0;
        let charts = local_charts(&usage, days);
        let chart = &charts[2];

        for width in [days as u16 + 10, 50, 80, 120] {
            let axis = TimeAxis::new(chart, width as usize);
            assert_eq!(axis.width, days, "plot width must be exactly month days");
            for index in 0..days - 1 {
                let x0 = axis.slot_x(index, 1);
                let x1 = axis.slot_x(index + 1, 1);
                assert_eq!(
                    x1 - x0,
                    1,
                    "day {index} and {next} must be adjacent",
                    next = index + 1
                );
            }

            let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
            terminal
                .draw(|frame| draw_stem_chart(frame, frame.area(), chart, 12))
                .unwrap();
            let buffer = terminal.backend().buffer();
            // The baseline is at area.bottom() - 2
            let baseline_row: String = (0..width).map(|x| buffer[(x, 8)].symbol()).collect();
            assert!(
                baseline_row.contains("┴┴┴"),
                "baseline should have 3 adjacent connected stems without gaps at width {width}: {baseline_row}"
            );
        }
    }
}
