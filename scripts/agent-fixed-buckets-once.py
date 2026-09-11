"""One-shot, hash-guarded source edit. Removed before the final PR diff."""
from pathlib import Path
import subprocess

expected = {
    'crates/ai-monitor/src/ui/token.rs': 'ea46da43872e163bd21fc9119016e3a1b8e673c6',
    'crates/ai-monitor/src/ui/token/tests.rs': '3e562f6d52d757eb29c2b51f9faffbcf682a1d1a',
    'crates/ai-monitor/src/ui.rs': '435ead27e464b2c911957d7ae8e13315811251ca',
    'crates/ai-monitor/src/main.rs': '9a1b2cb318e28e4a5faec7cb34a3bcd29abbc30e',
}
for name, sha in expected.items():
    actual = subprocess.check_output(['git', 'hash-object', name], text=True).strip()
    if actual != sha:
        raise RuntimeError(f'Concurrent source change in {name}: {actual}; refusing to overwrite')

def replace_once(text, old, new):
    if text.count(old) != 1:
        raise RuntimeError(f'Expected one occurrence of {old[:100]!r}')
    return text.replace(old, new, 1)

path = Path('crates/ai-monitor/src/ui/token.rs')
s = path.read_text()
s = replace_once(s, 'use super::{CYAN, GREEN, INK, MUTED, TRACK, columns, compact, truncate};',
                 'use super::{CYAN, GREEN, INK, MUTED, TRACK, View, columns, compact, truncate};\nuse chrono::{Datelike, Timelike};')
s = replace_once(s, 'const BAR: Color = Color::Rgb(33, 150, 243);', '''const BAR_LOW: Color = Color::Rgb(144, 202, 249);
const BAR_MID: Color = Color::Rgb(33, 150, 243);
const BAR_HIGH: Color = Color::Rgb(13, 71, 161);
const BAR_RED: Color = Color::Rgb(163, 22, 22);''')
s = replace_once(s, '/// Four quarter hours per hourly bar, or four six-hour buckets per day.',
                 '/// Four independent bars per hour/day. This groups ticks, never values.')
