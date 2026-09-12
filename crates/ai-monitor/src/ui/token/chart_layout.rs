//! Calendar geometry and the monthly ranking, expressed in terminal cells.

use super::{
    CYAN, Chart, GREEN, INK, MONTH_MODEL_MAX_BYTES, MUTED, ModelUsage, columns, compact, money,
    truncate,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

#[cfg(test)]
mod tests;

const RANKING_MIN_WIDTH: usize = 16;
const RANKING_NAME_MIN: usize = 10;
const MONEY_CHART_MIN_WIDTH: u16 = 24;
const PANEL_GAP: u16 = 2;

/// A whole number of terminal cells per visible tick interval. Both labels and
/// data use this scale; changing label density must never shift a day's data.
#[derive(Clone, Copy, Debug)]
pub(super) struct TimeAxis {
    pub(super) step: usize,
    pitch: usize,
    pub(super) width: usize,
}

impl TimeAxis {
    pub(super) fn new(chart: &Chart<'_>, available: usize) -> Self {
        let units = chart.axis_units;
        if units == 0 || available == 0 {
            return Self {
                step: 1,
                pitch: 0,
                width: 0,
            };
        }
        let available = if chart.aggregate_daily && available >= units {
            units
        } else {
            available
        };
        let label_width = (chart.first_tick as usize + units - 1).to_string().len();
        // Pick an arithmetic progression, not rounded samples of an arbitrary
        // label count. Leave two cells between the widest adjacent labels.
        let step = [1, 2, 5, 10, 15, 30, units]
            .into_iter()
            .map(|step| step.min(units))
            .find(|step| available * step / units >= label_width + 2 || *step == units)
            .unwrap_or(units);
        let pitch = available * step / units;
        Self {
            step,
            pitch,
            width: (units * pitch).div_ceil(step),
        }
    }

    pub(super) fn slot_x(self, index: usize, buckets_per_unit: usize) -> usize {
        if self.width == 0 || buckets_per_unit == 0 {
            return 0;
        }
        ((2 * index + 1) * self.pitch / (2 * buckets_per_unit * self.step)).min(self.width - 1)
    }
}

pub(super) fn tick_positions(chart: &Chart<'_>, available: usize) -> Vec<(usize, String)> {
    let axis = TimeAxis::new(chart, available);
    if axis.width == 0 || chart.axis_units == 0 {
        return vec![];
    }
    // Anchor the progression at the final calendar day/hour. September always
    // ends at 30; a narrow chart uses 5, 10, 15, 20, 25, 30 rather than uneven
    // jumps such as 1, 3, 5, 7, 9, 12. The domain still includes every day.
    let first = (chart.axis_units - 1) % axis.step;
    (first..chart.axis_units)
        .step_by(axis.step)
        .map(|index| {
            (
                axis.slot_x(index, 1),
                (chart.first_tick as usize + index).to_string(),
            )
        })
        .filter(|(x, label)| label.len() <= x + 1)
        .collect()
}

/// Split only the money chart's row. The monthly Token rectangle is returned
/// unchanged: the ranking must never borrow its width, height, or position.
/// If the bottom row cannot fit two readable panels, keep the full-width chart.
pub(super) fn monthly_areas(areas: &[Rect]) -> Option<([Rect; 2], Rect)> {
    let tokens = *areas.get(1)?;
    let cost = *areas.get(2)?;
    let available = cost.width.checked_sub(PANEL_GAP)?;
    let left_width = available / 2;
    let right_width = available - left_width;
    if cost.height < super::MIN_CHART_HEIGHT
        || left_width < MONEY_CHART_MIN_WIDTH
        || (right_width as usize) < RANKING_MIN_WIDTH
    {
        return None;
    }
    let ranking = Rect::new(
        cost.x + left_width + PANEL_GAP,
        cost.y,
        right_width,
        cost.height,
    );
    Some((
        [tokens, Rect::new(cost.x, cost.y, left_width, cost.height)],
        ranking,
    ))
}

fn truncate_bytes(value: &str, max_bytes: usize) -> String {
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn model_name(model: &str, width: usize) -> String {
    let basename = model.rsplit('/').next().unwrap_or(model);
    let name = truncate_bytes(basename, MONTH_MODEL_MAX_BYTES);
    // A terminal-cell ellipsis is three UTF-8 bytes, so apply the byte cap to
    // the final display name too, not just to the input before truncation.
    truncate_bytes(&truncate(&name, width), MONTH_MODEL_MAX_BYTES)
}

fn ranking_table(models: &[ModelUsage], width: usize) -> Vec<Line<'static>> {
    if width < RANKING_MIN_WIDTH {
        return vec![];
    }
    let taken = &models[..models.len().min(10)];
    let costs: Vec<_> = taken
        .iter()
        .map(|model| model.cost.map(money).unwrap_or_else(|| "—".into()))
        .collect();
    let tokens: Vec<_> = taken
        .iter()
        .map(|model| compact(model.total_tokens()))
        .collect();
    let cost_width = costs
        .iter()
        .map(|text| columns(text))
        .max()
        .unwrap_or(0)
        .max(4);
    let token_width = tokens
        .iter()
        .map(|text| columns(text))
        .max()
        .unwrap_or(0)
        .max(5);
    let mut widths = [3, RANKING_NAME_MIN, cost_width, token_width];
    let mut enabled = [true; 4];
    let required = |enabled: &[bool; 4]| {
        let count = enabled.iter().filter(|show| **show).count();
        2 + count - 1
            + (0..4)
                .filter(|index| enabled[*index])
                .map(|index| widths[index])
                .sum::<usize>()
    };
    // Remove secondary fields before squeezing the model name or clipping a
    // number. Every displayed row and its header use the same chosen columns.
    for optional in [2, 3] {
        if required(&enabled) <= width {
            break;
        }
        enabled[optional] = false;
    }
    if required(&enabled) > width {
        return vec![];
    }
    let count = enabled.iter().filter(|show| **show).count();
    let fixed = 2 + count - 1
        + (0..4)
            .filter(|index| *index != 1 && enabled[*index])
            .map(|index| widths[index])
            .sum::<usize>();
    widths[1] = (width - fixed).min(MONTH_MODEL_MAX_BYTES);
    let free = width
        - 2
        - (0..4)
            .filter(|index| enabled[*index])
            .map(|index| widths[index])
            .sum::<usize>();
    let gap = free / (count - 1);
    let remainder = free % (count - 1);
    let left_padding = 1 + remainder / 2;
    let right_padding = 1 + remainder - remainder / 2;
    let last = (0..4).rfind(|index| enabled[*index]).unwrap_or(1);
    let row = |cells: [(String, Color); 4]| {
        let mut spans = vec![Span::raw(" ".repeat(left_padding))];
        for (index, (text, color)) in cells.into_iter().enumerate() {
            if !enabled[index] {
                continue;
            }
            let padding = " ".repeat(widths[index].saturating_sub(columns(&text)));
            let text = if index == 1 {
                format!("{text}{padding}")
            } else {
                format!("{padding}{text}")
            };
            spans.push(Span::styled(text, Style::default().fg(color)));
            if index != last {
                spans.push(Span::raw(" ".repeat(gap)));
            }
        }
        spans.push(Span::raw(" ".repeat(right_padding)));
        Line::from(spans)
    };
    let header = ["#", "模型", "金额", "Token"].map(|text| (text.into(), MUTED));
    let mut result = vec![row(header)];
    for (index, model) in taken.iter().enumerate() {
        result.push(row([
            (format!("{}.", index + 1), MUTED),
            (model_name(&model.model, widths[1]), CYAN),
            (
                costs[index].clone(),
                if model.cost.is_some() { GREEN } else { MUTED },
            ),
            (tokens[index].clone(), INK),
        ]));
    }
    result
}

pub(super) fn draw_monthly_ranking(frame: &mut Frame, area: Rect, models: &[ModelUsage]) {
    if (area.width as usize) < RANKING_MIN_WIDTH || area.height < 2 {
        return;
    }
    let show_header = area.height >= 6;
    let reserved = 1 + u16::from(show_header);
    let count = (area.height - reserved).min(10) as usize;
    let taken = &models[..models.len().min(count)];
    let table = ranking_table(taken, area.width as usize);
    if table.is_empty() {
        return;
    }
    let title = if taken.is_empty() {
        " 本月模型 · Token".to_owned()
    } else {
        let count = taken.len();
        [
            format!(" 本月模型 · Token TOP {count}"),
            format!(" 本月模型 · TOP {count}"),
            format!(" TOP {count}"),
        ]
        .into_iter()
        .find(|text| columns(text) <= area.width as usize)
        .unwrap_or_default()
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            truncate(&title, area.width as usize),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        )),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if show_header {
        frame.render_widget(
            Paragraph::new(table[0].clone()),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
    if taken.is_empty() {
        frame.render_widget(
            Paragraph::new(truncate(" 暂无本月用量记录", area.width as usize))
                .style(Style::default().fg(MUTED)),
            Rect::new(area.x, area.y + reserved, area.width, 1),
        );
        return;
    }
    // Exactly one terminal row per model, with no blank spacer rows. Height
    // limits the number of models, never the height of the adjacent chart.
    for (index, line) in table.into_iter().skip(1).enumerate() {
        let y = area.y + reserved + index as u16;
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
    }
}
