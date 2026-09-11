from pathlib import Path
import subprocess

ROOT = Path('crates/ai-monitor/src')

def replace_once(text, old, new):
    assert text.count(old) == 1, f'expected one occurrence: {old[:100]!r}'
    return text.replace(old, new, 1)

# Remove the navigation introduced in this PR, preserving the existing page scroll.
p = ROOT / 'main.rs'
s = p.read_text()
for line in [
    "                        KeyCode::Left => view.pan_charts(false),\n",
    "                        KeyCode::Right => view.pan_charts(true),\n",
    "                        KeyCode::Char('0') => view.chart_manual = false,\n",
]:
    s = replace_once(s, line, '')
p.write_text(s)
p = ROOT / 'ui.rs'
s = p.read_text()
a = s.index('    /// Native-bucket window starts,')
b = s.index('    /// Clickable footer hints,', a)
s = s[:a] + s[b:]
a = s.index('impl View {\n')
b = s.index('/// What a footer hint', a)
s = s[:a] + s[b:]
s = replace_once(s, '    view.max_chart_starts = [0; 2];\n', '')
s = replace_once(s, 'token::draw_charts(frame, &token_parts[1..], usage, view, now);', 'token::draw_charts(frame, &token_parts[1..], usage);')
s = replace_once(s, '    if room >= 60 && view.max_chart_starts.iter().any(|max| *max > 0) {\n        hint_spans.push(Span::styled("  ←→ 时段  0 当前", muted));\n    }\n', '')
p.write_text(s)

