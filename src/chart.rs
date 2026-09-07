//! Small, fixed-window charts. `height` includes the plot and axes; accessible
//! legends and the current/minimum/maximum readouts are laid out beneath it.
//! Focus or click a plot to inspect its history with Left/Right and Home/End.
//! Chart IDs must be unique across all charts rendered in the same context.

use std::hash::{DefaultHasher, Hash, Hasher};

use eframe::egui::{
    self, Align2, Color32, FontId, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui,
};

use crate::theme;

#[derive(Clone, Debug)]
pub struct Series {
    pub name: String,
    pub color: Color32,
    /// Collection run, changed when history is reset even if the clock continues.
    pub history_generation: u64,
    /// Monotonic timestamp at x = 0, shared by every series in this chart.
    pub time_origin: f64,
    /// Seconds relative to now, from -60 to 0. A non-finite y breaks the line.
    pub points: Vec<[f64; 2]>,
    pub dashed: bool,
}

#[derive(Clone, Copy)]
struct HistoryCursor {
    generation: u64,
    selected_at: f64,
    latest_at: f64,
}

pub fn show(
    ui: &mut Ui,
    id: impl Hash,
    series: &[Series],
    height: f32,
    fixed_max: Option<f64>,
    unit: &str,
) {
    let name = series
        .iter()
        .map(|series| series.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    show_named(ui, id, &name, series, height, fixed_max, unit);
}

/// Give the history control a descriptive accessible name while keeping the
/// series names short in its visible legends and historical value readouts.
pub fn show_named(
    ui: &mut Ui,
    id: impl Hash,
    name: &str,
    series: &[Series],
    height: f32,
    fixed_max: Option<f64>,
    unit: &str,
) {
    let mut id_hash = DefaultHasher::new();
    id.hash(&mut id_hash);
    // Responsive layouts move charts between parents. An explicit scope keeps
    // the plot's single interaction, keyboard focus, and cursor IDs unchanged.
    let chart_id = egui::Id::new(("loadpeek_chart", id_hash.finish()));
    ui.scope_builder(egui::UiBuilder::new().id(chart_id), |ui| {
        let width = ui.available_width().max(100.0);
        let (rect, mut response) =
            ui.allocate_exact_size(egui::vec2(width, height.max(78.0)), Sense::click());
        let painter = ui.painter();
        let font = FontId::proportional(12.0);
        let (lower, upper) = bounds(series, fixed_max);
        let axis_labels: Vec<_> = [upper, (lower + upper) / 2.0, lower]
            .into_iter()
            .map(|value| format_value(value, unit))
            .collect();
        let axis_width = axis_labels
            .iter()
            .map(|label| {
                painter
                    .layout_no_wrap(label.clone(), font.clone(), theme::SUBTEXT)
                    .size()
                    .x
            })
            .fold(28.0_f32, f32::max)
            + 12.0;
        let plot = Rect::from_min_max(
            rect.min + egui::vec2(axis_width.min(width * 0.42), 8.0),
            rect.max - egui::vec2(5.0, 24.0),
        );
        let selected_time = inspect_history(ui, &mut response, name, series, plot, unit);
        let painter = ui.painter();
        painter.rect_filled(plot.expand(5.0), 5.0, theme::BASE);
        if response.has_focus() {
            painter.rect_stroke(
                rect.shrink(1.0),
                5.0,
                Stroke::new(2.0, theme::MAUVE),
                egui::StrokeKind::Inside,
            );
        }

        for (index, label) in axis_labels.iter().enumerate() {
            let y = egui::lerp(plot.top()..=plot.bottom(), index as f32 / 2.0);
            painter.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                Stroke::new(1.0, theme::SURFACE1),
            );
            painter.text(
                egui::pos2(plot.left() - 10.0, y),
                Align2::RIGHT_CENTER,
                label,
                font.clone(),
                theme::SUBTEXT,
            );
        }
        for (fraction, label, anchor) in [
            (0.0, "−60s", Align2::LEFT_TOP),
            (0.5, "−30s", Align2::CENTER_TOP),
            (1.0, "now", Align2::RIGHT_TOP),
        ] {
            painter.text(
                egui::pos2(
                    egui::lerp(plot.left()..=plot.right(), fraction),
                    plot.bottom() + 8.0,
                ),
                anchor,
                label,
                font.clone(),
                theme::SUBTEXT,
            );
        }

        let chart_painter = painter.with_clip_rect(plot.expand(3.0).intersect(ui.clip_rect()));
        let screen = |point: [f64; 2]| {
            egui::pos2(
                plot.left() + ((point[0] + 60.0) / 60.0) as f32 * plot.width(),
                plot.bottom() - ((point[1] - lower) / (upper - lower)) as f32 * plot.height(),
            )
        };
        for data in series {
            let mut path = Vec::new();
            for &point in &data.points {
                if valid_point(point) {
                    path.push(screen(point));
                } else {
                    draw_path(&chart_painter, &path, data.color, data.dashed);
                    path.clear();
                }
            }
            draw_path(&chart_painter, &path, data.color, data.dashed);
            if let Some(point) = latest_point(data).filter(|point| point[1].is_finite()) {
                chart_painter.circle_filled(screen(point), 3.0, data.color);
            }
        }

        let has_data = series
            .iter()
            .any(|data| data.points.iter().any(|&point| valid_point(point)));
        if !has_data {
            // A real widget preserves this state in the accessibility tree.
            let mut overlay = ui.new_child(egui::UiBuilder::new().max_rect(plot).layout(
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            ));
            overlay.add(
                egui::Label::new(
                    RichText::new("Collecting samples · unavailable values remain blank")
                        .size(13.0)
                        .color(theme::SUBTEXT),
                )
                .wrap(),
            );
        }

        let hover_time = response
            .hover_pos()
            .filter(|point| plot.contains(*point))
            .map(|pointer| f64::from((pointer.x - plot.left()) / plot.width()) * 60.0 - 60.0);
        let inspect_time = if response.has_focus() {
            selected_time
        } else {
            hover_time
        };
        if let Some(seconds) = inspect_time {
            let cursor_x = screen([seconds, lower]).x;
            chart_painter.line_segment(
                [
                    egui::pos2(cursor_x, plot.top()),
                    egui::pos2(cursor_x, plot.bottom()),
                ],
                Stroke::new(1.0, theme::SUBTEXT),
            );
            if response.has_focus() {
                egui::Tooltip::for_widget(&response)
                    .show(|ui| history_tooltip(ui, series, seconds, unit, true));
            } else {
                response
                    .on_hover_ui_at_pointer(|ui| history_tooltip(ui, series, seconds, unit, false));
            }
        }

        for data in series {
            let current = latest_point(data)
                .map(|point| format_value(point[1], unit))
                .unwrap_or_else(|| "Unavailable".to_owned());
            let min_max = data
                .points
                .iter()
                .filter(|&&point| valid_point(point))
                .map(|point| point[1])
                .fold(None::<(f64, f64)>, |range, value| {
                    Some(match range {
                        Some((minimum, maximum)) => (minimum.min(value), maximum.max(value)),
                        None => (value, value),
                    })
                });
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (key, _) = ui.allocate_exact_size(egui::vec2(22.0, 15.0), Sense::hover());
                draw_path(
                    ui.painter(),
                    &[key.left_center(), key.right_center()],
                    data.color,
                    data.dashed,
                );
                ui.label(
                    RichText::new(format!("{}: {current}", data.name))
                        .size(13.0)
                        .color(theme::TEXT),
                );
                if let Some((minimum, maximum)) = min_max {
                    ui.label(
                        RichText::new(format!(
                            "min {} · max {}",
                            format_value(minimum, unit),
                            format_value(maximum, unit)
                        ))
                        .size(12.0)
                        .color(theme::SUBTEXT),
                    );
                }
            });
        }
    });
}

