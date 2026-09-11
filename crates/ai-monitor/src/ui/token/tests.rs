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
fn every_native_bucket_tiles_the_plot_with_exactly_zero_unowned_units() {
    for count in [96usize, 112, 116, 120, 124] {
        for plot in count.div_ceil(2)..=420 {
            let geometry = Geometry::new((plot + 8) as u16, 8, count).unwrap();
            let owners = geometry.owners();
            assert!(
                owners.iter().all(Option::is_some),
                "gap: {count} bins, {plot} columns"
            );
            assert_eq!(owners[0], Some(0));
            assert_eq!(owners.last(), Some(&Some(count - 1)));
            let minimum = owners.len() / count;
            for index in 0..count {
                let occupied = owners.iter().filter(|owner| **owner == Some(index)).count();
                assert!(occupied == minimum || occupied == minimum + 1);
            }
            assert!(
                owners
                    .windows(2)
                    .all(|pair| pair[0] == pair[1] || pair[0].unwrap() + 1 == pair[1].unwrap())
            );
        }
    }
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
        if quarter.is_multiple_of(4) {
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
fn daily_money_points_and_monthly_token_ticks_share_the_exact_date_columns() {
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
            let mut terminal = Terminal::new(TestBackend::new(width, 11)).unwrap();
            terminal
                .draw(|frame| daily_money::draw(frame, frame.area(), &charts[2], origin, days))
                .unwrap();
            let buffer = terminal.backend().buffer();
            for (row, expected) in tokens.iter().enumerate().skip(9) {
                let actual: String = (0..width)
                    .map(|x| buffer[(x, row as u16)].symbol())
                    .collect();
                assert_eq!(actual.trim_end(), expected.to_string().trim_end());
            }
            let geometry = Geometry::new(width, origin, days * 4).unwrap();
            for day in 0..days {
                let x = (origin + geometry.centre(day * 4)) as u16;
                assert_eq!(
                    (1..=8)
                        .filter(|row| buffer[(x, *row)].symbol() == "●")
                        .count(),
                    1
                );
            }
            assert_eq!(charts[1].series.len(), days * 4);
            assert_eq!(daily_money::totals(&usage.month.costs).len(), days);
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
fn the_actual_buffer_has_no_extra_gap_between_equal_positive_bars() {
    // Decode the ACTUAL raster, independently of Geometry's edge formula.
    // Equal nonzero heights expose false gaps that sparse real data masks.
    for count in [96usize, 112, 116, 120, 124] {
        let values = vec![100u64; count];
        for width in (8 + count / 2) as u16..=260 {
            let buffer = render_chart(&hourly(&values), width, 4, 8);
            let mut raster = Vec::new();
            for x in 8..width {
                let cell = &buffer[(x, 4)];
                match cell.symbol() {
                    "█" => raster.extend([cell.fg, cell.fg]),
                    "▌" => {
                        assert_ne!(cell.fg, Color::Reset, "empty left half at {width}/{x}");
                        assert_ne!(cell.bg, Color::Reset, "empty right half at {width}/{x}");
                        raster.extend([cell.fg, cell.bg]);
                    }
                    other => panic!(
                        "unexpected gap/glyph {other:?}, count {count}, width {width}, x {x}"
                    ),
                }
            }
            let transitions = raster.windows(2).filter(|pair| pair[0] != pair[1]).count();
            assert_eq!(
                transitions + 1,
                count,
                "all {count} adjacent bars must stay distinguishable at {width}"
            );
        }
    }
}

#[test]
fn export_real_chart_terminal_fixtures() {
    use ratatui::backend::{Backend, CrosstermBackend};
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-12T12:00:00+08:00")
        .unwrap()
        .timestamp();
    for width in [100u16, 120, 160] {
        for case in ["equal", "varied"] {
            let mut usage = usage();
            usage.hours.buckets = vec![100_000_000; 96];
            usage.month.buckets = vec![1_000_000_000; 120];
            usage.month.costs = vec![0.; 120];
            if case == "varied" {
                usage.hours.buckets.fill(0);
                usage.month.buckets.fill(0);
                let values = [20, 48, 48, 48, 48, 32, 20, 40, 40, 20, 20, 0, 0, 0, 70, 90];
                for (index, value) in values.into_iter().enumerate() {
                    usage.hours.buckets[index] = value * 1_000_000;
                }
                usage.hours.buckets[95] = 35_000_000;
                usage.month.buckets[40..46].copy_from_slice(&[
                    300_000_000,
                    1_100_000_000,
                    1_100_000_000,
                    1_100_000_000,
                    300_000_000,
                    300_000_000,
                ]);
            }
            for (day, cost) in [12., 25., 10., 0., 45., 20., 15., 30., 70., 18., 390., 110.]
                .into_iter()
                .enumerate()
            {
                usage.month.costs[day * 4] = cost;
            }
            let mut terminal = Terminal::new(TestBackend::new(width, 36)).unwrap();
            terminal
                .draw(|frame| {
                    let areas = [
                        Rect::new(0, 0, width, 12),
                        Rect::new(0, 12, width, 12),
                        Rect::new(0, 24, width, 12),
                    ];
                    draw_charts(frame, &areas, &usage, now);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(
                buffer
                    .content
                    .iter()
                    .filter(|cell| cell.symbol() == "●")
                    .count(),
                12
            );
            if let Some(directory) = std::env::var_os("CHART_PREVIEW_DIR") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                // This is real Crossterm ANSI from the actual Ratatui buffer,
                // not an HTML/Python recreation of the chart geometry.
                let mut output = b"\x1b[2J\x1b[H\x1b[?25l".to_vec();
                {
                    let mut backend = CrosstermBackend::new(&mut output);
                    let blank = Buffer::empty(buffer.area);
                    backend.draw(blank.diff(buffer).into_iter()).unwrap();
                    backend.flush().unwrap();
                }
                let path = directory.join(format!("charts-{width}x36-{case}.ansi"));
                std::fs::write(path, output).unwrap();
            }
        }
    }
}
