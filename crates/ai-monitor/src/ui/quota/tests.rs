use super::*;
use crate::model::{FetchError, Source};
use crate::ui::{RED, quota_panel_lines, render_panel_lines};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::time::{Duration, Instant};

const NOW: i64 = 1_700_000_000;

fn meter(label: &str, remaining: f64, seconds: i64, decimals: u8) -> Meter {
    Meter {
        label: label.into(),
        remaining: Some(remaining),
        resets_at: Some(NOW + seconds),
        available: false,
        decimals,
    }
}

fn card(title: &str, meters: Vec<Meter>) -> Card {
    Card { meters, ..Card::empty(title) }
}

fn state(source: Source, cards: Vec<Card>) -> SourceState {
    let mut state = SourceState::new(source, Duration::from_secs(60));
    state.finish(Ok(cards), NOW - 31, Instant::now());
    state
}

fn fixtures() -> Vec<SourceState> {
    vec![
        state(Source::Codex, vec![
            card("Codex · GPT", vec![meter("周", 34., 5 * 86400 + 21 * 3600, 0)]),
            card("Codex · 5.3 Spark", vec![
                meter("5H", 64., 3600 + 12 * 60, 0),
                meter("周", 32., 2 * 86400 + 19 * 3600, 0),
            ]),
        ]),
        state(Source::Agy, vec![card("AGY (Bluefish Carter)", vec![
            meter("5H", 96.32, 3 * 3600 + 23 * 60, 2),
            meter("周", 94.95, 5 * 86400 + 18 * 3600, 2),
        ])]),
        state(Source::Agy2, vec![card("AGY2 (陈悦麒)", vec![
            meter("5H", 100., 4 * 3600 + 59 * 60, 0),
            meter("周", 98.91, 5 * 86400 + 19 * 3600, 2),
        ])]),
        state(Source::Go, vec![card("OpenCode Go", vec![
            meter("5H", 97., 46 * 60 + 14, 0),
            meter("周", 26., 86400 + 17 * 3600, 0),
            meter("月", 64., 25 * 86400 + 23 * 3600, 0),
        ])]),
        state(Source::Go2, vec![card("OpenCode GO-2", vec![
            meter("5H", 100., 4 * 3600 + 59 * 60, 0),
            meter("周", 77., 86400 + 17 * 3600, 0),
            meter("月", 89., 29 * 86400 + 5 * 3600, 0),
        ])]),
        state(Source::Grok, vec![card("SuperGrok", vec![meter("周", 40., 86400 + 7 * 3600, 0)])]),
        state(Source::OpenRouter, vec![Card {
            balance: Some(39.83),
            ..Card::empty("OpenRouter")
        }]),
    ]
}

fn text(rows: &[Line<'_>]) -> String {
    rows.iter().map(Line::to_string).collect::<Vec<_>>().join("\n")
}

fn render(states: &[SourceState], width: u16, height: u16, scroll: usize) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| {
        let area = frame.area();
        let inner = panel(frame, area, states, NOW);
        let rows = quota_panel_lines(states, inner, NOW);
        render_panel_lines(frame, area, inner, rows, scroll);
    }).unwrap();
    terminal.backend().buffer().clone()
}

fn buffer_text(buffer: &Buffer) -> String {
    (0..buffer.area.height).map(|y| {
        let mut row = String::new();
        let mut x = 0;
        while x < buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            row.push_str(symbol);
            x += columns(symbol).max(1) as u16;
        }
        row.trim_end().to_owned()
    }).collect::<Vec<_>>().join("\n")
}

#[test]
fn every_line_is_bounded_including_unicode_errors_and_notes() {
    let mut states = fixtures();
    states[2].cards[0].title = "AGY2 (陈悦麒-e\u{301}-很长的账号名称\n不能覆盖下一行)".into();
    states[2].cards[0].note = Some("很长的说明\twith a newline\nthat must not break the grid".into());
    states[3].error = Some("网络错误：upstream timeout with an unusually long error message".into());
    states[6].cards[0].balance = Some(f64::MAX);
    for width in 0..=96 {
        for row in lines(&states, width, NOW) {
            assert!(row.width() <= width as usize, "width {width}: {row:?}");
            assert!(!row.to_string().chars().any(char::is_control));
        }
    }
}

