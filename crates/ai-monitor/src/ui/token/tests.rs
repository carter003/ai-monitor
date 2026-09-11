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
                Paragraph::new(histogram(chart, width, rows, origin)),
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
fn the_historical_total_survives_a_tiny_height() {
    let usage = usage();
    let rendered = text(&lines(&usage, 80, 2));
    assert!(rendered.contains("历史累计"));
    assert!(rendered.contains("126.3B tokens"));
    assert!(rendered.contains("$318.6"));
    assert!(!rendered.contains('╔'));
}

#[test]
fn default_granularity_does_not_change_on_wider_panes() {
    let usage = usage();
    let charts = local_charts(&usage);
    for width in [100, 140, 220, 400] {
        for chart in &charts {
            assert_eq!(merge_factor(chart, width - 8), 4);
        }
    }
    assert!(text(&histogram(&charts[0], 100, 4, 8)).contains("每柱 1小时"));
    assert!(text(&histogram(&charts[1], 100, 4, 8)).contains("每柱 1天"));
    assert!(text(&histogram(&charts[2], 100, 4, 8)).contains("每柱 1天"));
}

#[test]
fn merged_buckets_conserve_totals_and_use_natural_intervals() {
    for base_len in [96, 28 * 4, 29 * 4, 30 * 4, 31 * 4] {
        let data: Vec<_> = (1..=base_len as u64).collect();
        let chart = hourly(&data);
        let original: u64 = data.iter().sum();
        for plot in 1..=250 {
            let factor = merge_factor(&chart, plot);
            assert!(factor >= 4 && factor.is_multiple_of(4));
            let merged = merged_values(&chart.series, factor);
            assert_eq!(merged.iter().sum::<f64>(), original as f64);
            assert!(merged.len() <= plot.div_ceil(2));
        }
    }
    let values: Vec<_> = (0..124).map(|i| i as f64 * 0.01).collect();
    let original: f64 = values.iter().sum();
    for factor in [4, 8, 12, 20, 124] {
        let merged = merged_values(&Series::Money(&values), factor);
        assert!((merged.iter().sum::<f64>() - original).abs() < 1e-9);
    }
}

#[test]
fn partial_final_calendar_bucket_is_retained_and_labelled() {
    let values = vec![1u64; 31 * 4];
    let chart = Chart {
        label: "本月 Token",
        series: Series::Tokens(&values),
        span_seconds: 6 * 3_600,
        tick_buckets: 4,
        first_tick: 1,
    };
    let factor = merge_factor(&chart, 32);
    assert_eq!(factor, 8);
    let merged = merged_values(&chart.series, factor);
    assert_eq!(merged.len(), 16);
    assert_eq!(merged.last(), Some(&4.));
    let rendered = text(&histogram(&chart, 40, 4, 8));
    assert!(rendered.contains("2天"));
    assert!(rendered.contains("末柱不足"));
}

#[test]
fn bars_ticks_and_labels_have_the_same_actual_buffer_coordinates() {
    // 71 plot columns: 24 two-cell bars plus 23 one-cell gaps.
    // Expected positions are independent of Geometry's implementation.
    let values = vec![5u64; 96];
    let chart = hourly(&values);
    let buffer = render_chart(&chart, 79, 4, 8);
    assert_eq!(buffer[(7, 5)].symbol(), "└");
    for hour in 0..24u16 {
        let start = 8 + hour * 3;
        let centre = start + 1;
        assert_eq!(buffer[(start, 4)].symbol(), "█");
        assert_eq!(buffer[(start + 1, 4)].symbol(), "█");
        assert_eq!(buffer[(centre, 5)].symbol(), "┴");
        let label = hour.to_string();
        let left = centre - label.len() as u16 / 2;
        for (offset, character) in label.chars().enumerate() {
            assert_eq!(
                buffer[(left + offset as u16, 6)].symbol(),
                character.to_string()
            );
        }
        if hour < 23 {
            assert_eq!(buffer[(start + 2, 4)].symbol(), " ");
        }
    }
}

#[test]
fn dense_partial_caps_and_bodies_use_equal_width_full_cell_glyphs() {
    // Each hourly sum is 12; at a nice ceiling of 20 over three plot rows,
    // the second row is a partial cap and the third is a full body.
    let values = vec![3u64; 96];
    let chart = hourly(&values);
    let buffer = render_chart(&chart, 55, 3, 8);
    for hour in 0..24u16 {
        let x = 8 + hour * 2;
        assert_eq!(buffer[(x, 2)].symbol(), "▆");
        assert_eq!(buffer[(x, 3)].symbol(), "█");
        assert_eq!(buffer[(x, 2)].fg, BAR);
        assert_eq!(buffer[(x, 3)].fg, BAR);
        if hour < 23 {
            assert_eq!(buffer[(x + 1, 2)].symbol(), " ");
            assert_eq!(buffer[(x + 1, 3)].symbol(), " ");
        }
    }
    assert!(buffer.content.iter().all(|cell| cell.symbol() != "▊"));
}

