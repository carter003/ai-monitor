from pathlib import Path
import hashlib


def read_expected(path, expected):
    file = Path(path)
    raw = file.read_bytes()
    actual = hashlib.sha1(b'blob ' + str(len(raw)).encode() + b'\0' + raw).hexdigest()
    if actual != expected:
        raise SystemExit(f'Refusing to overwrite changed source: {path} {actual}')
    return file, raw.decode()


def once(text, old, new):
    if text.count(old) != 1:
        raise SystemExit(f'Expected one patch anchor, got {text.count(old)}: {old[:80]}')
    return text.replace(old, new, 1)


file, source = read_expected('crates/ai-monitor/src/ui/token.rs', '77a441eb91a478750410f4227fec19ea240cb04d')
source = once(source, '#[cfg(test)]\nmod tests;', 'mod money_line;\n\n#[cfg(test)]\nmod tests;')
source = once(source, '''/// Full-period geometry. First remove compulsory gaps, then use half-cell
/// bars if one column per original bucket cannot fit. Never crop or aggregate.
/// Coordinates are measured in `resolution` horizontal units per terminal cell.''', '''/// Fixed-pitch, zero-gap packing for the ENTIRE period. Width selects one
/// uniform bar width/resolution. Remainder units belong only to the two outer
/// margins, never to individual bucket slots. In particular, rounding the
/// left/right edge of EACH proportional slot would reintroduce irregular gaps.
/// Coordinates are measured in `resolution` horizontal units per terminal cell.''')
source = once(source, '''        let starts = (0..count)
            .map(|index| {
                let left = index * available / count;
                let right = (index + 1) * available / count;
                left + (right - left - bar_width) / 2
            })
            .collect();''', '''        let padding = (available - count * bar_width) / 2;
        let starts = (0..count)
            .map(|index| padding + index * bar_width)
            .collect();''')
source = once(source, '''    let title = format!(
        " {} · 每柱 {} · {}-{}{}",
        chart.label,
        span_label(chart.span_seconds),''', '''    let mark = if chart.series.money() { "点" } else { "柱" };
    let title = format!(
        " {} · 每{} {} · {}-{}{}",
        chart.label,
        mark,
        span_label(chart.span_seconds),''')
source = once(source, '''    let max = nice_ceiling(series_peak(&chart.series));
    let vertical = if geometry.resolution == 1 { 8 } else { 1 };''', '''    let max = nice_ceiling(series_peak(&chart.series));
    let money_plot = chart.series.money().then(|| money_line::plot(chart, &geometry, rows, max));
    let vertical = if geometry.resolution == 1 { 8 } else { 1 };''')
source = once(source, '''        let base = (rows - 1 - row) * vertical;
        if geometry.resolution == 1 {''', '''        let base = (rows - 1 - row) * vertical;
        if let Some(ref plot) = money_plot {
            spans.extend(plot[row].spans.iter().cloned());
        } else if geometry.resolution == 1 {''')
file.write_text(source)

file, tests = read_expected('crates/ai-monitor/src/ui/token/tests.rs', '708a10c808f77661cc15ac7da621ed500a48ba85')
tests = once(tests, 'pair[0] + geometry.bar_width <= pair[1]', 'pair[0] + geometry.bar_width == pair[1]')
tests = once(tests, '''        assert_eq!(buffer[(8, 10)].fg, BAR_MID);
    }
    assert_eq!(quantized_height''', '''        let first_bar = (8..width)
            .map(|x| &buffer[(x, 10)])
            .find(|cell| !cell.symbol().trim().is_empty())
            .expect("the period must retain its first nonzero bucket");
        assert_eq!(first_bar.fg, BAR_MID);
    }
    assert_eq!(quantized_height''')