#[test]
fn all_meters_share_percentage_bar_and_reset_columns() {
    let mut states = fixtures();
    states[2].cards[0].meters[0].decimals = 2;
    states[2].cards[0].meters[0].resets_at = Some(NOW + 123 * 86400);
    let cols = Columns::new(46, &states, NOW);
    let rows = lines(&states, 46, NOW);
    let meters: Vec<_> = rows.iter().filter(|r| r.to_string().contains('%')).collect();
    assert_eq!(meters.len(), 14);
    for row in meters {
        let value = row.to_string();
        let percent_at = value.find('%').unwrap();
        let bar_at = value.find('━').unwrap();
        assert_eq!(columns(&value[..percent_at]), cols.margin + cols.label + cols.value);
        assert_eq!(columns(&value[..bar_at]), cols.margin + cols.label + cols.value + 2);
        assert_eq!(value.matches('━').count(), cols.bar);
        assert_eq!(row.width(), 46);
    }
    assert!(text(&rows).contains("100.00%"));
    assert!(text(&rows).contains("123d 00h"));
}

#[test]
fn a_narrow_pane_hides_bars_without_dropping_data_or_adding_rows() {
    let states = fixtures();
    let narrow = lines(&states, 30, NOW);
    let wide = lines(&states, 46, NOW);
    let output = text(&narrow);
    assert!(!output.contains('━'));
    assert_eq!(narrow.len(), wide.len());
    for state in &states {
        for meter in state.cards.iter().flat_map(|card| &card.meters) {
            let value = percent(meter.remaining.unwrap(), meter.decimals);
            let reset = reset_text(meter, NOW);
            assert!(narrow.iter().any(|row| {
                let row = row.to_string();
                row.contains(&value) && row.contains(&reset)
            }), "missing {value} / {reset}");
        }
    }
}

#[test]
fn tiny_panes_stack_complete_values_and_reset_times() {
    let states = vec![state(Source::Agy, vec![card("AGY", vec![
        meter("5H", 96.32, 5 * 86400 + 18 * 3600, 2),
        meter("周", 100., 86400, 2),
    ])])];
    for width in 11..22 {
        let rows = lines(&states, width, NOW);
        let output = text(&rows);
        for expected in ["96.32%", "100.00%", "5d 18h", "1d 00h", "5H", "周"] {
            assert!(output.contains(expected), "width {width}: missing {expected}: {output}");
        }
        assert!(!output.contains('━'));
        assert!(rows.iter().all(|row| row.width() <= width as usize));
    }
}