start = s.index('pub(super) fn draw_charts(')
end = s.index('\nfn chart_rows(', start)
s = s[:start] + '''pub(super) fn draw_charts(
    frame: &mut Frame,
    areas: &[Rect],
    usage: &UsageStats,
    view: &mut View,
    now: i64,
) {
    let charts = local_charts(usage);
    let origin = plot_origin(&charts);
    let local = chrono::DateTime::from_timestamp(now, 0)
        .map(|time| time.with_timezone(&chrono::Local));
    for (index, (chart, area)) in charts.iter().zip(areas).enumerate() {
        if area.height < MIN_CHART_HEIGHT {
            continue;
        }
        // Monthly tokens and money deliberately use the same navigation state.
        let nav = usize::from(index > 0);
        let plot = (area.width as usize).saturating_sub(origin);
        if let Some(window) = Window::new(chart.series.len(), chart.tick_buckets, plot, 0) {
            view.max_chart_starts[nav] = window.max_start;
            let current = local.as_ref().map_or(0, |time| {
                if nav == 0 { time.hour() as usize } else { time.day0() as usize }
            });
            if !view.chart_manual {
                // Follow the current hour/day, not the far-right future zeros.
                view.chart_starts[nav] = current.saturating_add(1)
                    .saturating_sub(window.groups).min(window.max_start);
            } else {
                view.chart_starts[nav] = view.chart_starts[nav].min(window.max_start);
            }
        }
        frame.render_widget(
            Paragraph::new(histogram(
                chart, area.width, chart_rows(area.height), origin, view.chart_starts[nav],
            )),
            *area,
        );
    }
}
''' + s[end:]
start = s.index('            let total: f64 = (0..chart.series.len())')
end = s.index('            columns(&value_label', start)
s = s[:start] + '            let max = nice_ceiling(series_peak(&chart.series));\n' + s[end:]
start = s.index('/// Defaults stay at one hour / one day')
s = s[:start] + r'''/// The scale is computed over ALL native buckets, not just the visible window.
/// Panning/resizing cannot change a bucket's height ratio or colour band.
fn series_peak(series: &Series<'_>) -> f64 {
    (0..series.len())
        .map(|index| series.value(index))
        .filter(|value| value.is_finite())
        .fold(0., f64::max)
}

fn bar_color(share: f64) -> Color {
    if share >= 0.95 {
        BAR_RED
    } else if share >= 0.80 {
        BAR_HIGH
    } else if share >= 0.30 {
        BAR_MID
    } else {
        BAR_LOW
    }
}

/// Show complete hour/day groups without merging their four native buckets.
/// A narrow terminal pans through the original series instead of changing time
/// resolution. Empty buckets keep their slots. Each bar needs a real gap.
#[derive(Debug)]
struct Window {
    first_group: usize,
    groups: usize,
    max_start: usize,
    start: usize,
    len: usize,
}

impl Window {
    fn new(len: usize, group: usize, plot: usize, requested: usize) -> Option<Self> {
        if len == 0 || group == 0 {
            return None;
        }
        let groups = (plot.saturating_sub(1) / (2 * group)).min(len.div_ceil(group));
        if groups == 0 {
            return None;
        }
        let max_start = len.div_ceil(group).saturating_sub(groups);
        let first_group = requested.min(max_start);
        let start = first_group * group;
        Some(Self {
            first_group,
            groups,
            max_start,
            start,
            len: (groups * group).min(len - start),
        })
    }
}

/// Odd-width full-cell bars have a real centre cell. The old two-column bar
/// placed its tick in the right cell: that was half a cell off optically even
/// though a test using the same floor(width/2) convention claimed alignment.
/// One blank at each plot edge also keeps two-digit group labels in bounds.
struct Geometry {
    origin: usize,
    plot: usize,
    bar_width: usize,
    starts: Vec<usize>,
}

impl Geometry {
    fn new(width: u16, origin: usize, count: usize) -> Option<Self> {
        let plot = (width as usize).saturating_sub(origin);
        if count == 0 || plot < 2 * count + 1 {
            return None;
        }
        let available = plot - 1;
        let mut bar_width = (available / count - 1).max(1);
        if bar_width.is_multiple_of(2) {
            bar_width -= 1;
        }
        let starts = (0..count)
            .map(|index| {
                let left = index * available / count;
                let right = (index + 1) * available / count;
                1 + left + (right - left - bar_width - 1) / 2
            })
            .collect();
        Some(Self { origin, plot, bar_width, starts })
    }

    fn centre(&self, index: usize) -> usize {
        self.starts[index] + self.bar_width / 2
    }

    fn gap_after(&self, index: usize) -> usize {
        let end = self.starts[index] + self.bar_width;
        self.starts.get(index + 1).copied().unwrap_or(self.plot).saturating_sub(end)
    }
}

fn histogram(
    chart: &Chart<'_>, width: u16, rows: usize, origin: usize, first_group: usize,
) -> Vec<Line<'static>> {
    let plot = (width as usize).saturating_sub(origin);
    if chart.series.len() == 0 || plot == 0 || rows == 0 {
        return vec![];
    }
    let Some(window) = Window::new(chart.series.len(), chart.tick_buckets, plot, first_group) else {
        return vec![Line::styled(
            truncate(&format!("{} · 请拉宽，保留每柱{}", chart.label, span_label(chart.span_seconds)), width as usize),
            Style::default().fg(MUTED),
        )];
    };
    let Some(geometry) = Geometry::new(width, origin, window.len) else {
        return vec![];
    };
    let max = nice_ceiling(series_peak(&chart.series));
    let subrows = rows * 8;
    let values: Vec<_> = (window.start..window.start + window.len)
        .map(|index| chart.series.value(index)).collect();
    let eighths: Vec<_> = values.iter().map(|value| {
        if !value.is_finite() || *value <= 0. {
            0
        } else {
            ((*value / max * subrows as f64).round() as usize).min(subrows)
        }
    }).collect();
    // All four bars get a small tick. Integer hours/dates label the FIRST
    // bucket of the group (00 minutes / 00 hours), never an arbitrary bar.
    let ticks: Vec<_> = (0..window.len).step_by(chart.tick_buckets).map(|index| {
        (geometry.centre(index),
         (chart.first_tick as usize + window.first_group + index / chart.tick_buckets).to_string())
    }).collect();
    let first = chart.first_tick as usize + window.first_group;
    let last = first + window.groups - 1;
    let unit = if chart.first_tick == 0 { "时" } else { "日" };
    let title = format!(" {} · 每柱 {} · {}-{}{}{}", chart.label,
        span_label(chart.span_seconds), first, last, unit,
        if window.max_start > 0 { " ←→" } else { "" });
    let mut result = vec![Line::styled(truncate(&title, width as usize),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD))];
    let tick_step = rows.div_ceil(4);
    for row in 0..rows {
        let head = if row.is_multiple_of(tick_step) {
            value_label(max * (rows - row) as f64 / rows as f64, max, chart.series.money())
        } else { String::new() };
        let mut spans = axis_prefix(&head, '┤', geometry.origin);
        spans.push(Span::raw(" ".repeat(geometry.starts[0])));
        let base = (rows - 1 - row) * 8;
        for (index, filled) in eighths.iter().enumerate() {
            let glyph = match filled.saturating_sub(base).min(8) {
                0 => ' ',
                8 => '█',
                n => ['▁', '▂', '▃', '▄', '▅', '▆', '▇'][n - 1],
            };
            spans.push(Span::styled(glyph.to_string().repeat(geometry.bar_width),
                Style::default().fg(bar_color(values[index] / max))));
            spans.push(Span::raw(" ".repeat(geometry.gap_after(index))));
        }
        result.push(Line::from(spans));
    }
    let mut rule = vec!['─'; geometry.plot];
    for index in 0..window.len {
        rule[geometry.centre(index)] = '┴';
    }
    let mut baseline = axis_prefix(&value_label(0., max, chart.series.money()), '└', geometry.origin);
    // Emphasise the four-bucket group starts without shifting a tick's cell.
    for (column, glyph) in rule.into_iter().enumerate() {
        let major = ticks.iter().any(|(centre, _)| *centre == column);
        baseline.push(Span::styled(glyph.to_string(),
            Style::default().fg(if major { CYAN } else { TRACK })));
    }
    result.push(Line::from(baseline));
    result.push(tick_labels(&ticks, &geometry));
    result
}

fn axis_prefix(label: &str, mark: char, origin: usize) -> Vec<Span<'static>> {
    let padding = origin.saturating_sub(columns(label) + 3);
    vec![
        Span::styled(format!(" {}{label} ", " ".repeat(padding)), Style::default().fg(MUTED)),
        Span::styled(mark.to_string(), Style::default().fg(TRACK)),
    ]
}

/// Only hour/day labels are thinned on narrow panes; buckets are NEVER merged.
/// Two-digit labels use the conventional integer-cell centring rule. A label
/// is not pinned elsewhere to fit an edge; the plot includes label gutters.
fn tick_labels(ticks: &[(usize, String)], geometry: &Geometry) -> Line<'static> {
    let mut row = String::new();
    let mut next_free = 0;
    for (centre, text) in ticks {
        let width = columns(text);
        let Some(start) = centre.checked_sub(width / 2) else { continue };
        if start < next_free || start + width > geometry.plot { continue; }
        row.push_str(&" ".repeat(start.saturating_sub(columns(&row))));
        row.push_str(text);
        next_free = start + width + 1;
    }
    Line::styled(format!("{}{row}", " ".repeat(geometry.origin)), Style::default().fg(MUTED))
}
'''
path.write_text(s)

