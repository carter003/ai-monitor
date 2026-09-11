use super::*;
use crate::model::{Bucketed, ModelUsage, UsageStats, UsageTotal};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

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

fn hourly(values: &[u64]) -> Chart<'_> {
    Chart {
        label: "今日 Token",
        series: Series::Tokens(values),
        span_seconds: 15 * 60,
        tick_buckets: 4,
        first_tick: 0,
    }
}

fn text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_chart(chart: &Chart<'_>, width: u16, rows: usize, origin: usize) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, rows as u16 + 3)).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(
                Paragraph::new(histogram(chart, width, rows, origin, 0)),
                frame.area(),
            );
        })
        .unwrap();
    terminal.backend().buffer().clone()
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
    assert!(text.find("历史累计").unwrap() < text.find("模型用量").unwrap());
    assert!(!text.contains('╔') && !text.contains('╚') && !text.contains('║'));
}

#[test]
fn summaries_use_four_two_or_one_columns_based_on_actual_width() {
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
fn all_summary_and_model_rows_fit_their_pane() {
    let mut huge = usage();
    huge.models.push(ModelUsage {
        model: "智谱-e\u{301}-a-very-long-model-name".into(),
        input_total: u64::MAX,
        output: u64::MAX,
        reasoning: u64::MAX,
        cache_read: u64::MAX,
        cost: Some(1e17),
    });
    for usage in [UsageStats::default(), usage(), huge] {
        for width in 1..=220u16 {
            for height in [1, 3, 10, 24, 50, 100] {
                for row in lines(&usage, width, height) {
                    assert!(row.width() <= width as usize, "{width}x{height}: {row}");
                }
            }
            for row in model_table(&usage.models, width as usize, 6) {
                assert!(row.width() <= width as usize, "model row {width}: {row}");
            }
        }
    }
}

#[test]
fn narrow_tables_preserve_total_and_cost_before_detail_columns() {
    let usage = usage();
    for width in 26..=40 {
        let rendered = text(&model_table(&usage.models, width, 6));
        assert!(rendered.contains("合计"), "{width}: {rendered}");
        assert!(rendered.contains("COST"), "{width}: {rendered}");
        assert!(rendered.contains("10.0M") && rendered.contains("$3.50"));
    }
    let smallest = text(&model_table(&usage.models, 26, 6));
    assert!(!smallest.contains("THINK"));
    assert!(!smallest.contains("缓存命中"));
}

#[test]
fn input_and_cache_are_distinct_columns_with_original_accounting() {
    let usage = usage();
    let rendered = text(&model_table(&usage.models, 120, 6));
    for cell in [
        "IN",
        "OUT",
        "THINK",
        "缓存命中",
        "合计",
        "COST",
        "9.6M",
        "400K",
        "120K",
        "87.50%",
        "10.0M",
        "$3.50",
    ] {
        assert!(rendered.contains(cell), "missing {cell}: {rendered}");
    }
    assert!(!rendered.contains("9.6M("));
    // Neither reasoning nor cached input is added to gross input + output.
    assert_eq!(usage.models[0].total_tokens(), 10_000_000);
}

#[test]
fn table_headers_and_values_share_right_edges() {
    let usage = usage();
    let rows = model_table(&usage.models, 120, 6);
    let header = rows[0].to_string();
    let value = rows[1].to_string();
    let end = |line: &str, needle: &str| {
        let offset = line.find(needle).unwrap();
        columns(&line[..offset]) + columns(needle)
    };
    for (label, number) in [
        ("IN", "9.6M"),
        ("OUT", "400K"),
        ("THINK", "120K"),
        ("缓存命中", "87.50%"),
        ("合计", "10.0M"),
        ("COST", "$3.50"),
    ] {
        assert_eq!(
            end(&header, label),
            end(&value, number),
            "{label}: {header}\n{value}"
        );
    }
}

#[test]
fn unknown_cost_is_not_presented_as_zero() {
    let mut usage = usage();
    usage.models[0].cost = None;
    let rows = model_table(&usage.models, 120, 6);
    let value = rows[1].to_string();
    assert!(value.contains('—'));
    assert!(!value.contains("$0.00"));
}

#[test]
fn the_historical_total_survives_compact_and_scrollable_summaries() {
    let usage = usage();
    // Four peer summary cards already fit in two rows at this width. This
    // path must not be mistaken for the three-row historical-only fallback.
    let compact = lines(&usage, 80, 2);
    assert_eq!(compact.len(), 2);
    let rendered = text(&compact);
    for value in ["当日", "本周", "本月", "历史累计", "126.3B", "$318.6"] {
        assert!(rendered.contains(value), "missing {value}: {rendered}");
    }
    assert!(!rendered.contains('╔'));

    // One available row cannot hold the summary. Keep the historical values
    // in the scrollable content rather than discarding them to fit a frame.
    let fallback = lines(&usage, 80, 1);
    assert_eq!(fallback.len(), 3);
    let rendered = text(&fallback);
    for value in ["历史累计", "126.3B tokens", "$318.6"] {
        assert!(rendered.contains(value), "missing {value}: {rendered}");
    }
    assert!(!rendered.contains('╔'));
}

#[test]
fn axis_labels_and_sub_dollar_amounts_remain_readable() {
    assert_eq!(axis_scale(900.), (1., ""));
    assert_eq!(axis_scale(7_000.), (1e3, "K"));
    assert_eq!(axis_scale(320_000_000.), (1e6, "M"));
    assert_eq!(axis_scale(126_300_000_000.), (1e9, "B"));
    assert_eq!(axis_label(1_200_000_000., 1e9, "B"), "1.2B");
    assert_eq!(axis_label(0., 1e3, "K"), "0");
    assert_eq!(value_label(0.25, 0.5, true), "$0.25");
    assert_eq!(span_label(3600), "1小时");
    assert_eq!(span_label(86400), "1天");
}

#[test]
fn no_priced_share_badge_or_decorative_frame_is_reintroduced() {
    let usage = usage();
    for height in [3, 13, 24, 38] {
        let rendered = text(&lines(&usage, 100, height));
        assert!(!rendered.contains("计价"));
        assert!(!rendered.contains('╔'));
    }
}

#[test]
fn native_buckets_never_merge_at_any_terminal_width() {
    let usage = usage();
    let charts = local_charts(&usage);
    assert_eq!(charts[0].series.len(), 96);
    assert_eq!(charts[1].series.len(), 120);
    assert_eq!(charts[2].series.len(), 120);
    for width in [40, 80, 100, 160, 260, 400] {
        for (index, chart) in charts.iter().enumerate() {
            let rendered = text(&histogram(chart, width, 4, 8, 0));
            let grain = if index == 0 { "15分钟" } else { "6小时" };
            assert!(rendered.contains(grain), "{width}: {rendered}");
            let window = Window::new(chart.series.len(), 4, width as usize - 8, 0).unwrap();
            assert_eq!(window.len, window.groups * 4);
            // Each bar is the source value, not the sum of four neighbours.
            assert_eq!(
                chart.series.value(window.start),
                if index == 2 { 0.1 } else { 1_000_000. }
            );
        }
    }
}

#[test]
fn every_native_bucket_remains_reachable_including_zero_and_last_buckets() {
    for len in [96usize, 28 * 4, 29 * 4, 30 * 4, 31 * 4] {
        for plot in [9usize, 33, 65, 101, 249, 401] {
            let mut seen = vec![false; len];
            let first = Window::new(len, 4, plot, 0).unwrap();
            for start in 0..=first.max_start {
                let window = Window::new(len, 4, plot, start).unwrap();
                assert!(window.start.is_multiple_of(4));
                assert_eq!(window.len, window.groups * 4);
                seen[window.start..window.start + window.len].fill(true);
            }
            assert!(seen.iter().all(|seen| *seen), "len {len}, plot {plot}");
            let last = Window::new(len, 4, plot, usize::MAX).unwrap();
            assert_eq!(last.start + last.len, len);
        }
    }
}

#[test]
fn ninety_six_bars_have_independent_minor_ticks_and_four_per_hour() {
    // 96 one-cell bars, 95 one-cell gaps and a blank on EACH side.
    // Expected coordinates are independent of the Geometry implementation.
    let values = vec![100u64; 96];
    let buffer = render_chart(&hourly(&values), 201, 4, 8);
    assert_eq!(buffer[(7, 5)].symbol(), "└");
    for quarter in 0..96u16 {
        let x = 9 + quarter * 2;
        assert_eq!(buffer[(x, 4)].symbol(), "█");
        assert_eq!(buffer[(x, 5)].symbol(), "┴");
        assert_eq!(buffer[(x + 1, 4)].symbol(), " ");
        if quarter.is_multiple_of(4) {
            let label = (quarter / 4).to_string();
            let left = x - label.len() as u16 / 2;
            assert_eq!(buffer[(x, 5)].fg, CYAN);
            for (offset, character) in label.chars().enumerate() {
                assert_eq!(
                    buffer[(left + offset as u16, 6)].symbol(),
                    character.to_string()
                );
            }
        } else {
            assert_eq!(buffer[(x, 5)].fg, TRACK);
        }
    }
}

#[test]
fn real_bar_extents_are_symmetric_about_ticks_even_at_awkward_widths() {
    let values = vec![100u64; 96];
    for width in [201, 202, 250, 300, 350, 400, 450, 500, 600] {
        let buffer = render_chart(&hourly(&values), width, 4, 8);
        let mut bars = Vec::new();
        let mut x = 8;
        while x < width {
            if buffer[(x, 4)].symbol() != "█" {
                x += 1;
                continue;
            }
            let left = x;
            while x < width && buffer[(x, 4)].symbol() == "█" {
                x += 1;
            }
            let right = x - 1;
            assert_eq!((right - left + 1) % 2, 1, "even width at {width}");
            let centre = (left + right) / 2;
            assert_eq!(centre - left, right - centre, "optical centre at {width}");
            assert_eq!(buffer[(centre, 5)].symbol(), "┴");
            bars.push((left, right));
        }
        assert_eq!(bars.len(), 96, "{width}: {bars:?}");
        let expected = bars[0].1 - bars[0].0;
        assert!(bars.iter().all(|(left, right)| right - left == expected));
    }
}

#[test]
fn partial_caps_and_bodies_have_equal_width_without_burr_glyphs() {
    let values = vec![3u64; 96];
    // Native value 3 / ceiling 5 * 24 eighths = 14: six-eighth cap + body.
    let buffer = render_chart(&hourly(&values), 201, 3, 8);
    for quarter in 0..96u16 {
        let x = 9 + quarter * 2;
        assert_eq!(buffer[(x, 2)].symbol(), "▆");
        assert_eq!(buffer[(x, 3)].symbol(), "█");
        assert_eq!(buffer[(x, 2)].fg, BAR_MID);
        assert_eq!(buffer[(x, 3)].fg, BAR_MID);
        assert_eq!(buffer[(x + 1, 2)].symbol(), " ");
        assert_eq!(buffer[(x + 1, 3)].symbol(), " ");
    }
    assert!(buffer.content.iter().all(|cell| cell.symbol() != "▊"));
}

#[test]
fn the_requested_colour_boundaries_are_exact() {
    for (share, colour) in [
        (0., BAR_LOW),
        (0.2999, BAR_LOW),
        (0.30, BAR_MID),
        (0.7999, BAR_MID),
        (0.80, BAR_HIGH),
        (0.9499, BAR_HIGH),
        (0.95, BAR_RED),
        (1., BAR_RED),
        (2., BAR_RED),
    ] {
        assert_eq!(bar_color(share), colour, "share {share}");
    }
    assert_eq!(BAR_LOW, Color::Rgb(144, 202, 249));
    assert_eq!(BAR_MID, Color::Rgb(33, 150, 243));
    assert_eq!(BAR_HIGH, Color::Rgb(13, 71, 161));
}

#[test]
fn buffer_bars_use_the_same_band_for_caps_and_bodies() {
    let mut values = vec![0u64; 96];
    values[..7].copy_from_slice(&[29, 30, 79, 80, 94, 95, 100]);
    let buffer = render_chart(&hourly(&values), 201, 10, 8);
    for (quarter, colour) in [
        BAR_LOW, BAR_MID, BAR_MID, BAR_HIGH, BAR_HIGH, BAR_RED, BAR_RED,
    ]
    .into_iter()
    .enumerate()
    {
        let x = 9 + quarter as u16 * 2;
        assert_eq!(buffer[(x, 10)].fg, colour);
        for row in 1..=10 {
            if !buffer[(x, row)].symbol().trim().is_empty() {
                assert_eq!(buffer[(x, row)].fg, colour);
            }
        }
    }
}

#[test]
fn monthly_money_and_tokens_share_all_four_daily_coordinates() {
    for days in [28usize, 29, 30, 31] {
        let usage = UsageStats {
            month: Bucketed {
                buckets: vec![100; days * 4],
                costs: vec![12_345.; days * 4],
            },
            ..UsageStats::default()
        };
        let charts = local_charts(&usage);
        let origin = plot_origin(&charts);
        let width = (origin + 2 * days * 4 + 1) as u16;
        let tokens = histogram(&charts[1], width, 4, origin, 0);
        let money = histogram(&charts[2], width, 4, origin, 0);
        assert_eq!(tokens[5].to_string().matches('┴').count(), days * 4);
        assert_eq!(
            tokens[5].to_string().split_once('└').unwrap().1,
            money[5].to_string().split_once('└').unwrap().1
        );
        assert_eq!(tokens[6].to_string(), money[6].to_string());
        assert!(
            tokens[6]
                .to_string()
                .split_whitespace()
                .any(|label| label == days.to_string())
        );
        for start in [0, 9, 20, usize::MAX] {
            let tokens = histogram(&charts[1], 80, 4, origin, start);
            let money = histogram(&charts[2], 80, 4, origin, start);
            assert_eq!(tokens[6].to_string(), money[6].to_string());
        }
    }
}

#[test]
fn scale_and_colours_do_not_change_when_panning_or_resizing() {
    let mut values = vec![30u64; 96];
    values[95] = 100;
    let chart = hourly(&values);
    for width in [40, 80, 201, 400] {
        for start in [0, 5, 10, usize::MAX] {
            let rendered = histogram(&chart, width, 4, 8, start);
            assert!(rendered[1].to_string().contains("100"));
        }
    }
    let mut terminal = Terminal::new(TestBackend::new(41, 7)).unwrap();
    terminal
        .draw(|f| f.render_widget(Paragraph::new(histogram(&chart, 41, 4, 8, 5)), f.area()))
        .unwrap();
    assert_eq!(terminal.backend().buffer()[(9, 4)].fg, BAR_MID);
    assert_eq!(nice_ceiling(series_peak(&chart.series)), 100.);
}

#[test]
fn zero_missing_and_tiny_windows_never_invent_bars_or_merge_values() {
    let zeros = vec![0u64; 96];
    let chart = hourly(&zeros);
    let buffer = render_chart(&chart, 100, 8, 8);
    assert!(
        buffer
            .content
            .iter()
            .all(|cell| !cell.symbol().chars().any(|c| "█▊▁▂▃▄▅▆▇".contains(c)))
    );
    assert!(histogram(&hourly(&[]), 100, 4, 8, 0).is_empty());
    assert!(histogram(&chart, 8, 4, 8, 0).is_empty());
    assert!(histogram(&chart, 100, 0, 8, 0).is_empty());
    assert!(Window::new(96, 4, 8, 0).is_none());
    assert!(Geometry::new(8, 8, 96).is_none());
    assert!(Geometry::new(40, 8, 96).is_none());
}

#[test]
fn chart_rows_fit_at_all_sizes_and_keep_all_available_height() {
    let values: Vec<_> = (1..=96).map(|value| value * 1_000_000).collect();
    for width in 0..=420 {
        for rows in [0, 1, 2, 3, 7, 9, 15, 30] {
            let rendered = histogram(&hourly(&values), width, rows, 8, 0);
            for line in &rendered {
                assert!(line.width() <= width as usize, "{width}x{rows}: {line}");
            }
            if width >= 17 && rows > 0 {
                assert_eq!(rendered.len(), rows + 3);
            }
        }
    }
    for room in 0..=60 {
        assert_eq!(chart_rows(room), room.saturating_sub(4) as usize);
    }
}

#[test]
fn navigation_is_bounded_and_monthly_charts_share_one_day_offset() {
    let mut view = View {
        chart_starts: [8, 10],
        max_chart_starts: [12, 20],
        ..View::default()
    };
    view.pan_charts(false);
    assert_eq!(view.chart_starts, [7, 9]);
    assert!(view.chart_manual);
    for _ in 0..40 {
        view.pan_charts(true);
    }
    assert_eq!(view.chart_starts, [12, 20]);
    for _ in 0..40 {
        view.pan_charts(false);
    }
    assert_eq!(view.chart_starts, [0, 0]);
    let mut empty = View::default();
    empty.pan_charts(true);
    assert!(!empty.chart_manual);
}

#[test]
fn window_preserves_a_partial_final_group_without_summing_it() {
    let window = Window::new(123, 4, 65, usize::MAX).unwrap();
    assert_eq!(window.start + window.len, 123);
    assert_eq!(window.len % 4, 3);
}
