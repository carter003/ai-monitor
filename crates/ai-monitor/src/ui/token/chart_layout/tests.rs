use super::*;
use crate::model::{Bucketed, UsageStats};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn sample_usage() -> UsageStats {
    UsageStats {
        month_models: (0..10)
            .map(|index| ModelUsage {
                model: format!("provider/model-{index:02}"),
                input_total: (10 - index) * 100_000_000,
                cache_read: 0,
                output: 0,
                reasoning: 0,
                cost: if index == 3 {
                    None
                } else {
                    Some(12.34 + index as f64)
                },
            })
            .collect(),
        hours: Bucketed {
            buckets: vec![1_000_000; 96],
            costs: vec![0.01; 96],
        },
        month: Bucketed {
            buckets: vec![1_000_000; 124],
            costs: vec![0.1; 124],
        },
        ..UsageStats::default()
    }
}

fn row_text(buffer: &Buffer, area: Rect, y: u16) -> String {
    let mut result = String::new();
    let mut x = area.x;
    while x < area.right() {
        let symbol = buffer[(x, y)].symbol();
        result.push_str(symbol);
        x += columns(symbol).max(1) as u16;
    }
    result
}

#[test]
fn ticks_have_constant_calendar_and_cell_steps_at_every_width() {
    let usage = sample_usage();
    for days in [28, 29, 30, 31] {
        let charts = super::super::local_charts(&usage, days);
        for chart in &charts {
            for width in 10..=240 {
                let axis = TimeAxis::new(chart, width);
                let ticks = tick_positions(chart, width);
                assert!(axis.width <= width);
                assert!(!ticks.is_empty());
                assert_eq!(
                    ticks.last().unwrap().1,
                    (chart.first_tick as usize + chart.axis_units - 1).to_string()
                );
                assert_eq!(
                    ticks,
                    tick_positions(chart, axis.width),
                    "scale changed after snapping"
                );
                for pair in ticks.windows(2) {
                    let left = pair[0].1.parse::<usize>().unwrap();
                    let right = pair[1].1.parse::<usize>().unwrap();
                    assert_eq!(right - left, axis.step, "{days} days / {width} cells");
                    assert_eq!(
                        pair[1].0 - pair[0].0,
                        axis.pitch,
                        "{days} days / {width} cells"
                    );
                    assert!(pair[0].0 + 2 < pair[1].0 + 1 - pair[1].1.len());
                }
                for (x, label) in &ticks {
                    assert!(*x < axis.width);
                    assert!(label.len() <= x + 1);
                }
            }
        }
    }
}

#[test]
fn narrow_september_axes_use_five_day_steps_not_rounded_sampling() {
    let usage = sample_usage();
    let charts = super::super::local_charts(&usage, 30);
    for chart in &charts[1..] {
        let labels: Vec<_> = tick_positions(chart, 44)
            .into_iter()
            .map(|(_, label)| label)
            .collect();
        assert_eq!(labels, ["5", "10", "15", "20", "25", "30"]);
    }
}

#[test]
fn labels_and_daily_stems_share_the_exact_same_calendar_grid() {
    let usage = sample_usage();
    let charts = super::super::local_charts(&usage, 30);
    let chart = &charts[2];
    for width in 10..=180 {
        let axis = TimeAxis::new(chart, width);
        let ticks = tick_positions(chart, width);
        let mut values = vec![0.0; 30];
        for (_, label) in &ticks {
            values[label.parse::<usize>().unwrap() - 1] = 1.0;
        }
        let heights = super::super::stem_heights(chart, &values, 1.0, axis.width, 8);
        let columns: Vec<_> = heights
            .iter()
            .enumerate()
            .filter_map(|(x, h)| (*h > 0).then_some(x))
            .collect();
        assert_eq!(columns, ticks.iter().map(|(x, _)| *x).collect::<Vec<_>>());
    }
}

#[test]
fn rendered_monthly_label_rows_never_merge_or_overwrite_dates() {
    let usage = sample_usage();
    for days in [28, 29, 30, 31] {
        let charts = super::super::local_charts(&usage, days);
        for chart in &charts[1..] {
            for width in [16, 32, 40, 49, 60, 95, 120, 160, 220] {
                let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
                terminal
                    .draw(|frame| super::super::draw_stem_chart(frame, frame.area(), chart, 12))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let row = row_text(buffer, buffer.area, 11);
                let labels: Vec<usize> =
                    row.split_whitespace().map(|s| s.parse().unwrap()).collect();
                assert_eq!(labels.last(), Some(&days), "{width}: {row}");
                let gaps: Vec<_> = labels.windows(2).map(|p| p[1] - p[0]).collect();
                assert!(
                    gaps.windows(2).all(|pair| pair[0] == pair[1]),
                    "{width}: {row}"
                );
            }
        }
    }
}

