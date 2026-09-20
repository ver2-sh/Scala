use ratatui::Frame;
use ratatui::layout::Position;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use scala_core::{RuntimeCompatibility, RuntimeOperationPhase, RuntimeSourceBuildSystem};

use crate::app::{App, Overlay, RuntimeSearchFocus};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{
    ActionState, KEY_COLUMN, action_style, format_bytes, input_window, inventory_columns,
    inventory_row, key_value, key_value_width, marquee_text, popup_block,
};
use crate::ui::layout::{HoverTarget, UiLayout};
use crate::ui::screens::compatibility_label;

pub fn render(frame: &mut Frame<'_>, app: &App, theme: &Theme, glyphs: &Glyphs, layout: &UiLayout) {
    if app.overlay != Some(Overlay::RuntimeSearch) {
        return;
    }
    let Some(area) = layout.runtime_search_popup else {
        return;
    };
    let result_count = app.runtime_search_indices().len();
    let title = if let Some(context) = &app.runtime_search_context {
        if app.runtime_search_loading {
            format!(" Runtime search - {context} - contacting providers ")
        } else {
            format!(" Runtime search - {context} ")
        }
    } else if app.runtime_search_loading {
        " Runtime search - contacting providers ".to_owned()
    } else {
        " Runtime search ".to_owned()
    };
    frame.render_widget(Clear, area);
    frame.render_widget(popup_block(&title, theme, glyphs, layout.compact), area);

    let query_style = if app.runtime_search_focus == RuntimeSearchFocus::Query {
        theme.focused
    } else if app.hover == Some(HoverTarget::RuntimeSearchInput) {
        theme.hovered
    } else {
        theme.text
    };
    let query = if app.runtime_search_query.is_empty() {
        Line::from(vec![
            Span::styled("Search  ", theme.accent),
            Span::styled(
                "filter by runtime, backend, accelerator, or format",
                theme.hint,
            ),
        ])
    } else {
        let window = input_window(
            &app.runtime_search_query,
            app.runtime_search_cursor,
            layout.runtime_search_input.width.saturating_sub(8) as usize,
        );
        Line::from(vec![
            Span::styled("Search  ", theme.accent),
            Span::styled(window.text, query_style),
        ])
    };
    frame.render_widget(Paragraph::new(query), layout.runtime_search_input);

    let submit_style = action_style(
        theme,
        if app.runtime_search_loading || app.runtime_mutation_busy() {
            ActionState::Disabled
        } else {
            ActionState::Primary
        },
        app.hover == Some(HoverTarget::RuntimeSearchSubmit),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if app.runtime_search_loading {
                "Searching…"
            } else {
                "[ Search ]"
            },
            submit_style,
        ))),
        layout.runtime_search_submit,
    );

    let toggle_style = if app.runtime_search_focus == RuntimeSearchFocus::IncompatibleToggle {
        theme.focused
    } else if app.hover == Some(HoverTarget::RuntimeSearchIncompatibleToggle) {
        theme.hovered
    } else {
        theme.text
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                "[{}] Show incompatible",
                if app.runtime_search_show_incompatible {
                    "x"
                } else {
                    " "
                }
            ),
            toggle_style,
        ))),
        layout.runtime_search_incompatible_toggle,
    );

    render_results(frame, app, theme, glyphs, layout, result_count);
    render_details(frame, app, theme, layout);
    render_action(frame, app, theme, layout);
    set_cursor(frame, app, layout);
}