/// Expose the plot as a time-cursor slider, including assistive-technology
/// increment/decrement and set-value actions. This adds no visual layout row.
fn inspect_history(
    ui: &mut Ui,
    response: &mut egui::Response,
    name: &str,
    series: &[Series],
    plot: Rect,
    unit: &str,
) -> Option<f64> {
    use egui::{
        Key, Modifiers,
        accesskit::{Action, ActionData},
    };

    let mut times: Vec<_> = series
        .iter()
        .flat_map(|series| &series.points)
        .filter(|point| in_window(point[0]))
        .map(|point| point[0])
        .collect();
    times.sort_by(f64::total_cmp);
    times.dedup_by(|a, b| (*a - *b).abs() < 0.000_001);
    let last = times.len().saturating_sub(1);
    let time_origin = series.first().map_or(0.0, |series| series.time_origin);
    let generation = series.first().map_or(0, |series| series.history_generation);
    let state_id = response.id.with("history_cursor");
    let cursor = ui.data(|data| data.get_temp::<HistoryCursor>(state_id));
    // Retain the observation, rather than its moving position in the deque.
    // An expired observation falls back to the nearest remaining boundary;
    // a restarted history or monotonic timeline begins at its newest observation.
    let mut index = cursor
        .filter(|cursor| cursor.generation == generation && time_origin >= cursor.latest_at)
        .and_then(|cursor| {
            let seconds = cursor.selected_at - time_origin;
            times
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| (*a - seconds).abs().total_cmp(&(*b - seconds).abs()))
                .map(|(index, _)| index)
        })
        .unwrap_or(last);
    let old_index = index;
    let mut cursor_action = false;
    if response.clicked() {
        response.request_focus();
        if let Some(pointer) = response
            .interact_pointer_pos()
            .filter(|point| plot.contains(*point))
        {
            let seconds = f64::from((pointer.x - plot.left()) / plot.width()) * 60.0 - 60.0;
            index = times
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| (*a - seconds).abs().total_cmp(&(*b - seconds).abs()))
                .map_or(0, |(index, _)| index);
        }
    }
    if response.gained_focus() {
        response.scroll_to_me(None);
    }
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    ..Default::default()
                },
            )
        });
        ui.input_mut(|input| {
            if input.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                index = index.saturating_sub(1);
            }
            if input.consume_key(Modifiers::NONE, Key::ArrowRight) {
                index = index.saturating_add(1).min(last);
            }
            if input.consume_key(Modifiers::NONE, Key::Home) {
                index = 0;
            }
            if input.consume_key(Modifiers::NONE, Key::End) {
                index = last;
            }
            if input.consume_key(Modifiers::NONE, Key::Escape) {
                response.surrender_focus();
            }
        });
    }
    ui.input_mut(|input| {
        input.consume_accesskit_action_requests(response.id, |request| {
            match request.action {
                Action::Decrement => index = index.saturating_sub(1),
                Action::Increment => index = index.saturating_add(1).min(last),
                Action::SetValue => {
                    if let Some(ActionData::NumericValue(value)) = request.data
                        && value.is_finite()
                    {
                        index = (value.round().max(0.0) as usize).min(last);
                    }
                }
                _ => return false,
            }
            cursor_action = true;
            true
        });
    });
    if index != old_index {
        response.mark_changed();
    }
    let selected_time = times.get(index).copied();
    if let Some(seconds) = selected_time {
        if response.has_focus() || cursor_action || cursor.is_some() {
            ui.data_mut(|data| {
                data.insert_temp(
                    state_id,
                    HistoryCursor {
                        generation,
                        selected_at: time_origin + seconds,
                        latest_at: time_origin,
                    },
                );
            });
        }
    } else {
        // Resume clears history before collecting its new baseline. Do not
        // carry a cursor from that previous run into the new observations.
        ui.data_mut(|data| data.remove::<HistoryCursor>(state_id));
    }
    let label = format!(
        "{name}, 60-second history. Left and Right inspect samples; Home selects oldest; End selects newest; Escape leaves the chart."
    );
    let value = selected_time
        .map(|seconds| {
            format!(
                "{:.1} seconds ago. {}",
                (-seconds).max(0.0),
                sample_labels(series, seconds, unit).join(". ")
            )
        })
        .unwrap_or_else(|| "No samples available".to_owned());
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::slider(ui.is_enabled(), index as f64, &label);
        info.current_text_value = Some(value.clone());
        info
    });
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_min_numeric_value(0.0);
        node.set_max_numeric_value(last as f64);
        node.set_numeric_value_step(1.0);
        if !times.is_empty() {
            node.add_action(Action::SetValue);
        }
        if index > 0 {
            node.add_action(Action::Decrement);
        }
        if index < last {
            node.add_action(Action::Increment);
        }
    });
    selected_time
}

