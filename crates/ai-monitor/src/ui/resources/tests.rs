use super::*;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture() -> SystemStats {
    SystemStats {
        cpu: Some(3.4),
        cores: vec![9., 3., 4., 3., 5., 3., 5., 3., 2., 2., 3., 1.],
        memory_used: (6.7 * 1_073_741_824.) as u64,
        memory_total: (19.5 * 1_073_741_824.) as u64,
        swap_used: (0.4 * 1_073_741_824.) as u64,
        swap_total: 8 * 1_073_741_824,
        network_rx_per_sec: Some(22. * 1024.),
        network_tx_per_sec: Some(7. * 1024.),
        disk_read_per_sec: Some(0.),
        disk_write_per_sec: Some(187. * 1024.),
        load: "0.87  1.14  1.14".into(),
        cpu_history: [
            3, 4, 3, 3, 6, 9, 42, 15, 9, 6, 4, 3, 2, 3, 4, 5, 8, 11, 9, 6, 5, 8, 6, 4, 3, 5, 4, 3,
            4, 11, 4, 3, 10, 13, 16, 5, 4, 3, 5, 6, 3, 4, 3, 3,
        ]
        .into(),
        ..SystemStats::default()
    }
}

fn render(stats: &SystemStats, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw_system(frame, frame.area(), stats))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn line(buffer: &Buffer, y: u16) -> String {
    let mut value = String::new();
    let mut x = 0;
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        value.push_str(symbol);
        x += columns(symbol).max(1) as u16;
    }
    value
}

fn text_of(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| line(buffer, y))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn normal_panel_matches_the_approved_groups_and_column_order() {
    let buffer = render(&fixture(), 48, 20);
    let text = text_of(&buffer);
    for value in [
        "12 逻辑核",
        "6.7/19.5 GiB",
        "34.4%",
        "0.4/8.0 GiB",
        "5.0%",
        "22.0 KiB/s",
        "7.0 KiB/s",
        "187 KiB/s",
        "0 B/s",
        "1m 0.87",
        "5m 1.14",
        "15m 1.14",
        "CPU 核心",
        "逐核占用",
        "CPU 趋势",
        "峰值 42%",
        "最近 44 次采样",
        "0–100%",
    ] {
        assert!(text.contains(value), "missing {value}:\n{text}");
    }
    for y in 1..=3 {
        assert_eq!(buffer[(45, y)].symbol(), "%");
        assert!(line(&buffer, y).contains('━'));
    }
    // Labels never overlay a colored gauge background.
    assert!(buffer.content.iter().all(|cell| cell.bg == Color::Reset));
    assert!(line(&buffer, 9).contains("01"));
    assert!(line(&buffer, 9).contains("07"));
    assert!(line(&buffer, 14).contains("06"));
    assert!(line(&buffer, 14).contains("12"));
    assert!(line(&buffer, 15).contains("CPU 趋势"));
}

#[test]
fn compact_panel_hides_history_before_dropping_cores() {
    let buffer = render(&fixture(), 32, 16);
    let text = text_of(&buffer);
    assert!(!text.contains("CPU 趋势"));
    assert!(text.contains("22.0K/s"));
    assert!(text.contains("7.0K/s"));
    assert!(text.contains("187K/s"));
    assert!(!text.contains("KiB/s"));
    assert!(text.contains("0.87 · 1.14 · 1.14"));
    for y in 1..=3 {
        assert!(!line(&buffer, y).contains('━'));
    }
    assert!(line(&buffer, 8).contains("01"));
    assert!(line(&buffer, 8).contains("07"));
    assert!(line(&buffer, 13).contains("06"));
    assert!(line(&buffer, 13).contains("12"));
    assert!(!text.contains("12/12"));
}

#[test]
fn rates_preserve_units_and_distinguish_missing_from_zero() {
    for (bytes, full, compact) in [
        (0., "0 B/s", "0B/s"),
        (22. * 1024., "22.0 KiB/s", "22.0K/s"),
        (7. * 1024., "7.0 KiB/s", "7.0K/s"),
        (187. * 1024., "187 KiB/s", "187K/s"),
        (1024. * 1024., "1.0 MiB/s", "1.0M/s"),
        (1023.9 * 1024., "1.0 MiB/s", "1.0M/s"),
    ] {
        assert_eq!(throughput(Some(bytes), false), full);
        assert_eq!(throughput(Some(bytes), true), compact);
    }
    for value in [None, Some(-1.), Some(f64::NAN), Some(f64::INFINITY)] {
        assert_eq!(throughput(value, false), "—");
        assert_eq!(throughput(value, true), "—");
        assert_eq!(percentage(value), None);
    }
    assert_eq!(percentage(Some(120.)), Some(100.));
    assert_eq!(used_percent(1, 0), None);
}

#[test]
fn unknown_metrics_and_disabled_swap_are_not_reported_as_zero_usage() {
    let buffer = render(&SystemStats::default(), 48, 20);
    let text = text_of(&buffer);
    assert!(line(&buffer, 1).contains("采样中"));
    assert!(line(&buffer, 2).contains("采样中"));
    assert!(line(&buffer, 3).contains("未启用"));
    assert!(!text.contains("0.0%"));
    assert!(!text.contains("0 B/s"));
}

