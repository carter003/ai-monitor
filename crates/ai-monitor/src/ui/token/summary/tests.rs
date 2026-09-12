use super::*;
use crate::model::{Bucketed, ModelUsage};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, widgets::Paragraph};

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
fn four_two_and_one_column_grids_keep_every_period_once() {
    for (width, count, height) in [(112, 4, 6), (64, 2, 13), (32, 1, 27)] {
        let rows = lines(&usage(), width);
        let rendered = text(&rows);
        assert_eq!(rows.len(), height);
        assert_eq!(rows[0].to_string().matches('╭').count(), count);
        for label in ["当日", "本周", "本月", "历史累计"] {
            assert_eq!(rendered.matches(label).count(), 1);
        }
        assert_eq!(rendered.matches("TOKENS").count(), 4);
        assert_eq!(rendered.matches("COST").count(), 4);
        assert_eq!(rendered.matches("1.0B").count(), 1);
        assert_eq!(rendered.matches("4.2B").count(), 3);
        assert_eq!(rendered.matches("$146.3").count(), 1);
        assert_eq!(rendered.matches("$548.8").count(), 3);
    }
}

#[test]
fn frames_are_equal_centered_and_values_share_the_same_left_edge() {
    for width in 50..=240 {
        let rows = lines(&usage(), width);
        for group in rows.chunks(CARD_HEIGHT + 1) {
            let top = group[0].to_string();
            let starts = positions(&top, "╭");
            let ends = positions(&top, "╮");
            let widths: Vec<_> = starts
                .iter()
                .zip(&ends)
                .map(|(a, b)| b - a + 1)
                .collect();
            assert!(widths.iter().all(|width| *width == widths[0]));
            let left = starts[0];
            let right = width - ends.last().unwrap() - 1;
            assert!(left.abs_diff(right) <= 1);
            assert!(left >= OUTER_PADDING && right >= OUTER_PADDING);
            for row in &group[1..CARD_HEIGHT - 1] {
                let mut expected = Vec::new();
                for (start, end) in starts.iter().zip(&ends) {
                    expected.extend([*start, *end]);
                }
                assert_eq!(positions(&row.to_string(), "│"), expected);
            }
            let tokens = group[2].to_string();
            let costs = group[3].to_string();
            assert_eq!(positions(&tokens, "TOKENS"), positions(&costs, "COST"));
            let mut amounts = positions(&tokens, "1.0B");
            amounts.extend(positions(&tokens, "4.2B"));
            amounts.sort_unstable();
            let costs = positions(&costs, "$");
            assert_eq!(amounts, costs);
            assert_eq!(amounts, starts.iter().map(|x| x + 3).collect::<Vec<_>>());
        }
    }
}

#[test]
fn units_use_one_shared_column_across_different_magnitudes() {
    let mut usage = usage();
    usage.day_total.tokens = 9_000;
    usage.day_total.cost = 0.01;
    usage.all_total.tokens = 126_300_000_000;
    usage.all_total.cost = 12_345.0;
    let rows = lines(&usage, 120);
    let tokens = rows[2].to_string();
    let costs = rows[3].to_string();
    assert_eq!(positions(&tokens, "TOKENS"), positions(&costs, "COST"));
    assert!(tokens.contains("126.3B") && costs.contains("$12345"));
    assert!(tokens.contains("9.0K") && costs.contains("$0.01"));
}

#[test]
fn widths_including_zero_and_extreme_values_never_overflow() {
    let normal = usage();
    let mut large = usage();
    large.all_total.tokens = u64::MAX;
    large.all_total.cost = 123_456_789.0;
    for usage in [normal, large] {
        for width in 0..=240 {
            let rows = lines(&usage, width);
            assert!(rows.iter().all(|row| row.width() <= width), "width={width}");
            if width >= 32 {
                let rendered = text(&rows);
                assert!(rendered.contains(&compact(usage.all_total.tokens)));
                assert!(rendered.contains(&money(usage.all_total.cost)));
            }
        }
    }
    let mut extreme = usage();
    extreme.all_total.cost = f64::MAX;
    for width in 1..=240 {
        assert!(lines(&extreme, width).iter().all(|row| row.width() <= width));
    }
}

#[test]
fn secondary_labels_disappear_together_before_values_are_shortened() {
    let rendered = text(&lines(&usage(), 24));
    assert!(!rendered.contains("TOKENS") && !rendered.contains("COST"));
    assert!(rendered.contains("4.2B") && rendered.contains("$548.8"));
    assert!(!rendered.contains('…'));
}

