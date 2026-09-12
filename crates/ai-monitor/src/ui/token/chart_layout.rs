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

const RANKING_MIN_HEIGHT: u16 = 12; // Title, header, and all ten models.
const MONTHLY_MIN_WIDTH: u16 = 66;
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

/// Use the combined height of both monthly charts so TOP 10 never needs a
/// second column or gets clipped to five entries. Short/narrow panes retain
/// the existing full-width charts rather than squeezing unreadable content.
pub(super) fn monthly_areas(areas: &[Rect]) -> Option<([Rect; 2], Rect)> {
    let tokens = *areas.get(1)?;
    let cost = *areas.get(2)?;
    if tokens.width < MONTHLY_MIN_WIDTH
        || tokens.height < super::MIN_CHART_HEIGHT
        || cost.height < super::MIN_CHART_HEIGHT
        || tokens.x != cost.x
        || tokens.width != cost.width
        || tokens.bottom() != cost.y
    {
        return None;
    }
    let height = tokens.height.saturating_add(cost.height);
    if height < RANKING_MIN_HEIGHT {
        return None;
    }
    let left_width = (tokens.width - PANEL_GAP) / 2;
    let right = Rect::new(
        tokens.x + left_width + PANEL_GAP,
        tokens.y,
        tokens.width - left_width - PANEL_GAP,
        height,
    );
    Some((
        [
            Rect::new(tokens.x, tokens.y, left_width, tokens.height),
            Rect::new(cost.x, cost.y, left_width, cost.height),
        ],
        right,
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
    // Two outer cells and at least one cell in each of the three gaps.
    let fixed = 3 + cost_width + token_width + 2 + 3;
    if width < fixed + columns("模型") {
        return vec![Line::styled(
            truncate(" 窗口过窄", width),
            Style::default().fg(MUTED),
        )];
    }
    let widths = [
        3,
        (width - fixed).min(MONTH_MODEL_MAX_BYTES),
        cost_width,
        token_width,
    ];
    let free = width - 2 - widths.iter().sum::<usize>();
    let gaps = [
        free / 3 + usize::from(!free.is_multiple_of(3)),
        free / 3 + usize::from(free % 3 > 1),
        free / 3,
    ];
    let row = |cells: [(String, Color); 4]| {
        let mut spans = vec![Span::raw(" ")];
        for (index, (text, color)) in cells.into_iter().enumerate() {
            let padding = " ".repeat(widths[index].saturating_sub(columns(&text)));
            let text = if index == 1 {
                format!("{text}{padding}")
            } else {
                format!("{padding}{text}")
            };
            spans.push(Span::styled(text, Style::default().fg(color)));
            if let Some(gap) = gaps.get(index) {
                spans.push(Span::raw(" ".repeat(*gap)));
            }
        }
        spans.push(Span::raw(" "));
        Line::from(spans)
    };
    let mut result = vec![row(
        ["#", "模型", "金额", "Token"].map(|s| (s.into(), MUTED))
    )];
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
    if area.width < 20 || area.height < RANKING_MIN_HEIGHT {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::styled(
            truncate(" 本月模型 · Token TOP 10", area.width as usize),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        )),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let table = ranking_table(models, area.width as usize);
    frame.render_widget(
        Paragraph::new(table[0].clone()),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    let body = Rect::new(area.x, area.y + 2, area.width, area.height - 2);
    let count = table.len() - 1;
    if models.is_empty() {
        frame.render_widget(
            Paragraph::new("暂无本月用量记录")
                .style(Style::default().fg(MUTED))
                .centered(),
            Rect::new(body.x, body.y + body.height / 2, body.width, 1),
        );
        return;
    }
    // Center each row in an equal-height band. Integer-cell rounding can only
    // change adjacent row gaps by one cell, including on terminal resize.
    for (index, line) in table.into_iter().skip(1).enumerate() {
        let y = body.y + ((2 * index + 1) * body.height as usize / (2 * count)) as u16;
        frame.render_widget(Paragraph::new(line), Rect::new(body.x, y, body.width, 1));
    }
}