path = Path('crates/ai-monitor/src/ui.rs')
s = path.read_text()
s = replace_once(s, '    pub page_size: usize,', '''    pub page_size: usize,
    /// Native-bucket window starts, measured in hours and days (not bars).
    /// The two monthly charts share the day start and always pan together.
    pub chart_starts: [usize; 2],
    pub max_chart_starts: [usize; 2],
    pub chart_manual: bool,''')
s = replace_once(s, '/// What a footer hint or a mouse gesture asks the app to do.', '''impl View {
    /// Pan both timelines independently at their ends. No data is rebucketed.
    pub fn pan_charts(&mut self, later: bool) {
        if self.max_chart_starts.iter().all(|max| *max == 0) { return; }
        self.chart_manual = true;
        for (start, max) in self.chart_starts.iter_mut().zip(self.max_chart_starts) {
            *start = if later { start.saturating_add(1).min(max) }
                     else { start.saturating_sub(1).min(max) };
        }
    }
}

/// What a footer hint or a mouse gesture asks the app to do.''')
s = replace_once(s, '    let area = frame.area();', '    let area = frame.area();\n    view.max_chart_starts = [0; 2];')
s = replace_once(s, '        token::draw_charts(frame, &token_parts[1..], usage);',
                 '        token::draw_charts(frame, &token_parts[1..], usage, view, now);')
s = replace_once(s, '    hint_spans.push(Span::styled(" 退出", muted));', '''    hint_spans.push(Span::styled(" 退出", muted));
    if room >= 60 && view.max_chart_starts.iter().any(|max| *max > 0) {
        hint_spans.push(Span::styled("  ←→ 时段  0 当前", muted));
    }''')
path.write_text(s)
path = Path('crates/ai-monitor/src/main.rs')
s = path.read_text()
s = replace_once(s, '                        KeyCode::PageUp => {', '''                        KeyCode::Left => view.pan_charts(false),
                        KeyCode::Right => view.pan_charts(true),
                        KeyCode::Char('0') => view.chart_manual = false,
                        KeyCode::PageUp => {''')