fn render_results(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    layout: &UiLayout,
    result_count: usize,
) {
    let Some(search) = &app.runtime_search else {
        let (title, detail, style) = if let Some(error) = &app.runtime_search_error {
            ("Runtime search failed", error.as_str(), theme.error)
        } else {
            (
                "Search upstream runtimes",
                "Results are fetched only when this dialog is opened or Search is activated.",
                theme.text,
            )
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(title, style)),
                Line::from(Span::styled(detail, theme.muted)),
            ])
            .wrap(Wrap { trim: true }),
            layout.runtime_search_results,
        );
        return;
    };
    if result_count == 0 {
        let hidden = app.runtime_search_hidden_incompatible_count();
        let detail = if hidden > 0 {
            format!(
                "{hidden} incompatible result{} hidden. Enable Show incompatible to reveal {}.",
                if hidden == 1 { " is" } else { "s are" },
                if hidden == 1 { "it" } else { "them" }
            )
        } else if app.runtime_search_query.is_empty() {
            "No runtime candidates were returned by the configured providers.".to_owned()
        } else {
            "No fetched runtime matches this filter. Use Search to query providers; Enter remains a shortcut."
                .to_owned()
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("{}  No matching runtimes", glyphs.empty),
                    theme.text,
                )),
                Line::from(Span::styled(detail, theme.muted)),
            ])
            .wrap(Wrap { trim: true }),
            layout.runtime_search_results,
        );
        return;
    }

    let columns = inventory_columns(
        layout.runtime_search_results,
        &[12, 16, 12],
        if layout.runtime_search_results.width >= 60 {
            &[12, 16, 12]
        } else {
            &[12]
        },
    );
    let wide = layout.runtime_search_results.width >= 60;
    let headings = if wide {
        vec!["Runtime candidate", "Version", "Compatibility", "State"]
    } else {
        vec!["Runtime candidate", "Fit"]
    };
    inventory_row(
        frame,
        layout.runtime_search_header,
        &columns,
        &headings.into_iter().map(str::to_owned).collect::<Vec<_>>(),
        theme.hint,
        glyphs,
    );
    for (index, row) in &layout.runtime_search_rows {
        let result = &search.results[*index];
        let available = &result.entry.available;
        let selected = app.selected_runtime_search_result == Some(*index);
        let name = format!(
            "{} {}",
            if selected { ">" } else { " " },
            available.display_name
        );
        let compatibility = compatibility_label(&result.entry.compatibility, theme);
        let values = if wide {
            vec![
                name,
                available.identity.version.clone(),
                compatibility.0.to_owned(),
                if result.installed {
                    "Installed"
                } else {
                    "Available"
                }
                .to_owned(),
            ]
        } else {
            vec![name, compatibility.0.to_owned()]
        };
        let mut style = if selected {
            theme.selected
        } else {
            compatibility.1
        };
        if app.hover == Some(HoverTarget::RuntimeSearchResult(*index)) {
            style = style.patch(theme.hovered);
        }
        inventory_row(frame, *row, &columns, &values, style, glyphs);
    }
}