#[test]
fn short_panes_remove_decoration_and_stay_within_the_height_budget() {
    for width in [0, 1, 8, 16, 25, 46, 50, 64, 100, 112, 160] {
        for height in 0..=32 {
            let rows = super::super::lines(&usage(), width, height);
            assert!(rows.len() <= height, "{width}x{height}");
            assert!(rows.iter().all(|row| row.width() <= width as usize));
        }
    }
    let rendered = text(&super::super::lines(&usage(), 46, 10));
    for label in ["当日", "本周", "本月", "历史累计"] {
        assert!(rendered.contains(label));
    }
    assert!(!rendered.contains('╭'));
    assert!(rendered.contains("$548.8"));
}

#[test]
fn zero_totals_remain_actual_zeros() {
    let rendered = text(&lines(&UsageStats::default(), 112));
    assert_eq!(rendered.matches("$0.00").count(), 4);
    assert_eq!(rendered.matches("TOKENS").count(), 4);
}

#[test]
fn terminal_cells_keep_the_rectangle_background_and_numeric_hierarchy() {
    for width in [18u16, 24, 32, 64, 112, 160] {
        let rows = lines(&usage(), width as usize);
        let height = rows.len() as u16;
        let area = Rect::new(3, 2, width, height);
        let mut terminal = Terminal::new(TestBackend::new(width + 6, height + 4)).unwrap();
        terminal
            .draw(|frame| {
                for cell in &mut frame.buffer_mut().content {
                    cell.set_symbol("!");
                }
                frame.render_widget(Paragraph::new(rows.clone()), area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        for y in 0..height + 4 {
            for x in 0..width + 6 {
                if x < area.x || x >= area.right() || y < area.y || y >= area.bottom() {
                    assert_eq!(buffer[(x, y)].symbol(), "!");
                }
            }
        }
        if width >= 32 {
            let value_x = area.x + positions(&rows[0].to_string(), "╭")[0] as u16 + 3;
            let token = &buffer[(value_x, area.y + 2)];
            let cost = &buffer[(value_x, area.y + 3)];
            assert_eq!(token.fg, INK);
            assert_eq!(cost.fg, CYAN);
            assert_eq!(token.bg, BACKGROUND);
            assert!(token.modifier.contains(Modifier::BOLD));
            assert!(cost.modifier.contains(Modifier::BOLD));
        }
    }
}

fn buffer_text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            let mut row = String::new();
            let mut x = 0;
            while x < buffer.area.width {
                let symbol = buffer[(x, y)].symbol();
                row.push_str(symbol);
                x += columns(symbol).max(1) as u16;
            }
            row.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn export_real_summary_terminal_fixtures() {
    use ratatui::backend::{Backend, CrosstermBackend};
    use std::{fs, path::PathBuf};

    let temp = tempfile::tempdir().unwrap();
    let output = std::env::var_os("SUMMARY_PREVIEW_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.path().to_owned());
    fs::create_dir_all(&output).unwrap();
    // Synthetic totals from the approved design, never account credentials.
    let mut usage = usage();
    usage.models = (0..6)
        .map(|index| ModelUsage {
            model: format!("provider/sample-model-{}", index + 1),
            input_total: 9_600_000,
            cache_read: 8_400_000,
            output: 400_000,
            cost: Some(3.5),
            ..ModelUsage::default()
        })
        .collect();
    usage.month_models = usage.models.clone();
    usage.hours = Bucketed {
        buckets: vec![1_000_000; 96],
        costs: vec![0.1; 96],
    };
    usage.month = Bucketed {
        buckets: vec![1_000_000; 120],
        costs: vec![0.1; 120],
    };
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-12T12:00:00+00:00")
        .unwrap()
        .timestamp();
    for (prefix, width, height) in [
        ("summary", 114u16, 8u16),
        ("summary", 66, 15),
        ("summary", 34, 29),
        ("overview", 80, 24),
        ("overview", 120, 40),
        ("overview", 160, 50),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                if prefix == "summary" {
                    let area = crate::ui::panel(frame, frame.area(), " 本地消耗 · Token ");
                    frame.render_widget(
                        Paragraph::new(lines(&usage, area.width as usize)),
                        area,
                    );
                } else {
                    crate::ui::draw(
                        frame,
                        &crate::system::SystemStats::default(),
                        &[],
                        &usage,
                        &mut crate::ui::View::default(),
                        now,
                    );
                }
            })
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
        let name = format!("{prefix}-{width}x{height}-sample");
        fs::write(output.join(format!("{name}.ansi")), ansi).unwrap();
        fs::write(output.join(format!("{name}.txt")), buffer_text(buffer)).unwrap();
    }
}
