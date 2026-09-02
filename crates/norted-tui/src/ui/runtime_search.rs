use norted_core::{RuntimeCompatibility, RuntimeOperationPhase, RuntimeSourceBuildSystem};
use ratatui::Frame;
use ratatui::layout::Position;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Paragraph, Wrap};

use crate::app::{App, Overlay, RuntimeSearchFocus};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{
    ActionState, KEY_COLUMN, action_style, format_bytes, input_window, key_value, key_value_width,
    marquee_text, popup_block, remaining_width, truncate_middle,
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

    let items = layout.runtime_search_rows.iter().map(|(index, row)| {
        let result = &search.results[*index];
        let available = &result.entry.available;
        let (compatibility, compatibility_style) =
            compatibility_label(&result.entry.compatibility, theme);
        let marker = if result.installed {
            glyphs.running
        } else {
            glyphs.empty
        };
        let marker_style = if result.installed {
            theme.success
        } else {
            theme.muted
        };
        let mut style = if app.selected_runtime_search_result == Some(*index) {
            theme.selected
        } else {
            ratatui::style::Style::default()
        };
        if app.hover == Some(HoverTarget::RuntimeSearchResult(*index)) {
            style = style.patch(theme.hovered);
        }
        let active_row = app.selected_runtime_search_result == Some(*index)
            || app.hover == Some(HoverTarget::RuntimeSearchResult(*index));
        let name_width = result_name_width(row.width, marker, compatibility);
        let display_name = if active_row {
            marquee_text(
                &available.display_name,
                name_width,
                app.marquee_animation_frame / 3,
            )
        } else {
            truncate_middle(&available.display_name, name_width, glyphs.ellipsis)
        };
        let metadata = result_metadata_text(available);
        let installed = if result.installed { "  installed" } else { "" };
        let metadata_width = result_metadata_width(row.width, installed);
        let metadata = if active_row {
            marquee_text(&metadata, metadata_width, app.marquee_animation_frame / 3)
        } else {
            truncate_middle(&metadata, metadata_width, glyphs.ellipsis)
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled(format!("{marker}  "), marker_style),
                Span::styled(display_name, theme.text),
                Span::styled(format!("  {compatibility}"), compatibility_style),
            ]),
            Line::from(vec![
                Span::styled(metadata, theme.muted),
                Span::styled(installed, theme.success),
            ]),
        ];
        if layout.overlay_row_height > 2 {
            lines.push(Line::default());
        }
        ListItem::new(lines).style(style)
    });
    frame.render_widget(List::new(items), layout.runtime_search_results);
}

fn render_details(frame: &mut Frame<'_>, app: &App, theme: &Theme, layout: &UiLayout) {
    let Some(result) = app
        .selected_runtime_search_result
        .and_then(|index| app.runtime_search.as_ref()?.results.get(index))
    else {
        let mut lines = vec![
            Line::from(Span::styled("Details", theme.hint)),
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
    let display_name = marquee_text(
        &available.display_name,
        layout.runtime_search_details.width as usize,
        app.marquee_animation_frame / 3,
    );
    let target = format!("{} / {}", identity.platform, identity.architecture);
    let backend = format!("{} / {}", identity.accelerator, identity.variant);
    let (acquisition, size) = match available.download_size_bytes() {
        Some(bytes) => ("verified release download", format_bytes(bytes)),
        None => ("managed source build", "local build".to_owned()),
    };
    let selected_for = result.selected_for.join(", ");
    let mut lines = vec![
        Line::from(Span::styled(display_name, theme.text)),
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
            Paragraph::new(Line::from(Span::styled(
                "Tab focus  Space toggle  Up/Down or j/k select  F5 refresh",
                theme.hint,
            ))),
            layout.runtime_operation_status,
        );
    }
}

pub(super) fn result_name_width(row_width: u16, marker: &str, compatibility: &str) -> usize {
    let prefix = format!("{marker}  ");
    let suffix = format!("  {compatibility}");
    remaining_width(row_width, &[&prefix, &suffix])
}

pub(super) fn result_metadata_width(row_width: u16, installed: &str) -> usize {
    remaining_width(row_width, &[installed])
}

pub(super) fn result_metadata_text(available: &norted_core::AvailableRuntime) -> String {
    let formats = available
        .supported_formats
        .iter()
        .map(|format| format.as_str().to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join("/");
    let acquisition = if available.source_build().is_some() {
        "source build"
    } else {
        "upstream binary"
    };
    format!(
        "{}  {} / {}  {} · {}",
        available.identity.version,
        available.identity.accelerator,
        available.identity.variant,
        formats,
        acquisition,
    )
}

pub(crate) fn progress_text(progress: &norted_core::RuntimeOperationProgress) -> String {
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