#[test]
fn errors_are_visible_even_when_there_is_no_room_for_the_core_list() {
    let stats = SystemStats {
        error: Some("系统指标读取失败".into()),
        ..fixture()
    };
    let buffer = render(&stats, 48, 5);
    assert!(line(&buffer, 0).contains("读取失败"));
    let text = text_of(&render(&stats, 48, 20));
    assert!(text.contains("系统指标读取失败"));
}

#[test]
fn error_rows_take_priority_over_blank_separation_without_dropping_cores() {
    let stats = SystemStats {
        cores: vec![4.; 20],
        error: Some("系统指标读取失败".into()),
        ..fixture()
    };
    let buffer = render(&stats, 48, 20);
    let text = text_of(&buffer);
    assert!(text.contains("系统指标读取失败"));
    assert!(!text.contains("18/20"));
    assert!(line(&buffer, 18).contains("10"));
    assert!(line(&buffer, 18).contains("20"));
    assert!(!text.contains("CPU 趋势"));
}

#[test]
fn many_cores_have_an_explicit_visible_total_and_do_not_keep_a_decorative_plot() {
    let stats = SystemStats {
        cores: vec![4.; 192],
        ..fixture()
    };
    let buffer = render(&stats, 48, 15);
    let text = text_of(&buffer);
    assert!(text.contains("12/192"), "{text}");
    assert!(!text.contains("CPU 趋势"));
    assert!(text.contains("001"));
    assert!(text.contains("012"));
}

#[test]
fn odd_core_count_is_column_major_without_repeating_the_last_core() {
    let stats = SystemStats {
        cores: vec![11., 22., 33., 44., 55.],
        ..fixture()
    };
    let buffer = render(&stats, 48, 20);
    assert!(line(&buffer, 9).contains("01"));
    assert!(line(&buffer, 9).contains("04"));
    assert!(line(&buffer, 10).contains("02"));
    assert!(line(&buffer, 10).contains("05"));
    assert!(line(&buffer, 11).contains("03"));
    assert!(!line(&buffer, 11).contains("05"));
}

#[test]
fn history_peak_and_count_describe_the_visible_samples_only() {
    let stats = SystemStats {
        cpu_history: std::iter::once(99)
            .chain(std::iter::repeat_n(3, 88))
            .collect(),
        ..fixture()
    };
    let buffer = render(&stats, 48, 20);
    let text = text_of(&buffer);
    assert!(text.contains("峰值 3%"));
    assert!(!text.contains("99%"));
    assert!(text.contains("最近 88 次采样"));
    assert_eq!(buffer[(45, 17)].symbol(), "⣀");
}

#[test]
fn startup_history_is_right_aligned() {
    let stats = SystemStats {
        cpu_history: [3, 3, 3, 3].into(),
        ..fixture()
    };
    let buffer = render(&stats, 48, 20);
    assert_eq!(buffer[(2, 17)].symbol(), " ");
    assert_eq!(buffer[(43, 17)].symbol(), " ");
    for x in 44..46 {
        assert_eq!(buffer[(x, 17)].symbol(), "⣀");
    }
}

#[test]
fn extreme_sizes_counts_and_invalid_values_do_not_panic_or_overwrite_the_border() {
    for width in [1, 2, 8, 13, 22, 28, 32, 48, 80] {
        for height in [1, 2, 4, 8, 15, 20, 40] {
            for count in [0, 1, 5, 12, 192, 4096] {
                let stats = SystemStats {
                    cpu: Some(f64::NAN),
                    cores: vec![f64::INFINITY; count],
                    memory_used: u64::MAX,
                    memory_total: 1,
                    network_rx_per_sec: Some(f64::MAX),
                    ..fixture()
                };
                let buffer = render(&stats, width, height);
                if width >= 2 && height >= 2 {
                    assert_eq!(buffer[(width - 1, height - 1)].symbol(), "╯");
                    for y in 1..height - 1 {
                        assert_eq!(buffer[(width - 1, y)].symbol(), "│");
                    }
                }
            }
        }
    }
}

#[test]
fn rendering_stays_inside_an_offset_panel() {
    let mut terminal = Terminal::new(TestBackend::new(64, 30)).unwrap();
    let area = Rect::new(3, 4, 32, 16);
    terminal
        .draw(|frame| draw_system(frame, area, &fixture()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    for y in 0..30 {
        for x in 0..64 {
            if x < area.left() || x >= area.right() || y < area.top() || y >= area.bottom() {
                assert_eq!(buffer[(x, y)].symbol(), " ", "outside at {x},{y}");
            }
        }
    }
}

#[test]
fn panel_height_is_bounded_and_reserves_quota_space() {
    assert_eq!(height(39, 48, 12), 20);
    assert_eq!(height(23, 32, 12), 15);
    for body in [0, 1, 8, 12, 23, 39, 69] {
        for count in [0, 1, 12, 192, usize::MAX] {
            assert!(height(body, 48, count) <= body.saturating_sub(8));
        }
    }
}

pub(super) fn print_previews() {
    for (width, height) in [(48, 20), (32, 16), (24, 12)] {
        println!(
            "SYSTEM RATATUI BUFFER {width}x{height}\n{}\nEND SYSTEM BUFFER",
            text_of(&render(&fixture(), width, height))
        );
    }
}