path.write_text(s)

path = Path('crates/ai-monitor/src/ui/token/tests.rs')
s = path.read_text()
s = replace_once(s, 'Paragraph::new(histogram(chart, width, rows, origin)),',
                 'Paragraph::new(histogram(chart, width, rows, origin, 0)),')
start = s.index('#[test]\nfn default_granularity_does_not_change_on_wider_panes')
end = s.index('#[test]\nfn axis_labels_and_sub_dollar_amounts_remain_readable')
s = s[:start] + s[end:] + r'''

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
            assert_eq!(chart.series.value(window.start), if index == 2 { 0.1 } else { 1_000_000. });
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
                assert_eq!(buffer[(left + offset as u16, 6)].symbol(), character.to_string());
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
            if buffer[(x, 4)].symbol() != "█" { x += 1; continue; }
            let left = x;
            while x < width && buffer[(x, 4)].symbol() == "█" { x += 1; }
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
        (0., BAR_LOW), (0.2999, BAR_LOW), (0.30, BAR_MID),
        (0.7999, BAR_MID), (0.80, BAR_HIGH), (0.9499, BAR_HIGH),
        (0.95, BAR_RED), (1., BAR_RED), (2., BAR_RED),
    ] { assert_eq!(bar_color(share), colour, "share {share}"); }
    assert_eq!(BAR_LOW, Color::Rgb(144, 202, 249));
    assert_eq!(BAR_MID, Color::Rgb(33, 150, 243));
    assert_eq!(BAR_HIGH, Color::Rgb(13, 71, 161));
}

#[test]
fn buffer_bars_use_the_same_band_for_caps_and_bodies() {
    let mut values = vec![0u64; 96];
    values[..7].copy_from_slice(&[29, 30, 79, 80, 94, 95, 100]);
    let buffer = render_chart(&hourly(&values), 201, 10, 8);
    for (quarter, colour) in [BAR_LOW, BAR_MID, BAR_MID, BAR_HIGH, BAR_HIGH, BAR_RED, BAR_RED].into_iter().enumerate() {
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
            month: Bucketed { buckets: vec![100; days * 4], costs: vec![12_345.; days * 4] },
            ..UsageStats::default()
        };
        let charts = local_charts(&usage);
        let origin = plot_origin(&charts);
        let width = (origin + 2 * days * 4 + 1) as u16;
        let tokens = histogram(&charts[1], width, 4, origin, 0);
        let money = histogram(&charts[2], width, 4, origin, 0);
        assert_eq!(tokens[5].to_string().matches('┴').count(), days * 4);
        assert_eq!(tokens[5].to_string().split_once('└').unwrap().1,
                   money[5].to_string().split_once('└').unwrap().1);
        assert_eq!(tokens[6].to_string(), money[6].to_string());
        assert!(tokens[6].to_string().split_whitespace().any(|label| label == days.to_string()));
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
    terminal.draw(|f| f.render_widget(Paragraph::new(histogram(&chart, 41, 4, 8, 5)), f.area())).unwrap();
    assert_eq!(terminal.backend().buffer()[(9, 4)].fg, BAR_MID);
    assert_eq!(nice_ceiling(series_peak(&chart.series)), 100.);
}

#[test]
fn zero_missing_and_tiny_windows_never_invent_bars_or_merge_values() {
    let zeros = vec![0u64; 96];
    let chart = hourly(&zeros);
    let buffer = render_chart(&chart, 100, 8, 8);
    assert!(buffer.content.iter().all(|cell| !cell.symbol().chars().any(|c| "█▊▁▂▃▄▅▆▇".contains(c))));
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
            for line in &rendered { assert!(line.width() <= width as usize, "{width}x{rows}: {line}"); }
            if width >= 17 && rows > 0 { assert_eq!(rendered.len(), rows + 3); }
        }
    }
    for room in 0..=60 { assert_eq!(chart_rows(room), room.saturating_sub(4) as usize); }
}

#[test]
fn navigation_is_bounded_and_monthly_charts_share_one_day_offset() {
    let mut view = View { chart_starts: [8, 10], max_chart_starts: [12, 20], ..View::default() };
    view.pan_charts(false);
    assert_eq!(view.chart_starts, [7, 9]);
    assert!(view.chart_manual);
    for _ in 0..40 { view.pan_charts(true); }
    assert_eq!(view.chart_starts, [12, 20]);
    for _ in 0..40 { view.pan_charts(false); }
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
'''
path.write_text(s)
print('Patched exactly:', *expected, sep='\n')