fn sample_labels(series: &[Series], seconds: f64, unit: &str) -> Vec<String> {
    series
        .iter()
        .map(|data| {
            // Include missing samples when choosing the nearest time so a gap never
            // misleadingly reports a distant available value.
            let nearest = data
                .points
                .iter()
                .filter(|point| in_window(point[0]))
                .min_by(|a, b| (a[0] - seconds).abs().total_cmp(&(b[0] - seconds).abs()));
            let value = nearest
                .map(|point| format_value(point[1], unit))
                .unwrap_or_else(|| "Unavailable".to_owned());
            let age = nearest
                .map(|point| format!(" ({:.1}s ago)", (-point[0]).max(0.0)))
                .unwrap_or_default();
            format!("{}: {value}{age}", data.name)
        })
        .collect()
}

fn history_tooltip(ui: &mut Ui, series: &[Series], seconds: f64, unit: &str, keyboard: bool) {
    ui.label(RichText::new(format!("{:.1} seconds ago", (-seconds).max(0.0))).strong());
    for label in sample_labels(series, seconds, unit) {
        ui.label(label);
    }
    let hint = if keyboard {
        "Left / Right: sample   Home / End: oldest / newest   Esc: close"
    } else {
        "Click or Tab to inspect history with the keyboard"
    };
    ui.label(RichText::new(hint).small().color(theme::SUBTEXT));
}

