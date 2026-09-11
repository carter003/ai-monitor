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
    let mut terminal = Terminal::new(TestBackend::new(width, rows as u16 + 4)).unwrap();
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
fn all_buckets_fit_without_mandatory_gaps_or_a_window() {
    for count in [96usize, 112, 116, 120, 124] {
        for plot in count.div_ceil(2)..=420 {
            let geometry = Geometry::new((plot + 8) as u16, 8, count).unwrap();
            assert_eq!(geometry.starts.len(), count);
            let owners = geometry.owners();
            for index in 0..count {
                assert_eq!(
                    owners.iter().filter(|owner| **owner == Some(index)).count(),
                    geometry.bar_width
                );
            }
            assert!(
                geometry
                    .starts
                    .windows(2)
                    .all(|pair| pair[0] + geometry.bar_width <= pair[1])
            );
            assert!(geometry.starts[count - 1] + geometry.bar_width <= owners.len());
        }
    }
    // Exactly 96 columns: all 96 original quarter hours, NO compulsory gap.
    let geometry = Geometry::new(104, 8, 96).unwrap();
    assert_eq!(geometry.resolution, 1);
    assert_eq!(geometry.starts, (0..96).collect::<Vec<_>>());
    assert!(geometry.owners().iter().all(Option::is_some));
}

#[test]
fn full_range_and_every_hour_or_date_label_survive_resizing() {
    for count in [96usize, 112, 116, 120, 124] {
        let values = vec![100u64; count];
        let mut chart = hourly(&values);
        if count != 96 {
            chart.first_tick = 1;
            chart.span_seconds = 6 * 3600;
        }
        for width in (8 + count.div_ceil(2)) as u16..=260 {
            let rendered = histogram(&chart, width, 7, 8);
            let labels = text(&rendered[9..]);
            let got: Vec<_> = labels.split_whitespace().collect();
            for group in 0..count / 4 {
                let label = (chart.first_tick as usize + group).to_string();
                assert_eq!(
                    got.iter().filter(|got| **got == label).count(),
                    1,
                    "count {count} width {width} missing {label}: {labels}"
                );
            }
            assert!(!text(&rendered).contains('↔'));
            assert!(!text(&rendered).contains('←'));
            let grain = if count == 96 { "15分钟" } else { "6小时" };
            assert!(rendered[0].to_string().contains(grain));
        }
    }
}

#[test]
fn full_cell_bodies_ticks_and_caps_stay_aligned_without_empty_columns() {
    let values = vec![3u64; 96];
    let buffer = render_chart(&hourly(&values), 104, 3, 8);
    for quarter in 0..96u16 {
        let x = 8 + quarter;
        assert_eq!(buffer[(x, 2)].symbol(), "▆");
        assert_eq!(buffer[(x, 3)].symbol(), "█");
        assert_eq!(buffer[(x, 4)].symbol(), "┴");
        assert_eq!(buffer[(x, 2)].fg, buffer[(x, 3)].fg);
        if quarter % 4 == 0 {
            assert_eq!(buffer[(x, 4)].fg, CYAN);
        }
    }
    assert_ne!(buffer[(8, 3)].fg, buffer[(9, 3)].fg);
    assert!(buffer.content.iter().all(|cell| cell.symbol() != "▊"));
}

#[test]
fn dense_pairs_preserve_independent_heights_and_colours_in_the_buffer() {
    let mut values = vec![0u64; 124];
    values[0] = 20;
    values[1] = 80;
    values[123] = 100;
    // 62 columns for 124 half-width bars. Independent data, no pair sum/max.
    let buffer = render_chart(&hourly(&values), 70, 10, 8);
    assert_eq!(buffer[(8, 10)].symbol(), "▌");
    assert_eq!(buffer[(8, 10)].fg, BAR_LOW);
    assert_eq!(buffer[(8, 10)].bg, bar_ink(0.8, 1));
    assert_eq!(buffer[(8, 3)].symbol(), "▐");
    assert_eq!(buffer[(8, 3)].fg, bar_ink(0.8, 1));
    assert_eq!(buffer[(8, 3)].bg, Color::Reset);
    assert_eq!(buffer[(8, 2)].symbol(), " ");
    assert_eq!(buffer[(69, 1)].symbol(), "▐");
    assert_eq!(buffer[(69, 1)].fg, bar_ink(1., 123));
    assert_eq!(buffer[(69, 1)].bg, Color::Reset);
    assert_eq!(buffer[(9, 10)].symbol(), " ");
}