fn render_details(frame: &mut Frame<'_>, app: &App, theme: &Theme, layout: &UiLayout) {
    let Some(result) = app
        .selected_runtime_search_result
        .and_then(|index| app.runtime_search.as_ref()?.results.get(index))
    else {
        let mut lines = vec![
            Line::from(Span::styled("Details | D: full details", theme.hint)),
            Line::from(Span::styled(
                "Select a result to inspect its identity and compatibility.",
                theme.muted,
            )),
        ];
        if let Some(error) = &app.runtime_search_error {
            lines.push(Line::from(Span::styled(error, theme.error)));
        }
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }),
            layout.runtime_search_details,
        );
        return;
    };
    let available = &result.entry.available;
    let identity = &available.identity;
    let formats = available
        .supported_formats
        .iter()
        .map(|format| format.as_str().to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join(", ");
    let (compatibility, compatibility_style) =
        compatibility_label(&result.entry.compatibility, theme);
    let provider = &identity.package.provider_id;
    let source = identity
        .package
        .repository
        .as_deref()
        .unwrap_or(available.source_url.as_str());
    let detail_value_width = key_value_width(layout.runtime_search_details.width);
    let source = marquee_text(source, detail_value_width, app.marquee_animation_frame / 3);
    let target = format!("{} / {}", identity.platform, identity.architecture);
    let backend = format!("{} / {}", identity.accelerator, identity.variant);
    let (acquisition, size) = match available.download_size_bytes() {
        Some(bytes) => ("verified release download", format_bytes(bytes)),
        None => ("managed source build", "local build".to_owned()),
    };
    let selected_for = result.selected_for.join(", ");
    let mut lines = vec![
        Line::from(Span::styled(
            "Selected candidate | D: full details",
            theme.hint,
        )),
        key_value("ENGINE", &identity.engine_id, theme),
        key_value("VERSION", &identity.version, theme),
        key_value("TARGET", &target, theme),
        key_value("BACKEND", &backend, theme),
        key_value("FORMATS", &formats, theme),
        key_value("ACQUIRE", acquisition, theme),
        key_value("SIZE", &size, theme),
        key_value("PROVIDER", provider, theme),
        key_value("SOURCE", &source, theme),
        Line::from(vec![
            Span::styled(format!("{:<KEY_COLUMN$} ", "FIT"), theme.hint),
            Span::styled(compatibility, compatibility_style),
        ]),
    ];
    if let Some(source_build) = available.source_build() {
        lines.push(key_value(
            "REVISION",
            &source_build.source.commit_sha,
            theme,
        ));
        lines.push(key_value("GIT TREE", &source_build.source.tree_sha, theme));
        lines.push(key_value(
            "RECIPE",
            &source_build.recipe.recipe_version,
            theme,
        ));
        lines.push(key_value(
            "BUILD TARGET",
            &source_build.recipe.build_target,
            theme,
        ));
        let build_needs = match source_build.recipe.build_system {
            RuntimeSourceBuildSystem::Cmake => {
                let mut needs = vec![
                    format!(
                        "CMake >= {}",
                        source_build.prerequisites.minimum_cmake_version
                    ),
                    source_build
                        .prerequisites
                        .minimum_cuda_version
                        .as_deref()
                        .map_or_else(
                            || "CUDA toolkit/nvcc".to_owned(),
                            |version| {
                                source_build
                                    .prerequisites
                                    .maximum_cuda_version_exclusive
                                    .as_deref()
                                    .map_or_else(
                                        || format!("CUDA >= {version}"),
                                        |maximum| format!("CUDA >= {version}, < {maximum}"),
                                    )
                            },
                        ),
                ];
                if source_build.prerequisites.requires_ninja {
                    needs.push("Ninja".to_owned());
                }
                if let Some(standard) = source_build.prerequisites.minimum_cpp_standard {
                    needs.push(format!("C++{standard}"));
                } else if source_build.prerequisites.requires_cpp20_compiler {
                    needs.push("C++20".to_owned());
                }
                if source_build.prerequisites.requires_pkg_config {
                    needs.push("pkg-config".to_owned());
                }
                needs.join(", ")
            }
            RuntimeSourceBuildSystem::Make => format!(
                "Make, {} via {}, {} with C++{}",
                source_build
                    .prerequisites
                    .minimum_cuda_version
                    .as_deref()
                    .map_or_else(
                        || "CUDA toolkit".to_owned(),
                        |version| {
                            source_build
                                .prerequisites
                                .maximum_cuda_version_exclusive
                                .as_deref()
                                .map_or_else(
                                    || format!("CUDA >= {version}"),
                                    |maximum| format!("CUDA >= {version}, < {maximum}"),
                                )
                        },
                    ),
                source_build
                    .prerequisites
                    .cuda_compiler
                    .as_deref()
                    .map_or_else(|| "nvcc".to_owned(), |path| path.display().to_string()),
                source_build
                    .prerequisites
                    .cpp_compiler
                    .as_deref()
                    .unwrap_or("c++"),
                source_build
                    .prerequisites
                    .minimum_cpp_standard
                    .unwrap_or(17),
            ),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<KEY_COLUMN$} ", "BUILD NEEDS"), theme.hint),
            Span::styled(build_needs, theme.text),
        ]));
    }
    match &result.entry.compatibility {
        RuntimeCompatibility::NeedsAttention(reason)
        | RuntimeCompatibility::Incompatible(reason) => {
            lines.push(Line::from(Span::styled(reason, compatibility_style)));
        }
        RuntimeCompatibility::Recommended | RuntimeCompatibility::Compatible => {}
    }
    lines.extend(
        available
            .requirements
            .advisories
            .iter()
            .map(|note| Line::from(Span::styled(note, theme.muted))),
    );
    if !result.selected_for.is_empty() {
        lines.push(key_value("DEFAULT", &selected_for, theme));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }),
        layout.runtime_search_details,
    );
}

