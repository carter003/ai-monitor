use super::{AMBER, GREEN, MUTED, RED, columns, truncate};
use crate::network::{NetworkState, ROUTES};
use ratatui::{style::Style, text::Line};
use std::time::{Duration, Instant};

fn latency(value: Option<Duration>) -> String {
    value
        .map(|v| format!("{}ms", v.as_millis()))
        .unwrap_or_else(|| "—".into())
}

pub(super) fn lines(state: &NetworkState, width: u16, now: Instant) -> Vec<Line<'static>> {
    let mut rows = vec![];
    let width = width as usize;
    if width >= 24 {
        rows.push(Line::styled(
            if width >= 36 {
                "              建连    响应   连通率"
            } else {
                "   建连    响应   连通率"
            },
            Style::default().fg(MUTED),
        ));
    }
    for (index, (name, _)) in ROUTES.iter().enumerate() {
        let route = &state.routes[index];
        let probe = route.current.as_ref();
        let rate = route.rate(now);
        let color = match probe {
            None => MUTED,
            Some(p) if p.status.is_none() => RED,
            Some(p)
                if p.note.is_some()
                    || p.response.is_some_and(|v| v > Duration::from_millis(1500))
                    || rate.is_some_and(|v| v < 99.) =>
            {
                AMBER
            }
            _ => GREEN,
        };
        let style = Style::default().fg(color);
        let connect = latency(probe.and_then(|p| p.connect));
        let response = latency(probe.and_then(|p| p.response));
        let rate = rate
            .map(|v| format!("{v:.1}%"))
            .unwrap_or_else(|| "采样中".into());
        let values = format!(
            "{connect:>6} {response:>7} {}{rate}",
            " ".repeat(7usize.saturating_sub(columns(&rate)))
        );
        if width >= 36 {
            rows.push(Line::styled(format!("{name:<12}{values}"), style));
        } else if width >= 24 {
            rows.push(Line::styled(name.to_string(), style));
            rows.push(Line::styled(values, style));
        } else {
            rows.push(Line::styled(truncate(name, width), style));
            for (label, value) in [("建连", connect), ("响应", response), ("连通率", rate)] {
                let text = format!("{label} {value}");
                if columns(&text) <= width {
                    rows.push(Line::styled(text, style));
                } else {
                    rows.push(Line::styled(truncate(label, width), style));
                    // Never turn a clipped numeric reading into a different number.
                    rows.push(Line::styled(
                        if columns(&value) <= width {
                            value
                        } else {
                            "—".into()
                        },
                        style,
                    ));
                }
            }
        }
        if let Some(note) = probe.and_then(|p| p.note.as_deref()) {
            rows.push(Line::styled(truncate(note, width), style));
        }
    }
    let footer = format!("每{}秒探测 · 连通率近5分钟", state.interval.as_secs());
    if columns(&footer) <= width {
        rows.push(Line::styled(footer, Style::default().fg(MUTED)));
    } else {
        for text in [
            format!("每{}秒探测", state.interval.as_secs()),
            "连通率近5分钟".into(),
        ] {
            rows.push(Line::styled(
                truncate(&text, width),
                Style::default().fg(MUTED),
            ));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::UsageStats,
        network::{NetworkUpdate, Probe},
        system::SystemStats,
        ui::{self, View},
    };
    use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};
    fn sample() -> NetworkState {
        let mut state = NetworkState::new(true, Duration::from_secs(10));
        for route in 0..4 {
            for _ in 0..3 {
                state.apply(NetworkUpdate {
                    route,
                    at: Instant::now(),
                    probe: Probe {
                        connect: Some(Duration::from_millis(1220)),
                        response: Some(Duration::from_millis(4580)),
                        status: Some(200),
                        note: None,
                        retry_after: Duration::ZERO,
                    },
                });
            }
        }
        state
    }
    #[test]
    fn widths_and_chinese_never_clip_numeric_readings() {
        let state = sample();
        for width in 1..=80 {
            let rows = lines(&state, width, Instant::now());
            assert!(
                rows.iter().all(|row| row.width() <= width as usize),
                "width={width}: {rows:?}"
            );
            let mut terminal = Terminal::new(TestBackend::new(width, rows.len() as u16)).unwrap();
            terminal
                .draw(|f| f.render_widget(Paragraph::new(rows.clone()), f.area()))
                .unwrap();
            if width >= 13 {
                let text = format!("{:?}", terminal.backend().buffer());
                assert!(text.contains("1220ms"));
                assert!(text.contains("4580ms"));
                assert!(text.contains("100.0%"));
            }
        }
    }
    #[test]
    fn fixed_panel_and_short_scroll_fallback() {
        let state = sample();
        for (width, height, fixed) in [
            (120, 40, true),
            (80, 40, true),
            (40, 60, true),
            (120, 24, false),
            (26, 12, false),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut view = View::default();
            terminal
                .draw(|f| {
                    ui::draw(
                        f,
                        &SystemStats::default(),
                        &[],
                        &UsageStats::default(),
                        &state,
                        &mut view,
                        100,
                    )
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text = format!("{buffer:?}");
            assert!(text.contains("AI 线路"), "{width}x{height}: {text}");
            if fixed {
                assert!(text.contains("Antigravity"));
                assert!(text.contains("4580ms"));
            } else {
                assert!(view.max_scroll > 0);
            }
            view.scroll = usize::MAX;
            terminal
                .draw(|f| {
                    ui::draw(
                        f,
                        &SystemStats::default(),
                        &[],
                        &UsageStats::default(),
                        &state,
                        &mut view,
                        100,
                    )
                })
                .unwrap();
            assert!(view.scroll <= view.max_scroll);
        }
    }
    #[test]
    fn failures_show_no_old_latency_and_service_notes_are_visible() {
        let mut state = sample();
        state.apply(NetworkUpdate {
            route: 0,
            at: Instant::now(),
            probe: Probe {
                connect: None,
                response: None,
                status: None,
                note: Some("TLS失败".into()),
                retry_after: Duration::ZERO,
            },
        });
        let rows = lines(&state, 36, Instant::now());
        assert!(!rows[1].to_string().contains("1220ms"));
        assert!(rows[1].to_string().contains('—'));
        assert_eq!(rows[1].style.fg, Some(RED));
        assert!(rows.iter().any(|r| r.to_string().contains("TLS失败")));
        assert_eq!(rows[3].style.fg, Some(AMBER));
    }
}
