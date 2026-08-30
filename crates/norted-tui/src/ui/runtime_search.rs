use norted_core::{RuntimeCompatibility, RuntimeOperationPhase, RuntimeSourceBuildSystem};
use ratatui::Frame;
use ratatui::layout::Position;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Overlay, RuntimeSearchFocus};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{KEY_COLUMN, format_bytes, key_value, popup_block};
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
    frame.render_widget(popup_block(&title, theme, glyphs), area);

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
        Line::from(vec![
            Span::styled("Search  ", theme.accent),
            Span::styled(&app.runtime_search_query, query_style),
        ])
    };
    frame.render_widget(Paragraph::new(query), layout.runtime_search_input);

    let submit_style = if app.hover == Some(HoverTarget::RuntimeSearchSubmit) {
        theme.hovered
    } else {
        theme.accent
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if app.runtime_search_loading {
                "Searching…"
            } else {
                "[Enter] Go"
            },
            submit_style,
        ))),
        layout.runtime_search_submit,
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
        let detail = if app.runtime_search_query.is_empty() {
            "No compatible runtime candidates were returned by the configured providers."
        } else {
            "No fetched runtime matches this filter. Press Enter to search providers with it."
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

    let items = layout.runtime_search_rows.iter().map(|(index, _)| {
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
        let mut style = if app.selected_runtime_search_result == Some(*index) {
            theme.selected
        } else {
            ratatui::style::Style::default()
        };
        if app.hover == Some(HoverTarget::RuntimeSearchResult(*index)) {
            style = style.patch(theme.hovered);
        }
        ListItem::new(vec![
            Line::from(vec![
                Span::styled(format!("{marker}  "), marker_style),
                Span::styled(&available.display_name, theme.text),
                Span::styled(format!("  {compatibility}"), compatibility_style),
            ]),
            Line::from(vec![
                Span::styled(
                    format!(
                        "{}  {} / {}  {} · {}",
                        available.identity.version,
                        available.identity.accelerator,
                        available.identity.variant,
                        formats,
                        acquisition,
                    ),
                    theme.muted,
                ),
                Span::styled(
                    if result.installed { "  installed" } else { "" },
                    theme.success,
                ),
            ]),
        ])
        .style(style)
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
    let target = format!("{} / {}", identity.platform, identity.architecture);
    let backend = format!("{} / {}", identity.accelerator, identity.variant);
    let (acquisition, size) = match available.download_size_bytes() {
        Some(bytes) => ("verified release download", format_bytes(bytes)),
        None => ("managed source build", "local build".to_owned()),
    };
    let selected_for = result.selected_for.join(", ");
    let mut lines = vec![
        Line::from(Span::styled(&available.display_name, theme.text)),
        key_value("ENGINE", &identity.engine_id, theme),
        key_value("VERSION", &identity.version, theme),
        key_value("TARGET", &target, theme),
        key_value("BACKEND", &backend, theme),
        key_value("FORMATS", &formats, theme),
        key_value("ACQUIRE", acquisition, theme),
        key_value("SIZE", &size, theme),
        key_value("PROVIDER", provider, theme),
        key_value("SOURCE", source, theme),
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
            RuntimeSourceBuildSystem::Cmake => format!(
                "CMake >= {}, CUDA >= {}, Ninja, C++20, pkg-config",
                source_build.prerequisites.minimum_cmake_version,
                source_build.prerequisites.minimum_cuda_version
            ),
            RuntimeSourceBuildSystem::Make => format!(
                "Make, CUDA >= {} via {}, {} with C++{}",
                source_build.prerequisites.minimum_cuda_version,
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
    let (label, style) = match selected {
        Some(result) if result.installed => ("Installed", theme.success),
        Some(result)
            if matches!(
                result.entry.compatibility,
                RuntimeCompatibility::Incompatible(_)
            ) =>
        {
            ("Incompatible", theme.error)
        }
        Some(_) if app.runtime_mutation_busy() => ("Installing…", theme.warning),
        Some(_) => ("[Enter/i] Install", theme.accent),
        None => ("Select a runtime", theme.hint),
    };
    let style = if app.hover == Some(HoverTarget::RuntimeInstall) {
        style.patch(theme.hovered)
    } else {
        style
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(label, style))),
        layout.runtime_install_action,
    );
    if let Some(progress) = &app.runtime_operation {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                progress_text(progress),
                phase_style(progress.phase, theme),
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
                "Tab focus  Up/Down or j/k select  F5 refresh  Esc close",
                theme.hint,
            ))),
            layout.runtime_operation_status,
        );
    }
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
    let byte_index = app
        .runtime_search_query
        .char_indices()
        .nth(app.runtime_search_cursor)
        .map(|(index, _)| index)
        .unwrap_or(app.runtime_search_query.len());
    let prefix_width = UnicodeWidthStr::width(&app.runtime_search_query[..byte_index]) as u16;
    let cursor_x = layout
        .runtime_search_input
        .x
        .saturating_add(8)
        .saturating_add(prefix_width)
        .min(layout.runtime_search_input.right().saturating_sub(1));
    frame.set_cursor_position(Position::new(cursor_x, layout.runtime_search_input.y));
}
