from pathlib import Path
import hashlib

ROOT = Path('crates/ai-monitor/src')

def blob_sha(path):
    data = path.read_bytes()
    return hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()

def replace_once(text, before, after):
    assert text.count(before) == 1, f'Expected one patch anchor: {before[:100]}'
    return text.replace(before, after, 1)

def replace_test(text, name, replacement):
    start = text.index('#[test]\nfn ' + name + '(')
    end = text.find('\n#[test]', start + 1)
    if end < 0:
        end = len(text)
    return text[:start] + replacement.rstrip() + '\n' + text[end:]

p = ROOT / 'ui/token.rs'
assert blob_sha(p) == '77a441eb91a478750410f4227fec19ea240cb04d'
s = p.read_text()
s = replace_once(s, '#[cfg(test)]\nmod tests;', 'mod daily_money;\n#[cfg(test)]\nmod tests;')
s = replace_once(s,
    'pub(super) fn draw_charts(frame: &mut Frame, areas: &[Rect], usage: &UsageStats) {',
    'pub(super) fn draw_charts(frame: &mut Frame, areas: &[Rect], usage: &UsageStats, now: i64) {')
s = replace_once(s, '    let origin = plot_origin(&charts);\n    for (chart, area)', '''    let origin = plot_origin(&charts);
    let through_day = chrono::DateTime::from_timestamp(now, 0)
        .map(|time| chrono::Datelike::day(&time.with_timezone(&chrono::Local)) as usize)
        .unwrap_or(0);
    for (chart, area)''')
s = replace_once(s, '        if area.height >= MIN_CHART_HEIGHT {\n            frame.render_widget(', '''        if area.height >= MIN_CHART_HEIGHT {
            if chart.series.money() {
                daily_money::draw(frame, *area, chart, origin, through_day);
                continue;
            }
            frame.render_widget(''')
s = replace_once(s, '            let max = nice_ceiling(series_peak(&chart.series));', '''            let max = match &chart.series {
                Series::Money(costs) => daily_money::ceiling(costs),
                _ => nice_ceiling(series_peak(&chart.series)),
            };''')
start = s.index('/// Full-period geometry.')
end = s.index('/// A very slight alternating shade', start)
s = s[:start] + '''/// Full-period, gap-free tiling. Each shared edge ends one bucket AND starts
/// its neighbour. Rounding changes a bin's raster width by at most one unit;
/// it can never create unowned half-cells between positive adjacent buckets.
/// Do not centre a narrower, fixed-width bar inside independently rounded
/// slots: that was the source of the alternating blank/no-blank regression.
struct Geometry {
    origin: usize,
    plot: usize,
    resolution: usize,
    edges: Vec<usize>,
}

impl Geometry {
    fn new(width: u16, origin: usize, count: usize) -> Option<Self> {
        let plot = (width as usize).saturating_sub(origin);
        if count == 0 || plot.saturating_mul(2) < count {
            return None;
        }
        let resolution = if plot >= count { 1 } else { 2 };
        let available = plot * resolution;
        let edges = (0..=count).map(|index| index * available / count).collect();
        Some(Self { origin, plot, resolution, edges })
    }

    fn centre(&self, index: usize) -> usize {
        // The terminal column containing the bin midpoint. Half-cell mode
        // still has character-level ticks; it does not claim subpixel ticks.
        (self.edges[index] + self.edges[index + 1] - 1) / (2 * self.resolution)
    }

    fn owners(&self) -> Vec<Option<usize>> {
        let mut owners = vec![None; self.plot * self.resolution];
        for (index, edges) in self.edges.windows(2).enumerate() {
            owners[edges[0]..edges[1]].fill(Some(index));
        }
        owners
    }
}

''' + s[end:]
p.write_text(s)

p = ROOT / 'ui.rs'
assert blob_sha(p) == '435ead27e464b2c911957d7ae8e13315811251ca'
p.write_text(replace_once(p.read_text(),
    'token::draw_charts(frame, &token_parts[1..], usage);',
    'token::draw_charts(frame, &token_parts[1..], usage, now);'))