p = ROOT / 'ui/token.rs'
s = p.read_text()
s = replace_once(s, 'TRACK, View, columns', 'TRACK, columns')
s = replace_once(s, 'use chrono::{Datelike, Timelike};\n', '')
a = s.index('pub(super) fn draw_charts(')
b = s.index('fn chart_rows(', a)
s = s[:a] + '''pub(super) fn draw_charts(frame: &mut Frame, areas: &[Rect], usage: &UsageStats) {
    let charts = local_charts(usage);
    let origin = plot_origin(&charts);
    for (chart, area) in charts.iter().zip(areas) {
        if area.height >= MIN_CHART_HEIGHT {
            frame.render_widget(
                Paragraph::new(histogram(chart, area.width, chart_rows(area.height), origin)),
                *area,
            );
        }
    }
}

''' + s[b:]
s = s.replace('// Title, baseline, tick labels, and one blank separation row. Plot height', '// Title, baseline and two label rows (the second is normally blank). Plot height')
s = s.replace('/// The scale is computed over ALL native buckets, not just the visible window.\n/// Panning/resizing cannot change a bucket\'s height ratio or colour band.', '/// The scale is computed over all native buckets. Resizing changes only\n/// raster resolution, never the denominator or a bucket\'s colour band.')
a = s.index('/// Show complete hour/day groups without merging')
s = s[:a] + r'''/// Full-period geometry. First remove compulsory gaps, then use half-cell
/// bars if one column per original bucket cannot fit. Never crop or aggregate.
/// Coordinates are measured in `resolution` horizontal units per terminal cell.
struct Geometry {
    origin: usize,
    plot: usize,
    resolution: usize,
    bar_width: usize,
    starts: Vec<usize>,
}

impl Geometry {
    fn new(width: u16, origin: usize, count: usize) -> Option<Self> {
        let plot = (width as usize).saturating_sub(origin);
        if count == 0 || plot.saturating_mul(2) < count {
            return None;
        }
        let resolution = if plot >= count { 1 } else { 2 };
        let available = plot * resolution;
        let mut bar_width = available / count;
        if bar_width.is_multiple_of(2) {
            bar_width -= 1;
        }
        let starts = (0..count)
            .map(|index| {
                let left = index * available / count;
                let right = (index + 1) * available / count;
                left + (right - left - bar_width) / 2
            })
            .collect();
        Some(Self { origin, plot, resolution, bar_width, starts })
    }

    fn centre(&self, index: usize) -> usize {
        (self.starts[index] + self.bar_width / 2) / self.resolution
    }

    fn owners(&self) -> Vec<Option<usize>> {
        let mut owners = vec![None; self.plot * self.resolution];
        for (index, start) in self.starts.iter().copied().enumerate() {
            owners[start..start + self.bar_width].fill(Some(index));
        }
        owners
    }
}

/// A very slight alternating shade separates touching bars without consuming
/// an empty column. The four semantic bands are selected BEFORE this tint.
fn bar_ink(share: f64, index: usize) -> Color {
    let color = bar_color(share);
    if index.is_multiple_of(2) {
        return color;
    }
    match color {
        Color::Rgb(r, g, b) => {
            let shade = |channel: u8| (u16::from(channel) * 94 / 100) as u8;
            Color::Rgb(shade(r), shade(g), shade(b))
        }
        _ => color,
    }
}

fn quantized_height(value: f64, max: f64, steps: usize) -> usize {
    if !value.is_finite() || value <= 0. || max <= 0. {
        0
    } else {
        ((value / max * steps as f64).round() as usize).min(steps)
    }
}

/// Two independent coloured half-columns in one terminal cell. A foreground
/// left half plus a background right half preserves BOTH colours; braille
/// with a single foreground would lose one bucket's colour. Dense mode uses
/// whole terminal-row heights because a cell cannot encode two differently
/// coloured partial caps plus transparent background (three colours).
fn half_cell(left: Option<Color>, right: Option<Color>) -> Span<'static> {
    let clean = Style::default().fg(Color::Reset).bg(Color::Reset);
    match (left, right) {
        (None, None) => Span::styled(" ", clean),
        (Some(l), None) => Span::styled("▌", clean.fg(l)),
        (None, Some(r)) => Span::styled("▐", clean.fg(r)),
        (Some(l), Some(r)) => Span::styled("▌", clean.fg(l).bg(r)),
    }
}

fn histogram(chart: &Chart<'_>, width: u16, rows: usize, origin: usize) -> Vec<Line<'static>> {
    let plot = (width as usize).saturating_sub(origin);
    let count = chart.series.len();
    if count == 0 || plot == 0 || rows == 0 || chart.tick_buckets == 0 {
        return vec![];
    }
    let last = chart.first_tick as usize + count.div_ceil(chart.tick_buckets) - 1;
    let unit = if chart.first_tick == 0 { "时" } else { "日" };
    let title = format!(" {} · 每柱 {} · {}-{}{}", chart.label,
        span_label(chart.span_seconds), chart.first_tick, last, unit);
    let mut result = vec![Line::styled(truncate(&title, width as usize),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD))];
    let Some(geometry) = Geometry::new(width, origin, count) else {
        // Even half-columns have a finite resolution. Do not silently crop,
        // invent a scrollbar, or combine buckets below this physical minimum.
        result.push(Line::styled(truncate(&format!(
            "完整 {} 柱需至少 {} 列", count, origin + count.div_ceil(2)), width as usize),
            Style::default().fg(MUTED)));
        return result;
    };
    let max = nice_ceiling(series_peak(&chart.series));
    let vertical = if geometry.resolution == 1 { 8 } else { 1 };
    let heights: Vec<_> = (0..count)
        .map(|index| quantized_height(chart.series.value(index), max, rows * vertical))
        .collect();
    let inks: Vec<_> = (0..count)
        .map(|index| bar_ink(chart.series.value(index) / max, index))
        .collect();
    let owners = geometry.owners();
    let ticks: Vec<_> = (0..count).step_by(chart.tick_buckets)
        .map(|index| (geometry.centre(index),
            (chart.first_tick as usize + index / chart.tick_buckets).to_string()))
        .collect();
    let tick_step = rows.div_ceil(4);
    for row in 0..rows {
        let head = if row.is_multiple_of(tick_step) {
            value_label(max * (rows - row) as f64 / rows as f64, max, chart.series.money())
        } else { String::new() };
        let mut spans = axis_prefix(&head, '┤', geometry.origin);
        let base = (rows - 1 - row) * vertical;
        if geometry.resolution == 1 {
            for owner in &owners {
                let (used, color) = owner.map_or((0, Color::Reset), |index| {
                    (heights[index].saturating_sub(base).min(8), inks[index])
                });
                let glyph = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'][used];
                spans.push(Span::styled(glyph.to_string(),
                    Style::default().fg(color).bg(Color::Reset)));
            }
        } else {
            for pair in owners.chunks_exact(2) {
                let ink = |owner: Option<usize>| owner
                    .filter(|index| heights[*index] > base).map(|index| inks[index]);
                spans.push(half_cell(ink(pair[0]), ink(pair[1])));
            }
        }
        result.push(Line::from(spans));
    }
    let mut rule = vec!['─'; geometry.plot];
    // A half-cell cannot have its own box-drawing tick. Dense mode keeps the
    // hour/day major ticks; full-cell mode also draws each bucket's minor tick.
    if geometry.resolution == 1 {
        for index in 0..count {
            rule[geometry.centre(index)] = '┴';
        }
    }
    for (centre, _) in &ticks { rule[*centre] = '┴'; }
    let mut baseline = axis_prefix(&value_label(0., max, chart.series.money()), '└', geometry.origin);
    for (column, glyph) in rule.into_iter().enumerate() {
        let major = ticks.iter().any(|(centre, _)| *centre == column);
        baseline.push(Span::styled(glyph.to_string(),
            Style::default().fg(if major { CYAN } else { TRACK })));
    }
    result.push(Line::from(baseline));
    result.extend(tick_labels(&ticks, &geometry));
    result
}

fn axis_prefix(label: &str, mark: char, origin: usize) -> Vec<Span<'static>> {
    let label = truncate(label, origin.saturating_sub(3));
    let padding = origin.saturating_sub(columns(&label) + 3);
    vec![
        Span::styled(format!(" {}{label} ", " ".repeat(padding)), Style::default().fg(MUTED)),
        Span::styled(mark.to_string(), Style::default().fg(TRACK)),
    ]
}

/// Keep ALL hour/day numbers. When adjacent two-digit labels cannot share a
/// row, use the existing blank separator row rather than drop labels or move
/// them off their tick. The second row stays blank when everything fits.
fn tick_labels(ticks: &[(usize, String)], geometry: &Geometry) -> [Line<'static>; 2] {
    let mut labels = [String::new(), String::new()];
    let mut next_free = [0usize; 2];
    for (centre, text) in ticks {
        let width = columns(text);
        let start = centre.saturating_sub(width / 2);
        if start + width > geometry.plot { continue; }
        if let Some(row) = (0..2).find(|row| start >= next_free[*row]) {
            labels[row].push_str(&" ".repeat(start.saturating_sub(columns(&labels[row]))));
            labels[row].push_str(text);
            next_free[row] = start + width + 1;
        }
    }
    labels.map(|row| Line::styled(format!("{}{row}", " ".repeat(geometry.origin)),
        Style::default().fg(MUTED)))
}
'''
p.write_text(s)

