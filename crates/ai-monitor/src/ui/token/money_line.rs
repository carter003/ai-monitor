//! Amounts use a line and sample markers, never filled columns. The timeline
//! comes from the same Geometry as the monthly token bars; no independent
//! x-axis normalisation, resampling, or viewport is introduced here.

use super::{BAR_HIGH, BAR_MID, Chart, Geometry, quantized_height};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Widget,
        canvas::{Canvas, Line as Segment, Points},
    },
};

/// Integer coordinates on Canvas's 2-by-4 braille lattice. Every native sample
/// retains its own x position, even when two samples share a terminal cell.
/// Invalid samples break the line; an actual zero remains a valid point.
fn samples(chart: &Chart<'_>, geometry: &Geometry, rows: usize, max: f64) -> Vec<Option<(usize, usize)>> {
    let top = rows.saturating_mul(4).saturating_sub(1);
    (0..chart.series.len())
        .map(|index| {
            let value = chart.series.value(index);
            if !value.is_finite() || value < 0. {
                return None;
            }
            let centre = geometry.starts[index] + geometry.bar_width / 2;
            let x = centre * 2 / geometry.resolution;
            Some((x, quantized_height(value, max, top)))
        })
        .collect()
}

pub(super) fn plot(chart: &Chart<'_>, geometry: &Geometry, rows: usize, max: f64) -> Vec<Line<'static>> {
    if rows == 0 || geometry.plot == 0 {
        return vec![];
    }
    let area = Rect::new(0, 0, geometry.plot as u16, rows as u16);
    let mut buffer = Buffer::empty(area);
    let points = samples(chart, geometry, rows, max);
    let top = rows * 4 - 1;
    Canvas::default()
        .background_color(Color::Reset)
        .marker(Marker::Braille)
        .x_bounds([0., (geometry.plot * 2 - 1).max(1) as f64])
        .y_bounds([0., top as f64])
        .paint(|context| {
            for pair in points.windows(2) {
                if let [Some((x1, y1)), Some((x2, y2))] = pair {
                    context.draw(&Segment {
                        x1: *x1 as f64,
                        y1: *y1 as f64,
                        x2: *x2 as f64,
                        y2: *y2 as f64,
                        color: BAR_MID,
                    });
                }
            }
            // In half-column mode a full-character bullet would overwrite
            // its neighbour. A two-dot vertical marker preserves each sample
            // on the fine lattice instead. Stroke and markers are NOT filled
            // down to zero. No layer reset: preserve connecting line pixels.
            if geometry.resolution == 2 {
                for (x, y) in points.iter().flatten() {
                    let neighbour = if *y == top { y.saturating_sub(1) } else { y + 1 };
                    context.draw(&Points {
                        coords: &[(*x as f64, *y as f64), (*x as f64, neighbour as f64)],
                        color: BAR_HIGH,
                    });
                }
            }
        })
        .render(area, &mut buffer);
    if geometry.resolution == 1 {
        // A true solid bullet at each sample. Project directly to the same
        // terminal column used by its bar/tick, not through a second scale.
        for (x, y) in points.iter().flatten() {
            buffer[((x / 2) as u16, ((top - y) / 4) as u16)]
                .set_symbol("●")
                .set_fg(BAR_HIGH)
                .set_bg(Color::Reset);
        }
    }
    (0..area.height)
        .map(|y| {
            Line::from(
                (0..area.width)
                    .map(|x| {
                        let cell = &buffer[(x, y)];
                        Span::styled(
                            cell.symbol().to_owned(),
                            Style::default().fg(cell.fg).bg(Color::Reset),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::token::Series;

    fn amount(values: &[f64]) -> Chart<'_> {
        Chart {
            label: "本月金额",
            series: Series::Money(values),
            span_seconds: 6 * 3_600,
            tick_buckets: 4,
            first_tick: 1,
        }
    }

    #[test]
    fn every_six_hour_sample_has_its_own_uniform_x_in_both_densities() {
        for count in [112usize, 116, 120, 124] {
            let values: Vec<_> = (0..count).map(|i| i as f64).collect();
            let chart = amount(&values);
            for width in [count / 2 + 8, count + 8, count * 3 + 19] {
                let geometry = Geometry::new(width as u16, 8, count).unwrap();
                let points = samples(&chart, &geometry, 10, count as f64);
                assert_eq!(points.len(), count);
                let xs: Vec<_> = points.iter().map(|p| p.unwrap().0).collect();
                let pitch = xs[1] - xs[0];
                assert!(pitch > 0);
                assert!(xs.windows(2).all(|pair| pair[1] - pair[0] == pitch));
                for (index, x) in xs.iter().enumerate() {
                    assert_eq!(x / 2, geometry.centre(index));
                }
            }
        }
    }

    #[test]
    fn full_column_mode_draws_round_markers_connected_by_unfilled_lines() {
        let values = [25., 75., 25., 75.];
        let geometry = Geometry::new(40, 8, values.len()).unwrap();
        let lines = plot(&amount(&values), &geometry, 10, 100.);
        let text = lines.iter().map(Line::to_string).collect::<Vec<_>>().join("\n");
        assert_eq!(text.matches('●').count(), values.len());
        assert!(text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)));
        assert!(!text.chars().any(|c| "█▌▐▁▂▃▄▅▆▇".contains(c)));
        // A line around 25..75% does not fill the bottom 20% as a bar would.
        assert!(lines[8..].iter().all(|line| line.to_string().trim().is_empty()));
    }

    #[test]
    fn missing_samples_break_connections_and_zero_is_not_missing() {
        let chart = amount(&[0., f64::NAN, 50., f64::INFINITY, -1., 100.]);
        let geometry = Geometry::new(26, 8, 6).unwrap();
        let points = samples(&chart, &geometry, 10, 100.);
        assert_eq!(points[0].unwrap().1, 0);
        assert!(points[1].is_none() && points[3].is_none() && points[4].is_none());
        let lines = plot(&chart, &geometry, 10, 100.);
        let text = lines.iter().map(Line::to_string).collect::<Vec<_>>().join("\n");
        assert_eq!(text.matches('●').count(), 3);
        assert!(!text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)));
    }

    #[test]
    fn dense_markers_keep_independent_neighbour_heights_without_filled_columns() {
        let chart = amount(&[25., 75., 25., 75.]);
        let geometry = Geometry::new(10, 8, 4).unwrap();
        assert_eq!(geometry.resolution, 2);
        let points = samples(&chart, &geometry, 10, 100.);
        assert_eq!(points.iter().map(|p| p.unwrap().0).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        assert!(points[0].unwrap().1 < points[1].unwrap().1);
        let lines = plot(&chart, &geometry, 10, 100.);
        assert!(lines.iter().all(|line| line.width() == 2));
        assert!(lines[8..].iter().all(|line| line.to_string().trim().is_empty()));
        assert!(lines.iter().flat_map(|line| &line.spans).all(|span| span.style.bg == Some(Color::Reset)));
    }
}