#[test]
fn bar_widths_are_uniform_and_gaps_are_real_cells() {
    for plot in 1usize..=300 {
        for count in 1..=plot.div_ceil(2) {
            let geometry = Geometry::new((plot + 8) as u16, 8, count);
            assert!(geometry.bar_width >= 1);
            for index in 0..count {
                assert!(geometry.starts[index] + geometry.bar_width <= plot);
                if index + 1 < count {
                    assert!(geometry.gap_after(index) >= 1);
                }
            }
            assert_eq!(geometry.starts[0], 0);
            assert_eq!(geometry.starts[count - 1] + geometry.bar_width, plot);
        }
    }
}

#[test]
fn edge_labels_are_skipped_not_pinned_to_an_unrelated_position() {
    let geometry = Geometry::new(17, 8, 3);
    let ticks = [(0, "10".into()), (3, "3".into()), (8, "888".into())];
    let line = tick_labels(&ticks, &geometry).to_string();
    assert!(!line.contains("10") && !line.contains("888"));
    assert_eq!(line.find('3'), Some(11));
}

#[test]
fn monthly_tokens_and_money_share_calendar_coordinates() {
    for days in [28, 29, 30, 31] {
        let usage = UsageStats {
            month: Bucketed {
                buckets: vec![1_000_000; days * 4],
                costs: vec![12_345.0; days * 4],
            },
            ..UsageStats::default()
        };
        let charts = local_charts(&usage);
        let origin = plot_origin(&charts);
        for width in [40, 80, 120, 160] {
            let tokens = histogram(&charts[1], width, 4, origin);
            let money = histogram(&charts[2], width, 4, origin);
            let token_rule = tokens[5].to_string();
            let money_rule = money[5].to_string();
            assert_eq!(
                token_rule.split_once('└').unwrap().1,
                money_rule.split_once('└').unwrap().1
            );
            assert_eq!(tokens[6].to_string(), money[6].to_string());
            if width >= 80 {
                assert_eq!(token_rule.matches('┴').count(), days);
                assert!(
                    tokens[6]
                        .to_string()
                        .split_whitespace()
                        .any(|label| label == days.to_string())
                );
            }
        }
    }
}

#[test]
fn scale_tracks_the_merged_peak_instead_of_silently_clipping_it() {
    for value in [
        0.,
        1.,
        900.,
        200_000_001.,
        2_000_000_001.,
        401.,
        1e12,
        u64::MAX as f64,
    ] {
        let ceiling = nice_ceiling(value);
        assert!(ceiling.is_finite() && ceiling > 0.);
        assert!(ceiling >= value);
    }
    assert_eq!(nice_ceiling(210_000_000.), 250_000_000.);
    assert_eq!(nice_ceiling(3_000_000_000.), 5_000_000_000.);
    let values = vec![1_000_000_000; 96];
    let chart = hourly(&values);
    let merged = merged_values(&chart.series, merge_factor(&chart, 92));
    assert_eq!(merged[0], 4_000_000_000.);
    let rendered = text(&histogram(&chart, 100, 4, 8));
    assert!(rendered.contains("5B"));
    assert!(!rendered.contains("200M"));
}

#[test]
fn zero_and_missing_data_do_not_create_fake_bars() {
    let zeros = vec![0u64; 96];
    let chart = hourly(&zeros);
    let buffer = render_chart(&chart, 100, 8, 8);
    assert!(
        buffer
            .content
            .iter()
            .all(|cell| !cell.symbol().chars().any(|c| "█▊▁▂▃▄▅▆▇".contains(c)))
    );
    assert!(histogram(&hourly(&[]), 100, 4, 8).is_empty());
    assert!(histogram(&chart, 8, 4, 8).is_empty());
    assert!(histogram(&chart, 100, 0, 8).is_empty());
}

#[test]
fn all_chart_rows_fit_and_height_is_not_snapped() {
    let values: Vec<_> = (1..=96).map(|value| value * 1_000_000).collect();
    let chart = hourly(&values);
    for width in 0..=220 {
        for rows in [0, 1, 2, 3, 7, 9, 15, 30] {
            let rendered = histogram(&chart, width, rows, 8);
            for line in &rendered {
                assert!(line.width() <= width as usize, "{width}x{rows}: {line}");
            }
            if width > 8 && rows > 0 {
                assert_eq!(rendered.len(), rows + 3);
            }
        }
    }
    for room in 0..=60 {
        assert_eq!(chart_rows(room), room.saturating_sub(4) as usize);
    }
    assert_eq!(chart_rows(24), 20);
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