p = ROOT / 'ui/token/tests.rs'
assert blob_sha(p) == '708a10c808f77661cc15ac7da621ed500a48ba85'
s = p.read_text()
s = replace_test(s, 'all_buckets_fit_without_mandatory_gaps_or_a_window', r'''#[test]
fn every_native_bucket_tiles_the_plot_with_exactly_zero_unowned_units() {
    for count in [96usize, 112, 116, 120, 124] {
        for plot in count.div_ceil(2)..=420 {
            let geometry = Geometry::new((plot + 8) as u16, 8, count).unwrap();
            let owners = geometry.owners();
            assert!(owners.iter().all(Option::is_some), "gap: {count} bins, {plot} columns");
            assert_eq!(owners[0], Some(0));
            assert_eq!(owners.last(), Some(&Some(count - 1)));
            let minimum = owners.len() / count;
            for index in 0..count {
                let occupied = owners.iter().filter(|owner| **owner == Some(index)).count();
                assert!(occupied == minimum || occupied == minimum + 1);
            }
            assert!(owners.windows(2).all(|pair| pair[0] == pair[1]
                || pair[0].unwrap() + 1 == pair[1].unwrap()));
        }
    }
}''')
s = replace_test(s, 'awkward_widths_keep_bars_on_axis_and_all_major_labels', r'''#[test]
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
                    other => panic!("unexpected gap/glyph {other:?}, count {count}, width {width}, x {x}"),
                }
            }
            let transitions = raster.windows(2).filter(|pair| pair[0] != pair[1]).count();
            assert_eq!(transitions + 1, count, "all {count} adjacent bars must stay distinguishable at {width}");
        }
    }
}''')
s = replace_test(s, 'monthly_tokens_and_money_share_all_four_buckets_and_all_date_labels', r'''#[test]
fn daily_money_points_and_monthly_token_ticks_share_the_exact_date_columns() {
    for days in [28usize, 29, 30, 31] {
        let usage = UsageStats {
            month: Bucketed { buckets: vec![100; days * 4], costs: vec![12_345.; days * 4] },
            ..UsageStats::default()
        };
        let charts = local_charts(&usage);
        let origin = plot_origin(&charts);
        for width in [(origin + days * 2) as u16, 100, 140, 260] {
            let tokens = histogram(&charts[1], width, 7, origin);
            let mut terminal = Terminal::new(TestBackend::new(width, 11)).unwrap();
            terminal.draw(|frame| daily_money::draw(frame, frame.area(), &charts[2], origin, days)).unwrap();
            let buffer = terminal.backend().buffer();
            for (row, expected) in tokens.iter().enumerate().skip(9) {
                let actual: String = (0..width).map(|x| buffer[(x, row as u16)].symbol()).collect();
                assert_eq!(actual.trim_end(), expected.to_string().trim_end());
            }
            let geometry = Geometry::new(width, origin, days * 4).unwrap();
            for day in 0..days {
                let x = (origin + geometry.centre(day * 4)) as u16;
                assert_eq!((1..=8).filter(|row| buffer[(x, *row)].symbol() == "●").count(), 1);
            }
            assert_eq!(charts[1].series.len(), days * 4);
            assert_eq!(daily_money::totals(&usage.month.costs).len(), days);
        }
    }
}''')
s += r'''

#[test]
fn export_real_chart_terminal_fixtures() {
    use ratatui::backend::{Backend, CrosstermBackend};
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-12T12:00:00+08:00").unwrap().timestamp();
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
                    300_000_000, 1_100_000_000, 1_100_000_000,
                    1_100_000_000, 300_000_000, 300_000_000,
                ]);
            }
            for (day, cost) in [12., 25., 10., 0., 45., 20., 15., 30., 70., 18., 390., 110.].into_iter().enumerate() {
                usage.month.costs[day * 4] = cost;
            }
            let mut terminal = Terminal::new(TestBackend::new(width, 36)).unwrap();
            terminal.draw(|frame| {
                let areas = [Rect::new(0, 0, width, 12), Rect::new(0, 12, width, 12), Rect::new(0, 24, width, 12)];
                draw_charts(frame, &areas, &usage, now);
            }).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer.content.iter().filter(|cell| cell.symbol() == "●").count(), 12);
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
'''
p.write_text(s)

# Place the point glyph by integer terminal column, avoiding another floating
# projection of a tick. Repaint the baseline after Canvas so its blank layer
# cannot remove the axis; then overlay real-zero dots exactly on that baseline.
p = ROOT / 'ui/token/daily_money.rs'
s = p.read_text()
s = replace_once(s, '    frame.render_widget(Paragraph::new(axes), area);', '''    let baseline = axes[rows + 1].clone();
    frame.render_widget(Paragraph::new(axes), area);''')
a = s.index('            for day in 0..shown {')
b = s.index('        });', a)
s = s[:a] + s[b:]
s = replace_once(s, '    frame.render_widget(canvas, plot);', '''    frame.render_widget(canvas, plot);
    frame.render_widget(Paragraph::new(baseline),
        Rect::new(area.x, area.y + rows as u16 + 1, area.width, 1));
    for day in 0..shown {
        if let Some(value) = daily[day] {
            let y = ((1. - value / max) * rows as f64).round() as u16;
            frame.render_widget(
                Paragraph::new(Span::styled("●", Style::default().fg(bar_color(value / max)))),
                Rect::new(plot.x + ticks[day].0 as u16, plot.y + y.min(rows as u16), 1, 1),
            );
        }
    }''')
p.write_text(s)
print('Applied exact-source patches: no collector, pricing, database, or main-branch mutation.')