tests += r'''

/// Decode what is actually visible, not Geometry's ownership assertions.
/// Each entry is one physical half-column of the rendered bottom plot row.
fn visible_halves(buffer: &Buffer, width: u16, row: u16) -> Vec<Option<Color>> {
    let mut result = Vec::new();
    for x in 8..width {
        let cell = &buffer[(x, row)];
        match cell.symbol() {
            "█" => result.extend([Some(cell.fg), Some(cell.fg)]),
            "▌" => result.extend([
                Some(cell.fg),
                (cell.bg != Color::Reset).then_some(cell.bg),
            ]),
            "▐" => result.extend([None, Some(cell.fg)]),
            " " => result.extend([None, None]),
            other => panic!("unexpected glyph at {x}: {other}"),
        }
    }
    result
}

#[test]
fn equal_nonzero_bars_have_identical_pitch_and_zero_internal_gaps_in_real_buffer() {
    // Crucially include remainders: tests at exactly 96 or 120 columns alone
    // cannot reproduce the old per-slot rounding defect in the screenshot.
    for count in [96usize, 112, 116, 120, 124] {
        let values = vec![100u64; count];
        for plot in count / 2..=420 {
            let width = (plot + 8) as u16;
            let buffer = render_chart(&hourly(&values), width, 4, 8);
            let halves = visible_halves(&buffer, width, 4);
            let first = halves.iter().position(Option::is_some).unwrap();
            let last = halves.iter().rposition(Option::is_some).unwrap();
            let active = &halves[first..=last];
            assert!(active.iter().all(Option::is_some), "internal gap: count {count}, plot {plot}");
            let mut widths = Vec::new();
            let mut start = 0;
            for index in 1..=active.len() {
                if index == active.len() || active[index] != active[start] {
                    widths.push(index - start);
                    start = index;
                }
            }
            assert_eq!(widths.len(), count, "missing/combined bucket: count {count}, plot {plot}");
            assert!(widths.iter().all(|width| *width == widths[0]), "unequal widths: {widths:?}");
            // All unused width is OUTSIDE the period, approximately symmetric.
            assert!(first.abs_diff(halves.len() - last - 1) <= 2);
        }
    }
}

#[test]
fn a_real_zero_bucket_keeps_exactly_its_own_slot_not_an_extra_layout_gap() {
    let mut values = vec![100u64; 96];
    values[2] = 0;
    for plot in [48usize, 49, 92, 96, 97, 182, 288, 301] {
        let width = (plot + 8) as u16;
        let buffer = render_chart(&hourly(&values), width, 4, 8);
        let halves = visible_halves(&buffer, width, 4);
        let first = halves.iter().position(Option::is_some).unwrap();
        let last = halves.iter().rposition(Option::is_some).unwrap();
        let active = &halves[first..=last];
        let pitch = active.len() / 96;
        assert_eq!(active.len(), pitch * 96);
        for (index, half) in active.iter().enumerate() {
            assert_eq!(half.is_none(), (2 * pitch..3 * pitch).contains(&index));
        }
    }
}

#[test]
fn production_amount_chart_is_a_dotted_line_with_the_same_monthly_time_axis() {
    let usage = UsageStats {
        month: Bucketed {
            buckets: vec![100; 120],
            costs: (0..120).map(|i| if i % 2 == 0 { 25. } else { 75. }).collect(),
        },
        ..UsageStats::default()
    };
    let charts = local_charts(&usage);
    let origin = plot_origin(&charts);
    for width in [68u16, 100, 128, 139, 400] {
        let tokens = histogram(&charts[1], width, 10, origin);
        let money = histogram(&charts[2], width, 10, origin);
        assert!(money[0].to_string().contains("每点 6小时"));
        let plotted = text(&money[1..11]);
        assert!(!plotted.chars().any(|c| "█▌▐▁▂▃▄▅▆▇".contains(c)));
        if width as usize >= origin + 120 {
            assert_eq!(plotted.matches('●').count(), 120);
        } else {
            assert!(plotted.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)));
        }
        assert_eq!(tokens[11].to_string().split_once('└').unwrap().1,
                   money[11].to_string().split_once('└').unwrap().1);
        assert_eq!(text(&tokens[12..]), text(&money[12..]));
    }
}
'''
file.write_text(tests)
