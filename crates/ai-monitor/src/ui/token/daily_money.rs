//! Daily spending is a separate presentation of the four stored six-hour costs.
//! Never change the collector, pricing, or the native token-chart buckets.

use super::{
    BAR_MID, CYAN, Chart, Geometry, MUTED, Series, TRACK, axis_prefix, bar_color, chart_rows,
    nice_ceiling, tick_labels, truncate, value_label,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Paragraph,
        canvas::{Canvas, Line as CanvasLine},
    },
};

/// An invalid component makes the day's amount unknown, not a fabricated zero.
/// Keep the partial final group; ordinary production months have four per day.
pub(super) fn totals(costs: &[f64]) -> Vec<Option<f64>> {
    costs
        .chunks(4)
        .map(|day| {
            if day.iter().any(|value| !value.is_finite() || *value < 0.) {
                return None;
            }
            let sum: f64 = day.iter().sum();
            sum.is_finite().then_some(sum)
        })
        .collect()
}

pub(super) fn ceiling(costs: &[f64]) -> f64 {
    nice_ceiling(totals(costs).into_iter().flatten().fold(0., f64::max))
}

pub(super) fn draw(
    frame: &mut Frame,
    area: Rect,
    chart: &Chart<'_>,
    origin: usize,
    through_day: usize,
) {
    let Series::Money(costs) = &chart.series else {
        return;
    };
    let rows = chart_rows(area.height);
    if rows == 0 || costs.is_empty() || area.width as usize <= origin {
        return;
    }
    let daily = totals(costs);
    let title = format!(" 本月金额 · 每点 1天 · 1-{}日", daily.len());
    // Use the EXACT six-hour timeline of the token chart. A daily point sits
    // on its date's major tick, not on a separately stretched 1..31 axis.
    let Some(geometry) = Geometry::new(area.width, origin, costs.len()) else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    truncate(&title, area.width as usize),
                    Style::default().fg(CYAN),
                ),
                Line::styled(
                    truncate("请拉宽查看完整月份", area.width as usize),
                    Style::default().fg(MUTED),
                ),
            ]),
            area,
        );
        return;
    };
    let shown = through_day.min(daily.len());
    let max = nice_ceiling(daily[..shown].iter().flatten().copied().fold(0., f64::max));
    let ticks: Vec<_> = (0..daily.len())
        .map(|day| (geometry.centre(day * 4), (day + 1).to_string()))
        .collect();
    let mut axes = vec![Line::styled(
        truncate(&title, area.width as usize),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    )];
    let tick_step = rows.div_ceil(4);
    for row in 0..rows {
        let label = if row.is_multiple_of(tick_step) {
            value_label(max * (rows - row) as f64 / rows as f64, max, true)
        } else {
            String::new()
        };
        axes.push(Line::from(axis_prefix(&label, '┤', origin)));
    }
    let mut baseline = axis_prefix(&value_label(0., max, true), '└', origin);
    for column in 0..geometry.plot {
        let major = ticks.iter().any(|(x, _)| *x == column);
        baseline.push(Span::styled(
            if major { "┴" } else { "─" },
            Style::default().fg(if major { CYAN } else { TRACK }),
        ));
    }
    axes.push(Line::from(baseline));
    axes.extend(tick_labels(&ticks, &geometry));
    let baseline = axes[rows + 1].clone();
    frame.render_widget(Paragraph::new(axes), area);

    // Include the zero-baseline row: a genuine zero gets a point ON zero,
    // not a made-up positive minimum height. Future days keep their dates
    // but have neither points nor a misleading line returning to zero.
    let plot = Rect::new(
        area.x + origin as u16,
        area.y + 1,
        geometry.plot as u16,
        rows as u16 + 1,
    );
    let canvas = Canvas::default()
        .background_color(Color::Reset)
        .marker(Marker::Braille)
        .x_bounds([0., geometry.plot.saturating_sub(1).max(1) as f64])
        .y_bounds([0., max])
        .paint(|context| {
            for day in 1..shown {
                if let (Some(previous), Some(value)) = (daily[day - 1], daily[day]) {
                    context.draw(&CanvasLine {
                        x1: ticks[day - 1].0 as f64,
                        y1: previous,
                        x2: ticks[day].0 as f64,
                        y2: value,
                        color: BAR_MID,
                    });
                }
            }
        });
    frame.render_widget(canvas, plot);
    frame.render_widget(
        Paragraph::new(baseline),
        Rect::new(area.x, area.y + rows as u16 + 1, area.width, 1),
    );
    for day in 0..shown {
        if let Some(value) = daily[day] {
            let y = ((1. - value / max) * rows as f64).round() as u16;
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "●",
                    Style::default().fg(bar_color(value / max)),
                )),
                Rect::new(
                    plot.x + ticks[day].0 as u16,
                    plot.y + y.min(rows as u16),
                    1,
                    1,
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn render(costs: &[f64], width: u16, through_day: usize) -> Buffer {
        let chart = Chart {
            label: "本月金额",
            series: Series::Money(costs),
            span_seconds: 6 * 3600,
            tick_buckets: 4,
            first_tick: 1,
        };
        let mut terminal = Terminal::new(TestBackend::new(width, 14)).unwrap();
        terminal
            .draw(|frame| draw(frame, frame.area(), &chart, 8, through_day))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn four_six_hour_costs_become_one_daily_amount_without_changing_the_sum() {
        let costs = [1., 2., 3., 4., 5., 6., 7., 8., 9.];
        assert_eq!(totals(&costs), vec![Some(10.), Some(26.), Some(9.)]);
        assert_eq!(
            totals(&costs).into_iter().flatten().sum::<f64>(),
            costs.iter().sum::<f64>()
        );
        assert_eq!(totals(&[0.; 4]), vec![Some(0.)]);
        assert_eq!(totals(&[1., f64::NAN, 0., 0.]), vec![None]);
        assert_eq!(totals(&[1., f64::INFINITY, 0., 0.]), vec![None]);
        assert_eq!(totals(&[-1., 0., 0., 0.]), vec![None]);
    }

    #[test]
    fn one_point_per_elapsed_day_and_no_future_zero_line() {
        for days in [28usize, 29, 30, 31] {
            let mut costs = vec![0.; days * 4];
            for day in 0..12 {
                costs[day * 4] = (day % 4 * 25) as f64;
            }
            for width in [(8 + days * 2) as u16, 100, 133, 180] {
                let buffer = render(&costs, width, 12);
                let points: Vec<_> = buffer
                    .content
                    .iter()
                    .enumerate()
                    .filter(|(_, cell)| cell.symbol() == "●")
                    .map(|(offset, _)| (offset % width as usize, offset / width as usize))
                    .collect();
                assert_eq!(points.len(), 12, "{days} days, {width} columns");
                let geometry = Geometry::new(width, 8, costs.len()).unwrap();
                for day in 0..12 {
                    assert_eq!(
                        points
                            .iter()
                            .filter(|(x, _)| *x == 8 + geometry.centre(day * 4))
                            .count(),
                        1
                    );
                }
                // The zero on day 1 is a real marker on the baseline, not missing data.
                assert_eq!(buffer[(8 + geometry.centre(0) as u16, 11)].symbol(), "●");
                // Date labels continue, but no point or curve exists after day 12.
                for x in 8 + geometry.centre(12 * 4)..width as usize {
                    for y in 1..=10 {
                        assert!(buffer[(x as u16, y)].symbol().trim().is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn actual_curve_connects_daily_points_and_does_not_draw_money_bars() {
        let mut costs = vec![0.; 120];
        costs[0] = 10.;
        costs[4] = 90.;
        costs[8] = 20.;
        let buffer = render(&costs, 140, 3);
        assert_eq!(
            buffer
                .content
                .iter()
                .filter(|cell| cell.symbol() == "●")
                .count(),
            3
        );
        assert!(buffer.content.iter().any(|cell| {
            cell.symbol()
                .chars()
                .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
        }));
        assert!(
            !buffer
                .content
                .iter()
                .any(|cell| cell.symbol().chars().any(|ch| "█▌▐▁▂▃▄▅▆▇".contains(ch)))
        );
    }

    #[test]
    fn invalid_day_breaks_the_curve_instead_of_fabricating_a_point() {
        let mut costs = vec![0.; 120];
        costs[0] = 10.;
        costs[4] = f64::NAN;
        costs[8] = 20.;
        let buffer = render(&costs, 140, 3);
        assert_eq!(
            buffer
                .content
                .iter()
                .filter(|cell| cell.symbol() == "●")
                .count(),
            2
        );
        assert!(!buffer.content.iter().any(|cell| {
            cell.symbol()
                .chars()
                .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
        }));
    }
}