#[test]
fn all_ranking_rows_share_column_widths_and_numeric_right_edges() {
    let mut usage = sample_usage();
    usage.month_models[0].model = "provider/very-long-model-name".into();
    usage.month_models[0].cost = Some(123456.0);
    usage.month_models[1].model = "provider/智谱-e\u{301}-GLM".into();
    usage.month_models[2].input_total = 7;
    for width in 32..=120 {
        let rows = ranking_table(&usage.month_models, width);
        assert_eq!(rows.len(), 11, "width {width}");
        let widths: Vec<_> = rows[0]
            .spans
            .iter()
            .map(|span| columns(&span.content))
            .collect();
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(row.width(), width);
            assert_eq!(
                row.spans
                    .iter()
                    .map(|span| columns(&span.content))
                    .collect::<Vec<_>>(),
                widths
            );
            if index > 0 {
                let model = &usage.month_models[index - 1];
                let cost = model.cost.map(money).unwrap_or_else(|| "—".into());
                assert!(row.spans[5].content.ends_with(&cost));
                assert!(
                    row.spans[7]
                        .content
                        .ends_with(&compact(model.total_tokens()))
                );
            }
        }
        assert!(rows[0].spans[5].content.ends_with("金额"));
        assert!(rows[0].spans[7].content.ends_with("Token"));
        assert_eq!(rows[4].spans[5].style.fg, Some(MUTED));
    }
}

#[test]
fn model_names_are_provider_free_utf8_safe_and_at_most_fifteen_bytes() {
    for name in [
        "provider/abcdefghijklmnopqr",
        "vendor/智谱大模型最新版本",
        "vendor/e\u{301}-long-model-name",
    ] {
        for width in 0..=30 {
            let displayed = model_name(name, width);
            assert!(displayed.len() <= 15, "{displayed}");
            assert!(columns(&displayed) <= width, "{displayed}");
            assert!(!displayed.contains('/'));
        }
    }
    assert_eq!(
        model_name("vendor/abcdefghijklmnopqr", 20),
        "abcdefghijklmno"
    );
}

#[test]
fn monthly_layout_uses_two_left_charts_and_one_full_height_ranking() {
    let usage = sample_usage();
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-12T12:00:00+00:00")
        .unwrap()
        .timestamp();
    for width in [66, 80, 100, 140, 180] {
        for monthly_height in [12, 17, 24, 35] {
            let first_height = monthly_height / 2;
            let areas = [
                Rect::new(0, 0, width, 8),
                Rect::new(0, 8, width, first_height),
                Rect::new(0, 8 + first_height, width, monthly_height - first_height),
            ];
            let (left, ranking) = monthly_areas(&areas).unwrap();
            assert!(left[0].width.abs_diff(ranking.width) <= 1);
            assert_eq!(left[0].right() + PANEL_GAP, ranking.x);
            assert_eq!(left[0].bottom(), left[1].y);
            assert_eq!(ranking.y, left[0].y);
            assert_eq!(ranking.bottom(), left[1].bottom());
            let mut terminal = Terminal::new(TestBackend::new(width, 8 + monthly_height)).unwrap();
            terminal
                .draw(|frame| super::super::draw_charts(frame, &areas, &usage, now))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let rows: Vec<_> = (ranking.y..ranking.bottom())
                .filter_map(|y| {
                    let text = row_text(buffer, ranking, y);
                    text.contains("model-").then_some((y, text))
                })
                .collect();
            assert_eq!(rows.len(), 10, "{width} x {monthly_height}");
            for (index, (_, text)) in rows.iter().enumerate() {
                assert!(
                    text.trim_start().starts_with(&format!("{}.", index + 1)),
                    "{text}"
                );
                assert!(text.contains(&format!("model-{index:02}")), "{text}");
            }
            let gaps: Vec<_> = rows.windows(2).map(|p| p[1].0 - p[0].0).collect();
            assert!(gaps.iter().max().unwrap() - gaps.iter().min().unwrap() <= 1);
            assert!(row_text(buffer, left[0], left[0].y).contains("本月 Token"));
            assert!(row_text(buffer, left[1], left[1].y).contains("本月金额"));
        }
    }
}

