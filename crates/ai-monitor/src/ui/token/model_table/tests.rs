use super::*;
use ratatui::{Terminal, backend::TestBackend, layout::Rect, widgets::Paragraph};

fn sample_models() -> Vec<ModelUsage> {
    [
        ("deepseek/deepseek-v4.1-flash", 1_700_000_000, 9_945, 5_600_000, 3_200_000, Some(19.38)),
        ("meta/muse-spark-1.3-contributor", 929_100_000, 9_834, 1_300_000, 683_000, Some(3.62)),
        ("openai/gpt-6-astra", 252_500_000, 9_799, 879_000, 464_000, Some(42.0)),
        ("z-ai/glm-5.3-flash", 76_800_000, 9_245, 648_000, 296_000, Some(3.44)),
        ("muse-spark-1.3-contributor-free", 65_000_000, 9_540, 145_000, 63_000, None),
        ("google/gemini-3.8-flash", 18_700_000, 9_506, 72_000, 38_000, Some(2.30)),
    ]
    .into_iter()
    .map(|(name, input, basis_points, output, reasoning, cost)| ModelUsage {
        model: name.into(),
        input_total: input,
        cache_read: input / 10_000 * basis_points,
        output,
        reasoning,
        cost,
    })
    .collect()
}

fn text(lines: &[Line<'_>]) -> String {
    lines.iter().map(Line::to_string).collect::<Vec<_>>().join("\n")
}

fn right_edge(row: &str, value: &str) -> usize {
    let start = row.find(value).unwrap_or_else(|| panic!("{value} missing in {row}"));
    columns(&row[..start]) + columns(value)
}

#[test]
fn names_have_no_provider_and_at_most_25_characters() {
    assert_eq!(model_name("openai/gpt-6-astra", 100), "gpt-6-astra");
    assert_eq!(model_name("gateway/vendor/gpt-6-astra", 100), "gpt-6-astra");
    let long = "muse-spark-1.3-contributor-free";
    assert_eq!(model_name(long, 100), long.chars().take(25).collect::<String>());
    assert_eq!(model_name(&format!("provider/{}", "é".repeat(30)), 100), "é".repeat(25));
    for name in ["供应商/深度求索模型名称测试-very-long-model-name", "vendor/🦀🦀-long-model-name-that-overflows"] {
        for width in 0..=100 {
            let display = model_name(name, width);
            assert!(!display.contains('/'));
            assert!(display.chars().count() <= 25);
            assert!(columns(&display) <= width.min(25));
        }
    }
}

#[test]
fn columns_follow_the_requested_order_and_input_keeps_exact_hit_semantics() {
    let models = sample_models();
    let rows = model_table(&models, 100, 6);
    let header = rows[0].to_string();
    let positions: Vec<_> = ["模型", "COST", "合计", "IN(缓存)", "OUT", "THINK"]
        .into_iter().map(|label| header.find(label).unwrap()).collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(text(&rows).contains("1.7B(99.45%)"));
    assert!(text(&rows).contains("929.1M(98.34%)"));
    assert!(!text(&rows).contains("缓存命中"));
    assert!(!text(&rows).contains("deepseek/"));
    for column in [Column::Cost, Column::Total, Column::InputHit, Column::Output, Column::Think] {
        let edge = right_edge(&header, column.label());
        for (row, model) in rows[1..].iter().zip(&models) {
            assert_eq!(right_edge(&row.to_string(), &column.cell(model).0), edge);
        }
    }
    assert_eq!(rows[1].spans.iter().find(|span| span.content == "(99.45%)").unwrap().style.fg, Some(MUTED));
}

#[test]
fn zero_unknown_and_unpriced_values_do_not_turn_into_fake_zero_costs_or_rates() {
    let mut models = vec![ModelUsage { model: "vendor/zero".into(), ..ModelUsage::default() }];
    let rendered = text(&model_table(&models, 100, 6));
    assert!(rendered.contains("0(—)"));
    assert!(!rendered.contains("0.00%"));
    assert!(!rendered.contains("$0.00"));
    models[0].cost = Some(0.0);
    assert!(text(&model_table(&models, 100, 6)).contains("$0.00"));
}

#[test]
fn narrowing_hides_details_before_squeezing_names_and_never_changes_row_count() {
    let models = sample_models();
    let full = model_table(&models, 100, 6);
    assert!(full[0].to_string().contains("THINK"));
    let compact = model_table(&models, 64, 6);
    let header = compact[0].to_string();
    assert!(!header.contains("THINK") && !header.contains("缓存"));
    assert!(header.contains("IN") && header.contains("OUT"));
    let narrow = model_table(&models, 46, 6);
    let header = narrow[0].to_string();
    assert!(header.contains("COST") && header.contains("合计"));
    assert!(!header.contains("IN") && !header.contains("OUT"));
    for width in 1..=240 {
        let rows = model_table(&models, width, 6);
        assert_eq!(rows.len(), 7, "width={width}");
        assert!(rows.iter().all(|row| row.width() <= width), "width={width}: {}", text(&rows));
        if width >= 25 {
            for (row, model) in rows[1..].iter().zip(&models) {
                assert!(row.to_string().contains(&Column::Cost.cell(model).0));
                assert!(row.to_string().contains(&Column::Total.cell(model).0));
            }
        }
    }
}

#[test]
fn oversized_amounts_are_preserved_whole_or_the_column_is_hidden() {
    let models = vec![ModelUsage {
        model: "provider/large".into(), input_total: u64::MAX,
        cache_read: u64::MAX, output: u64::MAX, reasoning: u64::MAX,
        cost: Some(123_456_789.0),
    }];
    for width in 1..=240 {
        let rows = model_table(&models, width, 6);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.width() <= width));
        let header = rows[0].to_string();
        let row = rows[1].to_string();
        for column in [Column::Cost, Column::Total, Column::Output, Column::Think] {
            if header.contains(column.label()) {
                assert!(row.contains(&column.cell(&models[0]).0));
            }
        }
    }
}