fn render_action(frame: &mut Frame<'_>, app: &App, theme: &Theme, layout: &UiLayout) {
    let selected = app
        .selected_runtime_search_result
        .and_then(|index| app.runtime_search.as_ref()?.results.get(index));
    let (label, state) = match selected {
        Some(result) if result.installed => ("Installed", ActionState::Disabled),
        Some(result)
            if matches!(
                result.entry.compatibility,
                RuntimeCompatibility::Incompatible(_)
            ) =>
        {
            ("Incompatible", ActionState::Disabled)
        }
        Some(_) if app.runtime_mutation_busy() => ("Installing…", ActionState::Disabled),
        Some(_) if app.runtime_search_loading => ("Searching…", ActionState::Disabled),
        Some(_) => ("[ Install ]", ActionState::Primary),
        None => ("Select a runtime", ActionState::Disabled),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            action_style(theme, state, app.hover == Some(HoverTarget::RuntimeInstall)),
        ))),
        layout.runtime_install_action,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "[ Close ]",
            action_style(
                theme,
                ActionState::Normal,
                app.hover == Some(HoverTarget::RuntimeOverlayCancel),
            ),
        ))),
        layout.runtime_overlay_cancel,
    );
    if let Some(progress) = &app.runtime_operation {
        let phase = progress.phase;
        let progress = progress_text(progress);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                marquee_text(
                    &progress,
                    layout.runtime_operation_status.width as usize,
                    app.marquee_animation_frame / 3,
                ),
                phase_style(phase, theme),
            ))),
            layout.runtime_operation_status,
        );
    } else if let Some(error_count) = app
        .runtime_search
        .as_ref()
        .map(|search| search.provider_errors.len())
        .filter(|count| *count > 0)
    {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{error_count} provider warning(s)"),
                theme.warning,
            ))),
            layout.runtime_operation_status,
        );
    } else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("D: details", theme.hint))),
            layout.runtime_operation_status,
        );
    }
}

pub(crate) fn progress_text(progress: &scala_core::RuntimeOperationProgress) -> String {
    let phase = match progress.phase {
        RuntimeOperationPhase::CheckingPrerequisites => "Checking prerequisites",
        RuntimeOperationPhase::FetchingSource => "Fetching source",
        RuntimeOperationPhase::VerifyingSource => "Verifying source",
        RuntimeOperationPhase::Configuring => "Configuring",
        RuntimeOperationPhase::Building => "Building",
        RuntimeOperationPhase::Downloading => "Downloading",
        RuntimeOperationPhase::Verifying => "Verifying",
        RuntimeOperationPhase::Extracting => "Extracting",
        RuntimeOperationPhase::Probing => "Probing",
        RuntimeOperationPhase::Installing => "Installing",
        RuntimeOperationPhase::Installed => "Installed",
        RuntimeOperationPhase::Failed => "Failed",
    };
    let bytes = match (progress.bytes_completed, progress.bytes_total) {
        (Some(completed), Some(total)) => {
            format!(" {}/{}", format_bytes(completed), format_bytes(total))
        }
        (Some(completed), None) => format!(" {}", format_bytes(completed)),
        _ => String::new(),
    };
    if progress.detail.is_empty() {
        format!("{phase}{bytes}")
    } else {
        format!("{phase}{bytes}: {}", progress.detail)
    }
}

fn phase_style(phase: RuntimeOperationPhase, theme: &Theme) -> ratatui::style::Style {
    match phase {
        RuntimeOperationPhase::Installed => theme.success,
        RuntimeOperationPhase::Failed => theme.error,
        RuntimeOperationPhase::Downloading
        | RuntimeOperationPhase::CheckingPrerequisites
        | RuntimeOperationPhase::FetchingSource
        | RuntimeOperationPhase::VerifyingSource
        | RuntimeOperationPhase::Configuring
        | RuntimeOperationPhase::Building
        | RuntimeOperationPhase::Verifying
        | RuntimeOperationPhase::Extracting
        | RuntimeOperationPhase::Probing
        | RuntimeOperationPhase::Installing => theme.warning,
    }
}

fn set_cursor(frame: &mut Frame<'_>, app: &App, layout: &UiLayout) {
    if app.runtime_search_focus != RuntimeSearchFocus::Query {
        return;
    }
    let window = input_window(
        &app.runtime_search_query,
        app.runtime_search_cursor,
        layout.runtime_search_input.width.saturating_sub(8) as usize,
    );
    let cursor_x = layout
        .runtime_search_input
        .x
        .saturating_add(8)
        .saturating_add(window.cursor_column)
        .min(layout.runtime_search_input.right().saturating_sub(1));
    frame.set_cursor_position(Position::new(cursor_x, layout.runtime_search_input.y));
}