#[test]
fn small_monthly_panes_keep_full_width_charts() {
    for (width, height) in [(40, 24), (65, 24), (100, 10)] {
        let areas = [
            Rect::new(0, 0, width, 8),
            Rect::new(0, 8, width, height / 2),
            Rect::new(0, 8 + height / 2, width, height - height / 2),
        ];
        assert!(monthly_areas(&areas).is_none());
    }
    assert!(monthly_areas(&[]).is_none());
}

#[test]
fn empty_or_short_rankings_do_not_duplicate_or_invent_models() {
    let usage = sample_usage();
    for count in [0, 1, 3] {
        let mut terminal = Terminal::new(TestBackend::new(50, 20)).unwrap();
        terminal
            .draw(|frame| draw_monthly_ranking(frame, frame.area(), &usage.month_models[..count]))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<_> = (0..20).map(|y| row_text(buffer, buffer.area, y)).collect();
        assert_eq!(
            rows.iter().filter(|row| row.contains("model-")).count(),
            count
        );
        assert_eq!(
            rows.iter().any(|row| row.contains("暂无本月用量记录")),
            count == 0
        );
    }
}

#[test]
fn y_axis_labels_leave_room_for_their_tick_and_corner() {
    let usage = sample_usage();
    let charts = super::super::local_charts(&usage, 30);
    for chart in &charts {
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| super::super::draw_stem_chart(frame, frame.area(), chart, 12))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, buffer.area, 1).contains('┤'));
        assert!(row_text(buffer, buffer.area, 10).contains('└'));
    }
}

#[test]
fn export_real_chart_terminal_fixtures() {
    use ratatui::backend::{Backend, CrosstermBackend};
    use std::{fs, path::PathBuf};

    // Exercise the export in ordinary cargo test too. CI supplies a persistent
    // output directory for the GTK/VTE screenshot job; no credentials are read.
    let temp = tempfile::tempdir().unwrap();
    let output = std::env::var_os("CHART_PREVIEW_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.path().to_owned());
    fs::create_dir_all(&output).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-12T12:00:00+00:00")
        .unwrap()
        .timestamp();
    for (width, height) in [(80, 24), (120, 28), (160, 32)] {
        for pattern in ["equal", "varied"] {
            let mut usage = sample_usage();
            usage.month.buckets.fill(0);
            usage.month.costs.fill(0.0);
            for index in 40..48 {
                usage.month.buckets[index] = if pattern == "equal" {
                    700_000_000
                } else {
                    [500_000_000, 1_200_000_000, 1_200_000_000, 800_000_000][index % 4]
                };
            }
            usage.month.costs[40] = 470.0;
            usage.month.costs[44] = if pattern == "equal" { 470.0 } else { 220.0 };
            let names = [
                "deepseek-v4-flash",
                "muse-spark-1.3",
                "gpt-6-pro",
                "glm-5.3-flash",
                "muse-spark-1.2",
                "gemini-3.8-flash",
                "hy4-preview",
                "omen-alpha",
                "gpt-5.6-pro",
                "dots-3-next",
            ];
            for (model, name) in usage.month_models.iter_mut().zip(names) {
                model.model = format!("provider/{name}");
            }
            let areas = [
                Rect::default(),
                Rect::new(0, 0, width, height / 2),
                Rect::new(0, height / 2, width, height - height / 2),
            ];
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| super::super::draw_charts(frame, &areas, &usage, now))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let blank = Buffer::empty(buffer.area);
            let mut ansi = Vec::new();
            {
                let mut backend = CrosstermBackend::new(&mut ansi);
                backend.clear().unwrap();
                backend.hide_cursor().unwrap();
                backend.draw(blank.diff(buffer).into_iter()).unwrap();
                Backend::flush(&mut backend).unwrap();
            }
            let filename = format!("charts-{width}x{height}-{pattern}");
            assert!(!ansi.is_empty());
            fs::write(output.join(format!("{filename}.ansi")), ansi).unwrap();
            let text = (0..height)
                .map(|y| row_text(buffer, buffer.area, y))
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(output.join(format!("{filename}.txt")), text).unwrap();
        }
    }
}