#[test]
fn display_name_collisions_do_not_merge_models_or_reorder_totals() {
    let models = vec![
        ModelUsage { model: "one/same-model".into(), input_total: 100, ..ModelUsage::default() },
        ModelUsage { model: "two/same-model".into(), input_total: 200, ..ModelUsage::default() },
    ];
    let before = models.clone();
    let rows = model_table(&models, 100, 6);
    assert_eq!(rows.len(), 3);
    assert_eq!(text(&rows).matches("same-model").count(), 2);
    assert_eq!(models, before);
    assert!(model_table(&models, 0, 6).is_empty());
    assert_eq!(model_table(&models, 100, 0).len(), 1);
    assert_eq!(model_table(&[], 100, 6).len(), 1);
    assert_eq!(model_table(&sample_models(), 100, 3).len(), 4);
}

#[test]
fn rendering_respects_the_existing_rectangle_at_every_size() {
    for width in [8u16, 16, 25, 46, 64, 81, 100, 160] {
        let area = Rect::new(3, 2, width, 7);
        let mut terminal = Terminal::new(TestBackend::new(width + 6, 12)).unwrap();
        terminal.draw(|frame| {
            for y in 0..12 {
                for x in 0..width + 6 {
                    frame.buffer_mut()[(x, y)].set_symbol("!");
                }
            }
            frame.render_widget(Paragraph::new(model_table(&sample_models(), width as usize, 6)), area);
        }).unwrap();
        let buffer = terminal.backend().buffer();
        for y in 0..12 {
            for x in 0..width + 6 {
                if x < area.x || x >= area.right() || y < area.y || y >= area.bottom() {
                    assert_eq!(buffer[(x, y)].symbol(), "!");
                }
            }
        }
    }
}

#[test]
fn export_real_model_table_terminal_fixtures() {
    use ratatui::{backend::{Backend, CrosstermBackend}, buffer::Buffer};
    use std::{fs, path::PathBuf};

    let temp = tempfile::tempdir().unwrap();
    let output = std::env::var_os("MODEL_PREVIEW_DIR").map(PathBuf::from)
        .unwrap_or_else(|| temp.path().to_owned());
    fs::create_dir_all(&output).unwrap();
    // Synthetic fixtures resembling the requested screenshot, not account data.
    let models = sample_models();
    for width in [100u16, 81, 64, 46] {
        let height = 8;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| {
            let mut lines = vec![Line::styled(" 模型用量 · 最近24小时",
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD))];
            lines.extend(model_table(&models, width as usize, 6));
            frame.render_widget(Paragraph::new(lines), frame.area());
        }).unwrap();
        let buffer = terminal.backend().buffer();
        let blank = Buffer::empty(buffer.area);
        let mut ansi = Vec::new();
        {
            let mut backend = CrosstermBackend::new(&mut ansi);
            backend.clear().unwrap();
            backend.hide_cursor().unwrap();
            backend.draw(blank.diff(buffer).into_iter()).unwrap();
            Backend::flush(&mut backend).unwrap();
        }
        let name = format!("models-{width}x{height}-sample");
        fs::write(output.join(format!("{name}.ansi")), ansi).unwrap();
        let text = (0..height).map(|y| {
            (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>()
        }).collect::<Vec<_>>().join("\n");
        fs::write(output.join(format!("{name}.txt")), text).unwrap();
    }
}