fn in_window(seconds: f64) -> bool {
    seconds.is_finite() && (-60.0..=0.001).contains(&seconds)
}

fn valid_point(point: [f64; 2]) -> bool {
    in_window(point[0]) && point[1].is_finite()
}

fn latest_point(series: &Series) -> Option<[f64; 2]> {
    series
        .points
        .iter()
        .filter(|point| in_window(point[0]))
        .max_by(|a, b| a[0].total_cmp(&b[0]))
        .copied()
}

fn draw_path(painter: &Painter, path: &[Pos2], color: Color32, dashed: bool) {
    if path.len() >= 2 {
        let stroke = Stroke::new(2.0, color);
        if dashed {
            painter.extend(Shape::dashed_line(path, stroke, 6.0, 4.0));
        } else {
            painter.add(Shape::line(path.to_vec(), stroke));
        }
    } else if let Some(point) = path.first() {
        painter.circle_filled(*point, 2.5, color);
    }
}

fn bounds(series: &[Series], fixed_max: Option<f64>) -> (f64, f64) {
    let (mut minimum, mut maximum) = (0.0_f64, 0.0_f64);
    for point in series
        .iter()
        .flat_map(|series| &series.points)
        .filter(|&&point| valid_point(point))
    {
        minimum = minimum.min(point[1]);
        maximum = maximum.max(point[1]);
    }
    let lower = if minimum < 0.0 {
        -nice_upper(-minimum * 1.05)
    } else {
        0.0
    };
    let upper = fixed_max
        .filter(|max| max.is_finite() && *max > 0.0)
        .unwrap_or_else(|| nice_upper(maximum * 1.1));
    (lower, upper)
}

fn nice_upper(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    let magnitude = 10.0_f64.powf(value.log10().floor());
    let fraction = value / magnitude;
    let step = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    (step * magnitude).max(f64::MIN_POSITIVE)
}