p = ROOT / 'ui/token/tests.rs'
s = p.read_text()
s = replace_once(s, 'TestBackend::new(width, rows as u16 + 3)', 'TestBackend::new(width, rows as u16 + 4)')
s = replace_once(s, 'histogram(chart, width, rows, origin, 0)', 'histogram(chart, width, rows, origin)')
a = s.index('#[test]\nfn native_buckets_never_merge_at_any_terminal_width()')
s = s[:a] + r'''#[test]
fn all_buckets_fit_without_mandatory_gaps_or_a_window() {
    for count in [96usize, 112, 116, 120, 124] {
        for plot in count.div_ceil(2)..=420 {
            let geometry = Geometry::new((plot + 8) as u16, 8, count).unwrap();
            assert_eq!(geometry.starts.len(), count);
            let owners = geometry.owners();
            for index in 0..count {
                assert_eq!(owners.iter().filter(|owner| **owner == Some(index)).count(), geometry.bar_width);
            }
            assert!(geometry.starts.windows(2).all(|pair| pair[0] + geometry.bar_width <= pair[1]));
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
        if count != 96 { chart.first_tick = 1; chart.span_seconds = 6 * 3600; }
        for width in (8 + count.div_ceil(2)) as u16..=260 {
            let rendered = histogram(&chart, width, 7, 8);
            let labels = text(&rendered[9..]);
            let got: Vec<_> = labels.split_whitespace().collect();
            for group in 0..count / 4 {
                let label = (chart.first_tick as usize + group).to_string();
                assert_eq!(got.iter().filter(|got| **got == label).count(), 1,
                    "count {count} width {width} missing {label}: {labels}");
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
        if quarter % 4 == 0 { assert_eq!(buffer[(x, 4)].fg, CYAN); }
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
        (0., BAR_LOW), (0.2999, BAR_LOW), (0.30, BAR_MID), (0.7999, BAR_MID),
        (0.80, BAR_HIGH), (0.9499, BAR_HIGH), (0.95, BAR_RED), (1., BAR_RED),
    ] { assert_eq!(bar_color(share), colour); }
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
            month: Bucketed { buckets: vec![100; days * 4], costs: vec![12_345.; days * 4] },
            ..UsageStats::default()
        };
        let charts = local_charts(&usage);
        let origin = plot_origin(&charts);
        for width in [(origin + days * 2) as u16, 100, 140, 260] {
            let tokens = histogram(&charts[1], width, 7, origin);
            let money = histogram(&charts[2], width, 7, origin);
            assert_eq!(tokens[8].to_string().split_once('└').unwrap().1,
                money[8].to_string().split_once('└').unwrap().1);
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
        for x in 9..(count / 2 + 7) as u16 { assert_eq!(buffer[(x, 1)].symbol(), " "); }
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
    assert!(buffer.content.iter().all(|cell| !cell.symbol().chars().any(|c| "█▌▐▁▂▃▄▅▆▇".contains(c))));
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
            for row in &rendered { assert!(row.width() <= width as usize, "{width}: {row}"); }
            if width >= 56 && rows > 0 { assert_eq!(rendered.len(), rows + 4); }
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
            if buffer[(x, 4)].symbol() != "█" { x += 1; continue; }
            let left = x;
            let color = buffer[(x, 4)].fg;
            while x < width && buffer[(x, 4)].symbol() == "█" && buffer[(x, 4)].fg == color { x += 1; }
            let right = x - 1;
            assert_eq!((right - left + 1) % 2, 1);
            assert_eq!(buffer[((left + right) / 2, 5)].symbol(), "┴");
            bars.push((left, right));
        }
        assert_eq!(bars.len(), 96, "width {width}");
    }
}
'''
p.write_text(s)

# No old panning state or keystrokes may remain in production code.
for p in [ROOT / 'main.rs', ROOT / 'ui.rs', ROOT / 'ui/token.rs']:
    s = p.read_text()
    for forbidden in ['chart_starts', 'pan_charts', 'chart_manual', 'Window::new', 'merge_factor']:
        assert forbidden not in s, (p, forbidden)
Path(__file__).unlink()
