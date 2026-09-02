use ratatui::Frame;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Paragraph, Wrap};

use crate::app::{App, Overlay};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{
    ActionState, KEY_COLUMN, action_style, key_value, marquee_text, popup_block, remaining_width,
    truncate_middle,
};
use crate::ui::layout::{HoverTarget, UiLayout};
use crate::ui::screens::compatibility_label;

pub fn render(frame: &mut Frame<'_>, app: &App, theme: &Theme, glyphs: &Glyphs, layout: &UiLayout) {
    if app.overlay != Some(Overlay::ModelRuntime) {
        return;
    }
    let Some(area) = layout.runtime_search_popup else {
        return;
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        popup_block(" Model runtime override ", theme, glyphs, layout.compact),
        area,
    );

    let model = app
        .selected_model
        .and_then(|index| app.snapshot.models.get(index));
    let heading = model.map_or_else(
        || Line::from(Span::styled("Model unavailable", theme.error)),
        |model| {
            let package = model
                .norted_package
                .as_ref()
                .map_or_else(|| "Raw".to_owned(), |package| package.kind.to_string());
            let format = model.format.as_str().to_ascii_uppercase();
            let name_width = heading_name_width(layout.runtime_search_input.width, model);
            let display_name = marquee_text(
                &model.display_name,
                name_width,
                app.marquee_animation_frame / 3,
            );
            Line::from(vec![
                Span::styled("Model  ", theme.hint),
                Span::styled(display_name, theme.text),
                Span::styled(format!("  {format}"), theme.accent),
                Span::styled(format!("  {package}"), theme.hint),
            ])
        },
    );
    frame.render_widget(Paragraph::new(heading), layout.runtime_search_input);

    let Some(snapshot) = &app.runtime_list else {
        return;
    };
    let has_candidates = !app.runtime_picker_indices().is_empty();
    if has_candidates {
        let items = layout.runtime_search_rows.iter().map(|(index, row)| {
            let status = &snapshot.installed[*index];
            let manifest = &status.runtime.manifest;
            let identity = &manifest.identity;
            let model_compatibility = app
                .runtime_picker_compatibility(&manifest.runtime_id)
                .unwrap_or(&status.compatibility);
            let (compatibility, compatibility_style) =
                compatibility_label(model_compatibility, theme);
            let is_override = model.is_some_and(|model| {
                snapshot.selections.model_overrides.get(&model.id) == Some(&manifest.runtime_id)
            });
            let mut style = if app.runtime_picker_selection == Some(*index) {
                theme.selected
            } else {
                ratatui::style::Style::default()
            };
            if app.hover == Some(HoverTarget::RuntimePickerResult(*index)) {
                style = style.patch(theme.hovered);
            }
            let active_row = app.runtime_picker_selection == Some(*index)
                || app.hover == Some(HoverTarget::RuntimePickerResult(*index));
            let identity_text = format!("{}  {}", identity.engine_id, identity.version);
            let identity_width = picker_identity_width(row.width, compatibility);
            let identity_text = if active_row {
                marquee_text(
                    &identity_text,
                    identity_width,
                    app.marquee_animation_frame / 3,
                )
            } else {
                truncate_middle(&identity_text, identity_width, glyphs.ellipsis)
            };
            let override_status = if is_override {
                "  current override"
            } else {
                ""
            };
            let metadata = format!("{} / {}", identity.accelerator, identity.variant);
            let metadata_width = picker_metadata_width(row.width, override_status);
            let metadata = if active_row {
                marquee_text(&metadata, metadata_width, app.marquee_animation_frame / 3)
            } else {
                truncate_middle(&metadata, metadata_width, glyphs.ellipsis)
            };
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(identity_text, theme.text),
                    Span::styled(format!("  {compatibility}"), compatibility_style),
                ]),
                Line::from(vec![
                    Span::styled(metadata, theme.muted),
                    Span::styled(override_status, theme.success),
                ]),
            ];
            if layout.overlay_row_height > 2 {
                lines.push(Line::default());
            }
            ListItem::new(lines).style(style)
        });
        frame.render_widget(List::new(items), layout.runtime_search_results);
    } else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No compatible runtime is installed for this model",
                    theme.warning,
                )),
                Line::from(Span::styled(
                    app.runtime_picker_error.as_deref().unwrap_or(
                        "The installed runtime set has no usable candidate for this format.",
                    ),
                    theme.muted,
                )),
            ])
            .wrap(Wrap { trim: true }),
            layout.runtime_search_results,
        );
        let format = model
            .map(|model| model.format.as_str().to_ascii_uppercase())
            .unwrap_or_else(|| "MODEL".to_owned());
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled("Recommended next step", theme.hint)),
                Line::from(Span::styled(
                    format!(
                        "Search upstream {format} runtimes compatible with this host."
                    ),
                    theme.text,
                )),
                Line::from(Span::styled(
                    "Search is requested only when you activate the action below; installation remains explicit.",
                    theme.muted,
                )),
            ])
            .wrap(Wrap { trim: true }),
            layout.runtime_search_details,
        );
    }

    if let Some(status) = has_candidates
        .then_some(app.runtime_picker_selection)
        .flatten()
        .and_then(|index| snapshot.installed.get(index))
    {
        let manifest = &status.runtime.manifest;
        let identity = &manifest.identity;
        let formats = manifest
            .supported_formats
            .iter()
            .map(|format| format.as_str().to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(", ");
        let target = format!("{} / {}", identity.platform, identity.architecture);
        let backend = format!("{} / {}", identity.accelerator, identity.variant);
        let model_compatibility = app
            .runtime_picker_compatibility(&manifest.runtime_id)
            .unwrap_or(&status.compatibility);
        let (compatibility, compatibility_style) = compatibility_label(model_compatibility, theme);
        let mut lines = vec![
            Line::from(Span::styled("Selected runtime", theme.hint)),
            key_value("ENGINE", &identity.engine_id, theme),
            key_value("VERSION", &identity.version, theme),
            key_value("TARGET", &target, theme),
            key_value("BACKEND", &backend, theme),
            key_value("FORMATS", &formats, theme),
            Line::from(vec![
                Span::styled(format!("{:<KEY_COLUMN$} ", "FIT"), theme.hint),
                Span::styled(compatibility, compatibility_style),
            ]),
        ];
        if let norted_core::RuntimeCompatibility::NeedsAttention(reason)
        | norted_core::RuntimeCompatibility::Incompatible(reason) = model_compatibility
        {
            lines.push(Line::from(Span::styled(reason, compatibility_style)));
        }
        lines.extend(
            manifest
                .requirements
                .advisories
                .iter()
                .map(|note| Line::from(Span::styled(note, theme.muted))),
        );
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }),
            layout.runtime_search_details,
        );
    }

    let format_label = model
        .map(|model| model.format.as_str().to_ascii_uppercase())
        .unwrap_or_else(|| "MODEL".to_owned());
    let action_label = if app.runtime_mutation_busy() {
        "Saving…".to_owned()
    } else if !has_candidates {
        format!("[ Search {format_label} ]")
    } else {
        "[ Use selected ]".to_owned()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            action_label,
            action_style(
                theme,
                if app.runtime_mutation_busy() {
                    ActionState::Disabled
                } else {
                    ActionState::Primary
                },
                app.hover == Some(HoverTarget::RuntimePickerApply),
            ),
        ))),
        layout.runtime_install_action,
    );
    if app.selected_model_has_runtime_override() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if layout.compact {
                    "[ Clear ]"
                } else {
                    "[ Clear Override ]"
                },
                action_style(
                    theme,
                    if app.runtime_mutation_busy() {
                        ActionState::Disabled
                    } else {
                        ActionState::Normal
                    },
                    app.hover == Some(HoverTarget::RuntimePickerClear),
                ),
            ))),
            layout.runtime_picker_clear_action,
        );
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "[ Cancel ]",
            action_style(
                theme,
                ActionState::Normal,
                app.hover == Some(HoverTarget::RuntimeOverlayCancel),
            ),
        ))),
        layout.runtime_overlay_cancel,
    );
    let shortcut_hint = if app.selected_model_has_runtime_override() {
        if layout.compact {
            "Use actions; Enter/x are shortcuts."
        } else {
            "Select a runtime, then use the actions below. Enter/x remain shortcuts."
        }
    } else if layout.compact {
        "Use actions below; Enter is a shortcut."
    } else {
        "Select a runtime, then use the actions below. Enter remains a shortcut."
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(shortcut_hint, theme.hint))),
        layout.runtime_operation_status,
    );
}

pub(super) fn heading_name_width(total_width: u16, model: &norted_core::ModelArtifact) -> usize {
    let package = model
        .norted_package
        .as_ref()
        .map_or_else(|| "Raw".to_owned(), |package| package.kind.to_string());
    let format = model.format.as_str().to_ascii_uppercase();
    let format_span = format!("  {format}");
    let package_span = format!("  {package}");
    remaining_width(total_width, &["Model  ", &format_span, &package_span])
}

pub(super) fn picker_identity_width(row_width: u16, compatibility: &str) -> usize {
    let suffix = format!("  {compatibility}");
    remaining_width(row_width, &[&suffix])
}

pub(super) fn picker_metadata_width(row_width: u16, override_status: &str) -> usize {
    remaining_width(row_width, &[override_status])
}