pub fn format_value(value: f64, unit: &str) -> String {
    if !value.is_finite() {
        return "Unavailable".to_owned();
    }
    match unit {
        "B/s" | "bytes/s" => format_bytes(value, true),
        "B" | "bytes" => format_bytes(value, false),
        "%" => format!("{value:.1}%"),
        "MHz" if value.abs() >= 1000.0 => format!("{:.2} GHz", value / 1000.0),
        "MHz" => format!("{value:.0} MHz"),
        "GHz" => format!("{value:.2} GHz"),
        "°C" => format!("{value:.1} °C"),
        "" => format!("{value:.1}"),
        _ => format!("{value:.1} {unit}"),
    }
}

fn format_bytes(value: f64, per_second: bool) -> String {
    let mut scaled = value;
    let mut index = 0;
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    while scaled.abs() >= 1024.0 && index < units.len() - 1 {
        scaled /= 1024.0;
        index += 1;
    }
    let suffix = if per_second { "/s" } else { "" };
    if index == 0 {
        format!("{scaled:.0} {}{suffix}", units[index])
    } else {
        format!("{scaled:.1} {}{suffix}", units[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(points: Vec<[f64; 2]>) -> Series {
        Series {
            name: "Test".to_owned(),
            color: theme::BLUE,
            history_generation: 0,
            time_origin: 0.0,
            points,
            dashed: false,
        }
    }

    #[test]
    fn scale_ignores_missing_and_expired_samples() {
        let series = sample(vec![
            [-70.0, 10000.0],
            [-40.0, f64::NAN],
            [-20.0, 8.0],
            [0.0, 12.0],
        ]);
        assert_eq!(bounds(&[series], None), (0.0, 20.0));
        assert_eq!(bounds(&[], None), (0.0, 1.0));
        assert_eq!(bounds(&[sample(vec![[0.0, 0.0]])], None), (0.0, 1.0));
    }

    #[test]
    fn a_missing_latest_sample_is_not_reported_as_an_old_value() {
        let series = sample(vec![[-5.0, 42.0], [0.0, f64::NAN]]);
        assert!(latest_point(&series).unwrap()[1].is_nan());
        assert_eq!(
            format_value(latest_point(&series).unwrap()[1], "°C"),
            "Unavailable"
        );
    }

    #[test]
    fn formatter_preserves_units_and_binary_byte_scaling() {
        assert_eq!(format_value(1536.0, "B/s"), "1.5 KiB/s");
        assert_eq!(format_value(1073741824.0, "bytes"), "1.0 GiB");
        assert_eq!(format_value(3400.0, "MHz"), "3.40 GHz");
        assert_eq!(format_value(0.0, "B/s"), "0 B/s");
        assert_eq!(format_value(f64::NAN, "%"), "Unavailable");
    }

    #[test]
    fn named_charts_identify_each_control_and_preserve_series_readouts() {
        use egui::accesskit::{Action, ActionData, ActionRequest, Role, TreeId};

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let series = [Series {
            name: "Use".into(),
            ..sample(vec![[-10.0, 12.0], [0.0, 42.0]])
        }];
        let frame = |events| {
            ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    for core in [0, 1] {
                        show_named(
                            ui,
                            ("core", core),
                            &format!("Core {core} utilization"),
                            &series,
                            78.0,
                            Some(100.0),
                            "%",
                        );
                    }
                },
            )
        };

        let output = frame(Vec::new());
        let nodes = &output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes;
        let sliders: Vec<_> = nodes
            .iter()
            .filter(|(_, node)| node.role() == Role::Slider)
            .collect();
        assert_eq!(sliders.len(), 2);
        for core in [0, 1] {
            let prefix = format!("Core {core} utilization, 60-second history.");
            assert!(
                sliders
                    .iter()
                    .any(|(_, node)| node.label().is_some_and(|label| label.starts_with(&prefix)))
            );
        }
        assert_eq!(
            nodes
                .iter()
                .filter(|(_, node)| node.role() == Role::Label && node.value() == Some("Use: 42.0%"))
                .count(),
            2
        );
        let events = sliders
            .iter()
            .map(|(id, _)| {
                egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::SetValue,
                    target_tree: TreeId::ROOT,
                    target_node: *id,
                    data: Some(ActionData::NumericValue(0.0)),
                })
            })
            .collect();
        output.drop_without_applying_deltas();

        let output = frame(events);
        let sliders: Vec<_> = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == Role::Slider)
            .collect();
        assert_eq!(sliders.len(), 2);
        for (_, node) in sliders {
            assert_eq!(node.numeric_value(), Some(0.0));
            assert_eq!(
                node.value(),
                Some("10.0 seconds ago. Use: 12.0% (10.0s ago)")
            );
        }
        output.drop_without_applying_deltas();
    }

    #[test]
    fn history_controls_keep_focus_and_independent_cursors_when_reparented() {
        use egui::accesskit::{Action, ActionData, ActionRequest, Role, TreeId};

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let series = [sample(vec![[-10.0, 12.0], [-5.0, 24.0], [0.0, 42.0]])];
        let frame = |reparented, events| {
            let output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.push_id(if reparented { "stacked" } else { "columns" }, |ui| {
                        if reparented {
                            ui.label("Additional content before the charts");
                        }
                        for name in ["First", "Second"] {
                            show_named(ui, name, name, &series, 78.0, Some(100.0), "%");
                        }
                    });
                },
            );
            let mut controls: Vec<_> = output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .filter(|(_, node)| node.role() == Role::Slider)
                .map(|(id, node)| {
                    (
                        node.label().unwrap().to_owned(),
                        *id,
                        node.numeric_value().unwrap(),
                        node.value().unwrap().to_owned(),
                    )
                })
                .collect();
            controls.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(controls.len(), 2);
            assert_ne!(controls[0].1, controls[1].1);
            output.drop_without_applying_deltas();
            controls
        };

        let initial = frame(false, Vec::new());
        let action = |action, data| {
            egui::Event::AccessKitActionRequest(ActionRequest {
                action,
                target_tree: TreeId::ROOT,
                target_node: initial[0].1,
                data,
            })
        };
        let selected = frame(
            false,
            vec![
                action(Action::Focus, None),
                action(Action::SetValue, Some(ActionData::NumericValue(0.0))),
            ],
        );
        assert_eq!(selected[0].2, 0.0);
        assert_eq!(selected[1].2, 2.0);
        let focused = ctx.memory(|memory| memory.focused());
        assert!(focused.is_some());

        let reparented = frame(true, Vec::new());
        assert_eq!(reparented, selected);
        assert_eq!(ctx.memory(|memory| memory.focused()), focused);
        let moved = frame(
            true,
            vec![egui::Event::Key {
                key: egui::Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(moved[0].2, 1.0);
        assert!(moved[0].3.contains("Test: 24.0%"));
        assert_eq!(moved[1], selected[1]);
        assert_eq!(ctx.memory(|memory| memory.focused()), focused);
    }

    #[test]
    fn tab_navigation_scrolls_focused_charts_into_view() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let series = [sample(vec![[-10.0, 12.0], [0.0, 42.0]])];
        let mut frame_number = 0;
        let mut frame = |tab: Option<egui::Modifiers>| {
            let events = tab
                .into_iter()
                .flat_map(|modifiers| {
                    [true, false].map(|pressed| egui::Event::Key {
                        key: egui::Key::Tab,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    })
                })
                .collect();
            let mut viewport = Rect::NOTHING;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 200.0))),
                    time: Some(f64::from(frame_number) / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    viewport = egui::ScrollArea::vertical()
                        .id_salt("chart_navigation")
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for index in 0..5 {
                                show(ui, index, &series, 78.0, Some(100.0), "%");
                            }
                        })
                        .inner_rect;
                },
            )
            .drop_without_applying_deltas();
            frame_number += 1;
            viewport
        };

        frame(None);
        let mut visited = Vec::new();
        // Visit every chart, including those initially below the viewport, then
        // return to the first. Allow normal scroll animation to settle each time.
        for (step, modifiers) in [egui::Modifiers::NONE; 5]
            .into_iter()
            .chain([egui::Modifiers::SHIFT; 4])
            .enumerate()
        {
            frame(Some(modifiers));
            let mut viewport = Rect::NOTHING;
            for _ in 0..60 {
                viewport = frame(None);
            }
            let focused = ctx.memory(|memory| memory.focused()).unwrap();
            if step < 5 {
                assert!(!visited.contains(&focused));
                visited.push(focused);
            } else {
                assert_eq!(focused, visited[8 - step]);
            }
            let chart = ctx.read_response(focused).unwrap().rect;
            assert!(
                chart.top() >= viewport.top() - 1.0 && chart.bottom() <= viewport.bottom() + 1.0,
                "focused chart {chart:?} is outside scroll viewport {viewport:?}"
            );
        }
    }

    fn inspection_frame(
        ctx: &egui::Context,
        series: &[Series],
        key: Option<egui::Key>,
        focus: bool,
    ) -> (Option<f64>, bool, String) {
        let events = key
            .map(|key| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .into_iter()
            .collect();
        let mut selected = None;
        let mut focused = false;
        let output = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| {
                let (rect, mut response) =
                    ui.allocate_exact_size(egui::vec2(300.0, 78.0), Sense::click());
                if focus {
                    response.request_focus();
                }
                selected = inspect_history(ui, &mut response, "Test", series, rect, "%");
                focused = response.has_focus();
            },
        );
        let value = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| node.role() == egui::accesskit::Role::Slider)
            .and_then(|(_, node)| node.value())
            .unwrap()
            .to_owned();
        output.drop_without_applying_deltas();
        (selected, focused, value)
    }

    #[test]
    fn keyboard_inspection_reports_historical_values_and_can_leave_focus() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let series = [sample(vec![[-10.0, 12.0], [-5.0, f64::NAN], [0.0, 42.0]])];
        assert_eq!(inspection_frame(&ctx, &series, None, true).0, Some(0.0));
        // Install egui's arrow-key focus filter on the following frame.
        inspection_frame(&ctx, &series, None, false);
        let (selected, focused, accessible_value) =
            inspection_frame(&ctx, &series, Some(egui::Key::ArrowLeft), false);
        assert_eq!(selected, Some(-5.0));
        assert!(focused);
        assert!(accessible_value.contains("Unavailable"));
        let (selected, _, value) = inspection_frame(&ctx, &series, Some(egui::Key::Home), false);
        assert_eq!(selected, Some(-10.0));
        assert!(value.contains("12.0%"));
        assert_eq!(
            inspection_frame(&ctx, &series, Some(egui::Key::End), false).0,
            Some(0.0)
        );
        assert!(!inspection_frame(&ctx, &series, Some(egui::Key::Escape), false).1);
    }

    fn cpu_history_series(history: &crate::history::History) -> [Series; 1] {
        [Series {
            time_origin: history.latest_time(),
            ..sample(history.series(|snapshot| snapshot.cpu_percent))
        }]
    }

    fn push_cpu(history: &mut crate::history::History, at: f64, value: Option<f64>) {
        history.push(
            at,
            crate::metrics::Snapshot {
                cpu_percent: value,
                ..Default::default()
            },
        );
    }

    #[test]
    fn inspection_retains_observations_through_startup_trimming_and_interval_changes() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut history = crate::history::History::default();
        for at in 0..=58 {
            push_cpu(&mut history, f64::from(at), Some(f64::from(at)));
        }
        inspection_frame(&ctx, &cpu_history_series(&history), None, true);
        inspection_frame(&ctx, &cpu_history_series(&history), None, false);
        let (selected, _, value) = inspection_frame(
            &ctx,
            &cpu_history_series(&history),
            Some(egui::Key::ArrowLeft),
            false,
        );
        assert_eq!(selected, Some(-1.0));
        assert!(value.contains("Test: 57.0%"));

        // The original t=57 observation survives growth, deque trimming, and
        // fractional boundary points caused by changing the polling interval.
        for at in [59.0, 60.0, 61.0, 61.5, 66.5] {
            push_cpu(&mut history, at, Some(at));
            let (selected, _, value) =
                inspection_frame(&ctx, &cpu_history_series(&history), None, false);
            assert_eq!(selected, Some(57.0 - at));
            assert!(value.contains("Test: 57.0%"), "{value}");
        }

        // Once the observation expires, inspect the oldest remaining point.
        push_cpu(&mut history, 118.0, Some(118.0));
        let (selected, _, value) =
            inspection_frame(&ctx, &cpu_history_series(&history), None, false);
        assert_eq!(selected, Some(-60.0));
        assert!(value.contains("Test: 58.0%"));
    }

    #[test]
    fn inspection_retains_gaps_and_resets_when_history_is_cleared() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut history = crate::history::History::default();
        for at in 0..=60 {
            push_cpu(
                &mut history,
                f64::from(at),
                (at != 59).then_some(f64::from(at)),
            );
        }
        inspection_frame(&ctx, &cpu_history_series(&history), None, true);
        inspection_frame(&ctx, &cpu_history_series(&history), None, false);
        inspection_frame(
            &ctx,
            &cpu_history_series(&history),
            Some(egui::Key::ArrowLeft),
            false,
        );
        push_cpu(&mut history, 61.0, Some(61.0));
        let (selected, _, value) =
            inspection_frame(&ctx, &cpu_history_series(&history), None, false);
        assert_eq!(selected, Some(-2.0));
        assert!(value.contains("Unavailable"));

        history = crate::history::History::default();
        let (selected, _, value) =
            inspection_frame(&ctx, &cpu_history_series(&history), None, false);
        assert_eq!(selected, None);
        assert_eq!(value, "No samples available");
        push_cpu(&mut history, 80.0, Some(80.0));
        push_cpu(&mut history, 81.0, Some(81.0));
        let (selected, _, value) =
            inspection_frame(&ctx, &cpu_history_series(&history), None, false);
        assert_eq!(selected, Some(0.0));
        assert!(value.contains("Test: 81.0%"));

        // A replacement history with a restarted clock is safe even if its
        // empty frame was never shown (for example, while on another page).
        history = crate::history::History::default();
        push_cpu(&mut history, 1.0, Some(1.0));
        push_cpu(&mut history, 2.0, Some(2.0));
        let (selected, _, value) =
            inspection_frame(&ctx, &cpu_history_series(&history), None, false);
        assert_eq!(selected, Some(0.0));
        assert!(value.contains("Test: 2.0%"));
    }

    #[test]
    fn inspection_resets_hidden_history_on_a_new_collection_generation() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let series = [Series {
            time_origin: 20.0,
            ..sample(vec![[-10.0, 12.0], [-5.0, 24.0], [0.0, 42.0]])
        }];
        inspection_frame(&ctx, &series, None, true);
        inspection_frame(&ctx, &series, None, false);
        let (selected, focused, _) = inspection_frame(&ctx, &series, Some(egui::Key::Home), false);
        assert_eq!(selected, Some(-10.0));
        assert!(focused);

        // A chart on another page never observes the empty history at resume.
        // The worker's monotonic clock continues across the new baseline.
        let resumed = [Series {
            history_generation: 1,
            time_origin: 32.0,
            ..sample(vec![[-2.0, f64::NAN], [-1.0, 50.0], [0.0, 55.0]])
        }];
        let (selected, focused, value) = inspection_frame(&ctx, &resumed, None, false);
        assert_eq!(selected, Some(0.0));
        assert!(focused);
        assert!(value.contains("Test: 55.0%"));

        // New navigation in this generation still retains its observation.
        let (selected, _, _) = inspection_frame(&ctx, &resumed, Some(egui::Key::ArrowLeft), false);
        assert_eq!(selected, Some(-1.0));
        let advanced = [Series {
            history_generation: 1,
            time_origin: 33.0,
            ..sample(vec![
                [-3.0, f64::NAN],
                [-2.0, 50.0],
                [-1.0, 55.0],
                [0.0, 60.0],
            ])
        }];
        assert_eq!(inspection_frame(&ctx, &advanced, None, false).0, Some(-2.0));
    }
}
