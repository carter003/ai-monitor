use super::*;
use crate::model::{Bucketed, Card, FetchError, Meter, ModelUsage, Source, UsageTotal};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::time::{Duration, Instant};

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

fn render(usage: &UsageStats, width: u16, height: u16) -> (String, View) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut view = View::default();
    terminal
        .draw(|frame| draw(frame, &SystemStats::default(), &[], usage, &mut view, 100))
        .unwrap();
    (buffer_text(terminal.backend().buffer()), view)
}

fn sample_usage() -> UsageStats {
    UsageStats {
        models: (0..6)
            .map(|index| ModelUsage {
                model: format!("provider/model-with-a-long-name-{index}"),
                input_total: 9_600_000,
                cache_read: 8_400_000,
                output: 400_000,
                reasoning: 120_000,
                cost: Some(3.5),
            })
            .collect(),
        hours: Bucketed {
            buckets: vec![1_000_000; 96],
            costs: vec![0.1; 96],
        },
        month: Bucketed {
            buckets: vec![1_000_000; 124],
            costs: vec![0.1; 124],
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

fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let channel = |value: u8| {
        let value = f64::from(value) / 255.;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let hi = relative_luminance(a).max(relative_luminance(b));
    let lo = relative_luminance(a).min(relative_luminance(b));
    (hi + 0.05) / (lo + 0.05)
}

fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        other => panic!("expected RGB, got {other:?}"),
    }
}

#[test]
fn inks_clear_wcag_aa_on_a_white_terminal() {
    for color in [CYAN, MUTED, GREEN, AMBER, RED] {
        assert!(contrast(rgb(color), (255, 255, 255)) >= 4.5);
    }
}

#[test]
fn gauge_fill_and_label_stay_readable_on_the_track() {
    for color in [CYAN, GREEN, AMBER] {
        assert!(contrast(rgb(color), rgb(TRACK)) >= 3.0);
        assert!(contrast(rgb(INK), rgb(TRACK)) >= 4.5);
        assert!(contrast(rgb(LIGHT_INK), rgb(color)) >= 4.5);
    }
}

#[test]
fn gauge_label_is_inked_per_column_across_the_fill_boundary() {
    let stats = SystemStats {
        cpu: Some(63.0),
        memory_used: 9_000_000_000,
        memory_total: 20_000_000_000,
        ..SystemStats::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(44, 20)).unwrap();
    let mut view = View::default();
    terminal
        .draw(|f| draw(f, &stats, &[], &UsageStats::default(), &mut view, 100))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let label_cells = (0..buffer.area.width)
        .map(|x| &buffer[(x, 1)])
        .filter(|cell| cell.symbol() != "█" && !cell.symbol().trim().is_empty())
        .filter(|cell| cell.fg == INK || cell.fg == LIGHT_INK)
        .count();
    assert!(label_cells >= 5);
    for cell in &buffer.content {
        if cell.symbol() == "█" {
            assert_ne!(cell.fg, LIGHT_INK);
        }
    }
}

#[test]
fn all_terminal_sizes_render_and_scrolling_is_bounded() {
    let states: Vec<_> = Source::ALL
        .into_iter()
        .map(|source| SourceState::new(source, Duration::from_secs(60)))
        .collect();
    for (width, height) in [
        (1, 1),
        (20, 10),
        (26, 12),
        (32, 24),
        (40, 40),
        (52, 48),
        (80, 24),
        (100, 30),
        (120, 40),
        (160, 50),
        (220, 70),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut view = View {
            scroll: usize::MAX,
            ..View::default()
        };
        terminal
            .draw(|f| {
                draw(
                    f,
                    &SystemStats::default(),
                    &states,
                    &sample_usage(),
                    &mut view,
                    100,
                )
            })
            .unwrap();
        if width >= 26 && height >= 12 {
            assert!(view.scroll <= view.max_scroll, "{width}x{height}");
        }
    }
}

fn quota_text(state: &SourceState, now: i64) -> String {
    quota_lines(std::slice::from_ref(state), 80, now, false)
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn low_monthly_balance_and_reset_wait_are_visible() {
    let mut state = SourceState::new(Source::Go, Duration::from_secs(60));
    state.begin("a".into());
    state.finish(
        Ok(vec![Card {
            meters: vec![Meter::from_used("月", 98., Some(99), 0).unwrap()],
            ..Card::empty("OpenCode Go")
        }]),
        98,
        Instant::now(),
    );
    let text = quota_text(&state, 100);
    assert!(text.contains("2%"));
    assert!(text.contains("待刷新"));
    assert!(!text.contains("100%"));
}

#[test]
fn exhausted_weekly_source_shows_wait_instead_of_stale() {
    let mut state = SourceState::new(Source::Agy, Duration::from_secs(60));
    state.begin("a".into());
    state.finish(
        Ok(vec![Card {
            meters: vec![Meter::from_used("周", 100., Some(100 + 2 * 86400), 1).unwrap()],
            ..Card::empty("Agy")
        }]),
        100,
        Instant::now(),
    );
    let text = quota_text(&state, 100 + 300);
    assert!(text.contains("等1d 23h"));
    assert!(!text.contains("旧"));
}

#[test]
fn failed_manual_refresh_marks_held_data_stale() {
    let mut state = SourceState::new(Source::Agy, Duration::from_secs(60));
    state.begin("a".into());
    state.finish(
        Ok(vec![Card {
            meters: vec![Meter::from_used("周", 100., Some(10000), 0).unwrap()],
            ..Card::empty("A")
        }]),
        100,
        Instant::now(),
    );
    state.finish(Err(FetchError::new("连接超时")), 200, Instant::now());
    assert!(quota_text(&state, 200).contains("旧"));
}

#[test]
fn configured_ten_minute_refresh_is_not_stale_after_two_minutes() {
    let mut state = SourceState::new(Source::Go, Duration::from_secs(600));
    state.begin("a".into());
    state.finish(
        Ok(vec![Card {
            meters: vec![Meter::from_used("周", 20., Some(10000), 0).unwrap()],
            ..Card::empty("Go")
        }]),
        100,
        Instant::now() + Duration::from_secs(600),
    );
    assert!(!quota_text(&state, 221).contains("旧"));
    assert!(!quota_text(&state, 1300).contains("旧"));
    assert!(quota_text(&state, 1301).contains("旧"));
    state.finish(Err(FetchError::new("连接超时")), 222, Instant::now());
    assert!(quota_text(&state, 222).contains("旧"));
}

#[test]
fn percentage_keeps_near_full_precision() {
    assert_eq!(percent(99.97, 0), "99.97%");
    assert_eq!(percent(100., 0), "100%");
    assert_eq!(percent(0., 0), "0%");
}

#[test]
fn percentage_keeps_official_decimal_places() {
    for (value, places, expected) in [
        (100., 1, "100.0%"),
        (68.8, 1, "68.8%"),
        (0., 1, "0.0%"),
        (67.24, 2, "67.24%"),
        (82., 0, "82%"),
    ] {
        assert_eq!(percent(value, places), expected);
    }
}

#[test]
fn footer_shows_version_at_bottom_right() {
    let (text, _) = render(&UsageStats::default(), 80, 24);
    let version = concat!("v", env!("CARGO_PKG_VERSION"));
    assert!(text.lines().last().unwrap().ends_with(version));
}

#[test]
fn the_readme_changelog_matches_the_crate_version() {
    let readme = include_str!("../../README.md");
    let expected = format!("## {} 更新", env!("CARGO_PKG_VERSION"));
    let first = readme
        .lines()
        .find(|line| line.starts_with("## ") && line.ends_with(" 更新"))
        .unwrap();
    assert_eq!(first.trim(), expected);
    assert!(readme.contains(concat!("`v", env!("CARGO_PKG_VERSION"), "`")));
}

#[test]
fn throughput_is_fixed_width_and_promotes_after_999() {
    assert_eq!(throughput(0.0), "000K/s");
    assert_eq!(throughput(72.4 * 1024.0), "072K/s");
    assert_eq!(throughput(999.0 * 1024.0), "999K/s");
    assert_eq!(throughput(999.1 * 1024.0), "001M/s");
    assert_eq!(throughput(999.1 * 1024.0 * 1024.0), "001G/s");
    assert_eq!(throughput(999.1 * 1024.0 * 1024.0 * 1024.0), "001T/s");
}

#[test]
fn twelve_logical_cpus_form_a_vertical_axis_beside_history() {
    let stats = SystemStats {
        cpu: Some(32.0),
        cores: (1..=12).map(|n| n as f64 * 7.0).collect(),
        cpu_history: [10, 30, 80, 40].into(),
        ..SystemStats::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(110, 42)).unwrap();
    let mut view = View::default();
    terminal
        .draw(|f| draw(f, &stats, &[], &UsageStats::default(), &mut view, 100))
        .unwrap();
    let text = buffer_text(terminal.backend().buffer());
    let lines: Vec<_> = text.lines().collect();
    let first = lines
        .iter()
        .position(|line| line.contains("CPU01"))
        .unwrap();
    let last = lines
        .iter()
        .position(|line| line.contains("CPU12"))
        .unwrap();
    assert_eq!(last - first, 11);
    assert!(lines[first..=last].iter().all(|line| line.contains('%')));
    assert!(
        lines[first..=last]
            .iter()
            .all(|line| line.chars().any(|c| "▁▂▃▄▅▆▇█".contains(c)))
    );
    assert!(view.page_size > 0);
    assert!(last < 20);
}

#[test]
fn a_missing_database_says_so_instead_of_showing_zeros() {
    let usage = UsageStats {
        error: Some("unable to open database file".into()),
        ..sample_usage()
    };
    let (text, _) = render(&usage, 120, 40);
    assert!(text.contains("未连接 usage.db"));
    assert!(!text.contains("$0.00"));
    assert!(!text.contains("今日 Token"));
}

#[test]
fn the_total_survives_a_short_pane_without_a_double_frame() {
    let (text, _) = render(&sample_usage(), 80, 13);
    assert!(text.contains("历史累计"), "{text}");
    assert!(text.contains("126.3B"), "{text}");
    assert!(text.contains("$318.6"), "{text}");
    assert!(!text.contains('╔') && !text.contains('╚'));
}

#[test]
fn the_panels_identify_local_usage_and_cloud_remaining_separately() {
    let (text, _) = render(&sample_usage(), 120, 40);
    for label in [
        "系统资源 · 已用",
        "AI 额度 · 剩余",
        "本地消耗 · Token",
        "最近24小时",
        "今日 Token",
        "本月 Token",
        "本月金额",
    ] {
        assert!(text.contains(label), "missing {label}: {text}");
    }
    assert!(!text.contains("Token (24H)"));
}

#[test]
fn the_charts_start_after_content_not_at_forty_percent() {
    let usage = sample_usage();
    let short = Rect::new(0, 0, 100, 44);
    let tall = Rect::new(0, 0, 100, 68);
    let (a, a_drawn) = token_rects(short, &usage);
    let (b, b_drawn) = token_rects(tall, &usage);
    assert!(a_drawn && b_drawn);
    let expected = token::lines(&usage, 100, usize::MAX).len() as u16;
    assert_eq!(a[0].height, expected);
    assert_eq!(b[0].height, expected);
    assert_eq!(a[1].y, a[0].bottom());
    assert_eq!(b[1].y, b[0].bottom());
    for parts in [a, b] {
        let smallest = parts[1..].iter().map(|area| area.height).min().unwrap();
        let largest = parts[1..].iter().map(|area| area.height).max().unwrap();
        assert!(largest - smallest <= 1);
    }
    assert!(b[1].height > a[1].height);
}

#[test]
fn charts_stay_fixed_when_the_quota_list_scrolls() {
    let usage = sample_usage();
    let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
    let mut view = View::default();
    let states: Vec<_> = (0..30)
        .map(|_| SourceState::new(Source::Go, Duration::from_secs(60)))
        .collect();
    terminal
        .draw(|f| draw(f, &SystemStats::default(), &states, &usage, &mut view, 100))
        .unwrap();
    let before = terminal.backend().buffer().clone();
    view.scroll = usize::MAX;
    terminal
        .draw(|f| draw(f, &SystemStats::default(), &states, &usage, &mut view, 100))
        .unwrap();
    let after = terminal.backend().buffer();
    let token_x = sidebar_width(160) + 1;
    for y in 1..49 {
        for x in token_x..159 {
            assert_eq!(before[(x, y)], after[(x, y)]);
        }
    }
    assert!(view.max_scroll > 0);
}

#[test]
fn footer_hints_expose_clickable_regions_that_match_their_text() {
    let (text, view) = render(&sample_usage(), 80, 24);
    let footer = text.lines().last().unwrap();
    assert!(!view.footer_hits.is_empty());
    for (first, last, action) in &view.footer_hits {
        assert!(first <= last);
        let slice: String = footer
            .chars()
            .skip(*first as usize)
            .take((last - first + 1) as usize)
            .collect();
        let expected = match action {
            FooterAction::Refresh => "r",
            FooterAction::ScrollUp => "↑",
            FooterAction::ScrollDown => "↓",
        };
        assert!(slice.contains(expected));
    }
    assert!(!footer.contains("t 切换"));
}

#[test]
fn clicking_the_footer_refresh_region_works_on_a_narrow_pane() {
    for width in [40, 80] {
        let (_, view) = render(&sample_usage(), width, 24);
        assert!(
            view.footer_hits
                .iter()
                .any(|(_, _, action)| *action == FooterAction::Refresh)
        );
    }
}

#[test]
fn a_too_small_frame_clears_stale_mouse_regions() {
    let mut terminal = Terminal::new(TestBackend::new(10, 5)).unwrap();
    let mut view = View {
        footer_hits: vec![(1, 3, FooterAction::Refresh)],
        footer_row: Some(23),
        body: Some(Rect::new(0, 0, 80, 23)),
        ..View::default()
    };
    terminal
        .draw(|f| {
            draw(
                f,
                &SystemStats::default(),
                &[],
                &sample_usage(),
                &mut view,
                100,
            )
        })
        .unwrap();
    assert!(view.footer_hits.is_empty());
    assert!(view.footer_row.is_none());
    assert!(view.body.is_none());
}

#[test]
fn truncate_and_padding_support_wide_and_combining_characters() {
    assert_eq!(truncate("short", 10), "short");
    assert_eq!(truncate("deepseek/deepseek-v4-flash", 12), "deepseek/de…");
    assert_eq!(truncate("abcdef", 0), "");
    assert_eq!(columns("智谱-GLM"), 8);
    assert_eq!(truncate("智谱-GLM", 8), "智谱-GLM");
    assert_eq!(truncate("智谱-GLM", 5), "智谱…");
    assert_eq!(truncate("智谱-GLM", 4), "智…");
    assert_eq!(columns("e\u{301}"), 1);
    assert!(columns(&truncate("智谱-e\u{301}-GLM", 8)) <= 8);
}

#[test]
fn short_panes_show_as_many_readable_charts_as_fit() {
    let usage = sample_usage();
    let height = token::lines(&usage, 100, usize::MAX).len() as u16;
    for count in 0..=3u16 {
        let (parts, drawn) = token_rects(
            Rect::new(0, 0, 100, height + count * token::MIN_CHART_HEIGHT),
            &usage,
        );
        assert_eq!(drawn, count > 0);
        assert_eq!(
            parts[1..]
                .iter()
                .filter(|area| area.height >= token::MIN_CHART_HEIGHT)
                .count(),
            count as usize
        );
        if count > 0 {
            assert_eq!(parts[0].height, height);
        }
    }
}

#[test]
fn layout_preview_fixtures() {
    for (width, height) in [(80, 24), (120, 40), (160, 50)] {
        let (text, _) = render(&sample_usage(), width, height);
        println!("RATATUI BUFFER {width}x{height}\n{text}\nEND BUFFER");
    }
}