#[test]
fn every_half_cell_pattern_resets_background_and_keeps_both_inks() {
    let patterns = [
        (None, None, " ", Color::Reset, Color::Reset),
        (Some(BAR_LOW), None, "▌", BAR_LOW, Color::Reset),
        (None, Some(BAR_RED), "▐", BAR_RED, Color::Reset),
        (Some(BAR_LOW), Some(BAR_RED), "▌", BAR_LOW, BAR_RED),
    ];
    for (left, right, glyph, fg, bg) in patterns {
        let span = half_cell(left, right);
        assert_eq!(span.content, glyph);
        assert_eq!(span.style.fg, Some(fg));
        assert_eq!(span.style.bg, Some(bg));
    }
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
    ] {
        assert_eq!(bar_color(share), colour);
    }
    assert_eq!(BAR_LOW, Color::Rgb(144, 202, 249));
    for share in [0.29, 0.30, 0.80, 0.95] {
        assert_eq!(bar_ink(share, 0), bar_color(share));
        assert_ne!(bar_ink(share, 0), bar_ink(share, 1));
        assert_eq!(bar_ink(share, 1), bar_ink(share, 3));
    }
}

#[test]
fn monthly_tokens_and_money_share_all_four_buckets_and_all_date_labels() {
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
        for width in [(origin + days * 2) as u16, 100, 140, 260] {
            let tokens = histogram(&charts[1], width, 7, origin);
            let money = histogram(&charts[2], width, 7, origin);
            assert_eq!(
                tokens[8].to_string().split_once('└').unwrap().1,
                money[8].to_string().split_once('└').unwrap().1
            );
            assert_eq!(text(&tokens[9..]), text(&money[9..]));
            assert_eq!(charts[1].series.len(), days * 4);
            assert_eq!(charts[2].series.len(), days * 4);
        }
    }
}

#[test]
fn last_bucket_and_first_bucket_are_visible_together_in_one_frame() {
    for count in [96usize, 112, 116, 120, 124] {
        let mut values = vec![0u64; count];
        values[0] = 100;
        values[count - 1] = 100;
        let buffer = render_chart(&hourly(&values), (count / 2 + 8) as u16, 4, 8);
        assert_eq!(buffer[(8, 1)].symbol(), "▌");
        assert_eq!(buffer[((count / 2 + 7) as u16, 1)].symbol(), "▐");
        for x in 9..(count / 2 + 7) as u16 {
            assert_eq!(buffer[(x, 1)].symbol(), " ");
        }
    }
}

#[test]
fn no_scale_or_band_changes_when_switching_density() {
    let mut values = vec![30u64; 96];
    values[95] = 100;
    for width in [56, 80, 104, 160, 260] {
        let buffer = render_chart(&hourly(&values), width, 10, 8);
        let header = histogram(&hourly(&values), width, 10, 8)[1].to_string();
        assert!(header.contains("100"));
        assert_eq!(buffer[(8, 10)].fg, BAR_MID);
    }
    assert_eq!(quantized_height(f64::NAN, 100., 10), 0);
    assert_eq!(quantized_height(-1., 100., 10), 0);
    assert_eq!(nice_ceiling(210_000_000.), 250_000_000.);
}

#[test]
fn zero_data_and_extremely_narrow_frames_never_fake_or_crop_a_period() {
    let values = vec![0u64; 96];
    let buffer = render_chart(&hourly(&values), 100, 8, 8);
    assert!(
        buffer
            .content
            .iter()
            .all(|cell| !cell.symbol().chars().any(|c| "█▌▐▁▂▃▄▅▆▇".contains(c)))
    );
    assert!(histogram(&hourly(&[]), 100, 4, 8).is_empty());
    assert!(histogram(&hourly(&values), 100, 0, 8).is_empty());
    assert!(Geometry::new(55, 8, 96).is_none());
    let tiny = text(&histogram(&hourly(&values), 55, 4, 8));
    assert!(tiny.contains("0-23时"));
    assert!(tiny.contains("完整 96 柱"));
    assert!(!tiny.contains('↔'));
}

#[test]
fn every_row_fits_and_the_full_period_is_not_height_dependent() {
    let values: Vec<_> = (1..=96).map(|i| i * 1_000_000).collect();
    for width in 0..=260 {
        for rows in [0, 1, 3, 7, 9, 20] {
            let rendered = histogram(&hourly(&values), width, rows, 8);
            for row in &rendered {
                assert!(row.width() <= width as usize, "{width}: {row}");
            }
            if width >= 56 && rows > 0 {
                assert_eq!(rendered.len(), rows + 4);
            }
        }
    }
}

#[test]
fn awkward_widths_keep_bars_on_axis_and_all_major_labels() {
    let values = vec![100u64; 96];
    for width in [104, 105, 140, 201, 300, 400] {
        let buffer = render_chart(&hourly(&values), width, 4, 8);
        let mut bars = Vec::new();
        let mut x = 8;
        while x < width {
            if buffer[(x, 4)].symbol() != "█" {
                x += 1;
                continue;
            }
            let left = x;
            let color = buffer[(x, 4)].fg;
            while x < width && buffer[(x, 4)].symbol() == "█" && buffer[(x, 4)].fg == color {
                x += 1;
            }
            let right = x - 1;
            assert_eq!((right - left + 1) % 2, 1);
            assert_eq!(buffer[((left + right) / 2, 5)].symbol(), "┴");
            bars.push((left, right));
        }
        assert_eq!(bars.len(), 96, "width {width}");
    }
}