#[test]
fn account_names_are_muted_and_truncated_before_status() {
    let states = vec![state(Source::Agy2, vec![card(
        "AGY2 (很长的名字 Bluefish Carter e\u{301})", vec![],
    )])];
    let rows = lines(&states, 30, NOW);
    let row = &rows[0];
    assert!(row.to_string().contains("AGY2 · "));
    assert!(row.to_string().contains('…'));
    assert!(row.to_string().trim_end().ends_with("31s前"));
    let name = row.spans.iter().find(|s| s.content.contains("AGY2")).unwrap();
    let detail = row.spans.iter().find(|s| s.content.contains(" · ")).unwrap();
    assert_eq!(name.style.fg, Some(INK));
    assert!(name.style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(detail.style.fg, Some(MUTED));
    assert!(!detail.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn balance_keeps_usd_cents_and_right_alignment() {
    let states = vec![state(Source::OpenRouter, vec![Card {
        balance: Some(39.83), ..Card::empty("OpenRouter")
    }])];
    for width in [30, 46] {
        let rows = lines(&states, width, NOW);
        assert!(rows[1].to_string().trim_end().ends_with("$39.83 USD"));
        assert_eq!(rows[1].width(), width as usize);
        let amount = rows[1].spans.iter().find(|s| s.content.contains("$39.83")).unwrap();
        assert_eq!(amount.style.bg, Some(BALANCE_BG));
        assert_eq!(amount.style.fg, Some(GREEN));
    }
}

#[test]
fn low_and_stale_balances_keep_their_warning_semantics() {
    for (balance, is_stale, expected) in [(0., false, AMBER), (0.99, false, AMBER), (39.83, true, MUTED)] {
        let line = balance_line(balance, Columns::new(46, &[], NOW), is_stale);
        let amount = line.spans.iter().find(|s| s.content.starts_with('$')).unwrap();
        assert_eq!(amount.style.fg, Some(expected));
    }
}

#[test]
fn quota_thresholds_do_not_color_the_reset_countdown() {
    for (remaining, expected) in [(0., RED), (10., RED), (10.01, AMBER), (25., AMBER), (25.01, GREEN)] {
        let meter = meter("5H", remaining, 3600, 0);
        let rows = meter_lines(&meter, Columns::new(46, &[], NOW), NOW, false);
        let value = rows[0].spans.iter().find(|s| s.content.contains('%')).unwrap();
        let reset = rows[0].spans.iter().find(|s| s.content.contains("1h 00m")).unwrap();
        assert_eq!(value.style.fg, Some(expected));
        assert_eq!(reset.style.fg, Some(MUTED));
    }
}

#[test]
fn stale_and_expired_values_are_muted_not_replenished() {
    for (seconds, is_stale) in [(3600, true), (-1, false)] {
        let meter = meter("周", 2., seconds, 0);
        let rows = meter_lines(&meter, Columns::new(46, &[], NOW), NOW, is_stale);
        let value = rows[0].spans.iter().find(|s| s.content.contains('%')).unwrap();
        assert_eq!(value.style.fg, Some(MUTED));
        assert!(text(&rows).contains("2%"));
        assert!(!text(&rows).contains("100%"));
        if seconds < 0 { assert!(text(&rows).contains("待刷新")); }
    }
}

#[test]
fn unknown_available_quota_is_not_rendered_as_full() {
    let meter = Meter { remaining: None, resets_at: None, available: true, ..meter("5H", 0., 0, 0) };
    let rows = meter_lines(&meter, Columns::new(46, &[], NOW), NOW, false);
    assert!(text(&rows).contains('—'));
    assert!(text(&rows).contains("可用"));
    assert!(!text(&rows).contains('%'));
    assert!(rows[0].spans.iter().filter(|s| s.content.contains('━')).all(|s| s.style.fg == Some(BAR_TRACK)));
}

#[test]
fn errors_and_loading_notes_remain_visible() {
    let mut state = SourceState::new(Source::Agy, Duration::from_secs(60));
    assert!(text(&lines(std::slice::from_ref(&state), 46, NOW)).contains("正在读取额度"));
    state.finish(Err(FetchError::new("连接超时")), NOW, Instant::now());
    let output = text(&lines(std::slice::from_ref(&state), 46, NOW));
    assert!(output.contains("未连接"));
    assert!(output.contains("连接超时"));
    state.finish(Ok(vec![Card { note: Some("未订阅".into()), ..Card::empty("AGY") }]), NOW, Instant::now());
    assert!(text(&lines(&[state], 46, NOW)).contains("未订阅"));
}

#[test]
fn group_backgrounds_replace_empty_separator_rows_at_any_height() {
    let states = fixtures();
    let short = quota_panel_lines(&states, Rect::new(0, 0, 46, 8), NOW);
    let tall = quota_panel_lines(&states, Rect::new(0, 0, 46, 80), NOW);
    assert_eq!(short, tall);
    assert_eq!(short.len(), 23); // Eight headings, fourteen meters, one balance.
    assert!(short.iter().all(|row| !row.to_string().trim().is_empty()));
}

#[test]
fn headings_are_pinned_while_only_the_list_scrolls() {
    let states = fixtures();
    let before = render(&states, 48, 12, 0);
    let after = render(&states, 48, 12, 5);
    for y in 0..2 {
        // The scrollbar belongs to the right border, not to the pinned legend.
        for x in 0..47 { assert_eq!(before[(x, y)], after[(x, y)]); }
    }
    assert!(buffer_text(&before).contains("距重置"));
    assert_ne!(before[(2, 2)], after[(2, 2)]);
}

#[test]
fn real_buffer_has_the_approved_group_background_and_card_count() {
    let buffer = render(&fixtures(), 48, 28, 0);
    assert!(buffer_text(&buffer).lines().next().unwrap().contains("8 项"));
    assert_eq!(buffer[(2, 2)].bg, GROUP_BG);
    assert_eq!(buffer[(2, 2)].fg, INK);
    assert!(buffer[(2, 2)].modifier.contains(Modifier::BOLD));
    assert!(buffer_text(&buffer).contains("可用余额"));
    assert!(buffer_text(&buffer).contains("$39.83 USD"));
}

#[test]
fn tiny_real_buffers_render_without_overflow_or_panics() {
    let states = fixtures();
    for width in 1..=48 {
        for height in [1, 2, 3, 12, 28] {
            let buffer = render(&states, width, height, 0);
            assert_eq!(buffer.area, Rect::new(0, 0, width, height));
        }
    }
}

pub(super) fn print_preview_fixtures() {
    for (width, height) in [(48, 28), (32, 28), (18, 18)] {
        let buffer = render(&fixtures(), width, height, 0);
        println!("QUOTA BUFFER {width}x{height}\n{}\nEND QUOTA BUFFER", buffer_text(&buffer));
    }
}
