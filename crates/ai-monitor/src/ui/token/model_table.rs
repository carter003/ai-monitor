//! Rolling-24H table. Names and numeric slots are bounded; extra space belongs
//! to the gaps, never to an unbounded model-name column. No accounting here.

use super::{CYAN, GREEN, INK, MUTED, ModelUsage, columns, compact, money, truncate};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

#[cfg(test)]
mod tests;

const NAME_MAX: usize = 25;
const MIN_GAP: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Cost,
    Total,
    InputHit,
    Input,
    Output,
    Think,
}

impl Column {
    fn label(self) -> &'static str {
        match self {
            Self::Cost => "COST",
            Self::Total => "合计",
            Self::InputHit => "IN(缓存)",
            Self::Input => "IN",
            Self::Output => "OUT",
            Self::Think => "THINK",
        }
    }

    fn cell(self, model: &ModelUsage) -> (String, Color) {
        match self {
            Self::Cost => match model.cost {
                Some(value) => (money(value), GREEN),
                None => ("—".into(), MUTED),
            },
            Self::Total => (compact(model.total_tokens()), INK),
            Self::InputHit => (model.input_display(), INK),
            Self::Input => (compact(model.input_total), INK),
            Self::Output => (compact(model.output), INK),
            Self::Think => (compact(model.reasoning), MUTED),
        }
    }

    fn width(self, models: &[ModelUsage]) -> usize {
        // Stable slots for ordinary values. Exceptional amounts may grow a
        // slot, but must never be silently clipped or spill into its neighbour.
        let minimum = match self {
            Self::Cost => 8,
            Self::InputHit => 15,
            _ => 7,
        };
        models
            .iter()
            .map(|model| columns(&self.cell(model).0))
            .max()
            .unwrap_or(0)
            .max(columns(self.label()))
            .max(minimum)
    }
}

fn model_name(model: &str, width: usize) -> String {
    // Match the monthly ranking's provider removal, but use the requested
    // 25-character cap here rather than that panel's separate 15-byte cap.
    let basename = model.rsplit('/').next().unwrap_or(model);
    let name: String = basename.chars().take(NAME_MAX).collect();
    truncate(&name, width.min(NAME_MAX))
}

pub(super) fn model_table(models: &[ModelUsage], width: usize, limit: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![];
    }
    if models.is_empty() || limit == 0 {
        return vec![Line::styled(
            truncate(" 暂无用量记录", width),
            Style::default().fg(MUTED),
        )];
    }
    let taken = &models[..models.len().min(limit)];
    let padding = usize::from(width >= 6);
    let inner = width - 2 * padding;
    let mut slots: Vec<_> = [
        Column::Cost,
        Column::Total,
        Column::InputHit,
        Column::Output,
        Column::Think,
    ]
    .into_iter()
    .map(|column| (column, column.width(taken)))
    .collect();
    let required = |slots: &[(Column, usize)]| {
        slots.iter().map(|(_, width)| width).sum::<usize>() + MIN_GAP * slots.len()
    };

    // Preserve the 25-cell name slot before squeezing it: drop THINK, the
    // cache suffix, OUT, then IN. COST and total stay together on narrow panes.
    for optional in [Column::Think, Column::InputHit, Column::Output, Column::Input] {
        if NAME_MAX + required(&slots) <= inner {
            break;
        }
        if optional == Column::InputHit {
            if let Some(slot) = slots.iter_mut().find(|(column, _)| *column == optional) {
                *slot = (Column::Input, Column::Input.width(taken));
            }
        } else {
            slots.retain(|(column, _)| *column != optional);
        }
    }
    // At extreme widths hide whole remaining numeric columns, never part of
    // an amount. Keep one header and one line per model at every nonzero width.
    let name_min = columns("模型").min(inner);
    while !slots.is_empty() && name_min + required(&slots) > inner {
        slots.pop();
    }
    let name_width = inner.saturating_sub(required(&slots)).min(NAME_MAX);
    let free = inner - name_width - slots.iter().map(|(_, width)| width).sum::<usize>();
    let gap = free.checked_div(slots.len()).unwrap_or(0);
    let remainder = free.checked_rem(slots.len()).unwrap_or(0);
    let header_style = Style::default().fg(MUTED).add_modifier(Modifier::BOLD);

    let row = |model: Option<&ModelUsage>| {
        let name = model.map_or_else(
            || truncate("模型", name_width),
            |model| model_name(&model.model, name_width),
        );
        let used = columns(&name);
        let mut spans = vec![
            Span::raw(" ".repeat(padding)),
            Span::styled(
                name,
                if model.is_some() {
                    Style::default().fg(CYAN)
                } else {
                    header_style
                },
            ),
            Span::raw(" ".repeat(name_width.saturating_sub(used))),
        ];
        for (index, (column, cell_width)) in slots.iter().enumerate() {
            let (value, color) = model.map_or_else(
                || (column.label().to_owned(), MUTED),
                |model| column.cell(model),
            );
            spans.push(Span::raw(" ".repeat(gap + usize::from(index < remainder))));
            spans.push(Span::raw(
                " ".repeat(cell_width.saturating_sub(columns(&value))),
            ));
            let style = if model.is_none() {
                header_style
            } else if *column == Column::Total {
                Style::default().fg(color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color)
            };
            // Keep the input amount prominent; the adjacent cache percentage
            // is still the existing exact, truncated two-decimal hit ratio.
            if model.is_some() && *column == Column::InputHit {
                if let Some((amount, suffix)) = value.split_once('(') {
                    spans.push(Span::styled(amount.to_owned(), style));
                    spans.push(Span::styled(
                        format!("({suffix}"),
                        Style::default().fg(MUTED),
                    ));
                } else {
                    spans.push(Span::styled(value, style));
                }
            } else {
                spans.push(Span::styled(value, style));
            }
        }
        spans.push(Span::raw(" ".repeat(padding)));
        Line::from(spans)
    };
    let mut result = Vec::with_capacity(taken.len() + 1);
    result.push(row(None));
    result.extend(taken.iter().map(|model| row(Some(model))));
    result
}
