use norted_core::{
    ArtifactFormat, RegistryState, RuntimeCompatibility, RuntimeSourceBuildSystem,
    RuntimeUpdateState,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap};

use crate::app::{App, ModelLibraryView, Screen};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{
    ActionState, action_style, content_layout, format_bytes, key_value, load_progress_compact,
    render_empty, render_load_progress, section_title, truncate_middle,
};
use crate::ui::layout::{
    HoverTarget, InstalledModelAction, ModelProfileAction, SelectedRuntimeAction, UiLayout,
};
use crate::ui::runtime_search::progress_text;

pub fn render_screen(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    ui_layout: &UiLayout,
) {
    let area = ui_layout.content;
    match app.screen {
        Screen::Overview => render_overview(frame, area, app, theme, glyphs, ui_layout),
        Screen::Models => render_models(frame, area, app, theme, glyphs, ui_layout),
        Screen::ModelProfiles => render_model_profiles(frame, area, app, theme, ui_layout),
        Screen::Runtimes => render_runtimes(frame, area, app, theme, glyphs, ui_layout),
        Screen::Server => render_server(frame, area, app, theme, glyphs, ui_layout),
        Screen::Logs => render_logs(frame, area, app, theme, ui_layout),
        Screen::Settings => render_settings(frame, area, app, theme, ui_layout),
        Screen::Help => render_help_content(frame, area, theme, glyphs),
    }
}

fn render_overview(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    ui_layout: &UiLayout,
) {
    let title = content_layout(area)[0];
    frame.render_widget(
        section_title(
            "Overview",
            "Your local model runtime, from artifacts to API",
            theme,
        ),
        title,
    );
    render_metrics(
        frame,
        ui_layout.overview_metrics,
        app,
        theme,
        glyphs,
        ui_layout.compact,
    );
    if let Some(progress) = app.load_progress() {
        let compact_line = load_progress_compact(
            progress,
            app.load_animation_frame,
            ui_layout.overview_progress.width,
            glyphs,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(compact_line, theme.accent))),
            ui_layout.overview_progress,
        );
    }
    let body = match &app.snapshot.registry_state {
        RegistryState::NotScanned => vec![
            Line::from(Span::styled(
                format!("{}  Preparing model discovery", glyphs.transitional),
                theme.text,
            )),
            Line::default(),
            Line::from(Span::styled(
                "The interface is ready; configured directories have not been scanned yet.",
                theme.muted,
            )),
            Line::from(Span::styled(
                "Discovery will start in the background after this first frame.",
                theme.hint,
            )),
        ],
        RegistryState::Scanning => vec![
            Line::from(Span::styled(
                format!("{}  Discovering local models", glyphs.transitional),
                theme.text,
            )),
            Line::default(),
            Line::from(Span::styled(
                "The interface is ready while configured directories are scanned in the background.",
                theme.muted,
            )),
            Line::from(Span::styled(
                "Models will appear automatically when discovery completes.",
                theme.hint,
            )),
        ],
        RegistryState::Failed { message } => vec![
            Line::from(Span::styled("Model discovery failed", theme.error)),
            Line::default(),
            Line::from(Span::styled(message, theme.muted)),
            Line::from(Span::styled("Open Logs for details.", theme.hint)),
        ],
        RegistryState::Ready | RegistryState::ReadyWithWarnings { .. }
            if app.snapshot.models.is_empty() =>
        {
            vec![
                Line::from(Span::styled(
                    "No model artifacts discovered yet",
                    theme.text,
                )),
                Line::default(),
                Line::from(Span::styled(
                    if glyphs.unicode {
                        "Open Models → Discover to download from Hugging Face."
                    } else {
                        "Open Models, then Discover, to download from Hugging Face."
                    },
                    theme.accent,
                )),
                Line::from(Span::styled(
                    "Alternatively, configure external model paths for existing artifacts.",
                    theme.muted,
                )),
                Line::default(),
                Line::from(vec![
                    Span::styled("/models", theme.accent),
                    Span::styled("  inspect the registry    ", theme.muted),
                    Span::styled("?", theme.accent),
                    Span::styled("  open help", theme.muted),
                ]),
            ]
        }
        RegistryState::Ready | RegistryState::ReadyWithWarnings { .. } => vec![
            Line::from(Span::styled("Ready to explore", theme.text)),
            Line::from(Span::styled(
                "Open Models to inspect discovered local artifacts.",
                theme.muted,
            )),
        ],
    };
    frame.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: true }),
        ui_layout.overview_body,
    );
}

fn render_metrics(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    compact: bool,
) {
    let model_value = match &app.snapshot.registry_state {
        RegistryState::Ready | RegistryState::ReadyWithWarnings { .. } => {
            app.snapshot.models.len().to_string()
        }
        state => state.label().to_owned(),
    };
    let runtime_value = app.runtime_list.as_ref().map_or_else(
        || {
            if app.runtime_list_loading {
                "Preparing".to_owned()
            } else {
                "Unavailable".to_owned()
            }
        },
        |snapshot| snapshot.installed.len().to_string(),
    );
    let active_model = app.control.as_ref().map_or_else(
        || {
            if app.control_observation_pending() {
                "Observing".to_owned()
            } else {
                "Unavailable".to_owned()
            }
        },
        |control| {
            if control.backends.is_empty() {
                "None".to_owned()
            } else {
                format!("{} resident", control.backends.len())
            }
        },
    );
    let values = [
        ("SERVER", app.snapshot.server.label().to_owned()),
        ("MODELS", model_value),
        ("RUNTIMES", runtime_value),
        ("ACTIVE MODEL", active_model),
    ];
    if compact {
        let lines = values.into_iter().map(|(label, value)| {
            Line::from(vec![
                Span::styled(format!("{label:<14}"), theme.hint),
                Span::styled(value, theme.text),
            ])
        });
        frame.render_widget(Paragraph::new(lines.collect::<Vec<_>>()), area);
        return;
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 4); 4])
        .spacing(2)
        .split(area);
    for (index, (label, value)) in values.into_iter().enumerate() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(label, theme.hint)),
                Line::from(Span::styled(value, theme.text)),
            ])
            .block(
                Block::default()
                    .borders(Borders::LEFT)
                    .border_set(glyphs.border)
                    .border_style(theme.accent)
                    .padding(Padding::horizontal(1)),
            ),
            columns[index],
        );
    }
}

fn render_model_header(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    ui_layout: &UiLayout,
    area: Rect,
) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("Model Library", theme.accent))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let installed_active = app.model_library_view == ModelLibraryView::Installed;
    let installed_label = if installed_active {
        "[ Installed ]"
    } else {
        "  Installed  "
    };
    let mut installed_style = if installed_active {
        theme.nav_active
    } else {
        theme.nav_inactive
    };
    if app.hover == Some(HoverTarget::ModelLibraryTab(ModelLibraryView::Installed)) {
        installed_style = installed_style.patch(theme.hovered);
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(installed_label, installed_style))),
        ui_layout.model_installed_tab,
    );

    let discover_active = app.model_library_view == ModelLibraryView::Discover;
    let discover_label = match (discover_active, ui_layout.compact, glyphs.unicode) {
        (true, true, _) => "[ Discover ]",
        (false, true, _) => "  Discover  ",
        (true, false, true) => "[ Discover · Hugging Face ]",
        (false, false, true) => "  Discover · Hugging Face  ",
        (true, false, false) => "[ Discover - Hugging Face ]",
        (false, false, false) => "  Discover - Hugging Face  ",
    };
    let mut discover_style = if discover_active {
        theme.nav_active
    } else {
        theme.nav_inactive
    };
    if app.hover == Some(HoverTarget::ModelLibraryTab(ModelLibraryView::Discover)) {
        discover_style = discover_style.patch(theme.hovered);
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(discover_label, discover_style))),
        ui_layout.model_discover_tab,
    );

    let detail_area = Rect::new(area.x, area.y + 2, area.width, 1);
    if app.model_library_view == ModelLibraryView::Installed {
        let detail = match &app.snapshot.registry_state {
            RegistryState::NotScanned => "Local discovery is preparing".to_owned(),
            RegistryState::Scanning => "Scanning configured external model paths".to_owned(),
            RegistryState::Failed { .. } => "Local discovery failed; see Logs".to_owned(),
            RegistryState::Ready if app.snapshot.registry_warnings.is_empty() => {
                "Local artifacts and managed downloads".to_owned()
            }
            RegistryState::Ready | RegistryState::ReadyWithWarnings { .. } => format!(
                "Local artifacts with {} warning(s); see Logs",
                app.snapshot.registry_warnings.len()
            ),
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(detail, theme.muted))),
            detail_area,
        );
        return;
    }

    let label_width = ui_layout.model_search_field.x.saturating_sub(detail_area.x);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("Search:", theme.hint))),
        Rect::new(
            detail_area.x,
            detail_area.y,
            label_width,
            detail_area.height,
        ),
    );

    let query_width = ui_layout.model_search_field.width.saturating_sub(2) as usize;
    let (query, query_style) = if app.model_search_query.is_empty() {
        let placeholder = format!("Search Hugging Face models{}", glyphs.ellipsis);
        let text = if app.model_search_editing {
            format!("_  {placeholder}")
        } else {
            placeholder
        };
        (
            truncate_middle(&text, query_width, glyphs.ellipsis),
            theme.hint,
        )
    } else if app.model_search_editing {
        (
            editable_query_text(
                &app.model_search_query,
                app.model_search_cursor,
                query_width,
                glyphs.ellipsis,
            ),
            theme.command,
        )
    } else {
        (
            truncate_middle(&app.model_search_query, query_width, glyphs.ellipsis),
            theme.text,
        )
    };
    let mut field_style = query_style;
    if app.model_search_editing {
        field_style = field_style.patch(theme.focused);
    }
    if app.hover == Some(HoverTarget::ModelSearchField) {
        field_style = field_style.patch(theme.hovered);
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("[{query:<query_width$}]"),
            field_style,
        ))),
        ui_layout.model_search_field,
    );

    let submit_label = if ui_layout.compact {
        "[ Go ]"
    } else {
        "[ Search ]"
    };
    let mut submit_style = if app.model_search_loading {
        theme.muted
    } else {
        theme.accent
    };
    if !app.model_search_loading && app.hover == Some(HoverTarget::ModelSearchSubmit) {
        submit_style = submit_style.patch(theme.hovered);
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(submit_label, submit_style))),
        ui_layout.model_search_submit,
    );
}

fn editable_query_text(query: &str, cursor: usize, max_width: usize, ellipsis: &str) -> String {
    let mut characters = query.chars().collect::<Vec<_>>();
    let marker = cursor.min(characters.len());
    characters.insert(marker, '_');
    if characters.len() <= max_width {
        return characters.into_iter().collect();
    }

    let ellipsis_width = ellipsis.chars().count();
    let window_width = max_width
        .saturating_sub(ellipsis_width.saturating_mul(2))
        .max(1);
    let mut start = marker.saturating_sub(window_width / 2);
    let mut end = (start + window_width).min(characters.len());
    if marker >= end {
        end = (marker + 1).min(characters.len());
        start = end.saturating_sub(window_width);
    }
    let mut visible = String::new();
    if start > 0 {
        visible.push_str(ellipsis);
    }
    visible.extend(characters[start..end].iter());
    if end < characters.len() {
        visible.push_str(ellipsis);
    }
    visible
}

fn render_models(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    ui_layout: &UiLayout,
) {
    let layout = content_layout(area);
    render_model_header(frame, app, theme, glyphs, ui_layout, layout[0]);
    if app.model_library_view == ModelLibraryView::Discover {
        render_model_discover(frame, app, theme, glyphs, ui_layout);
        render_model_downloads(frame, app, theme, glyphs, ui_layout.model_downloads);
        return;
    }
    if matches!(app.snapshot.registry_state, RegistryState::NotScanned) {
        render_empty(
            frame,
            layout[1],
            &format!("{}  Preparing model discovery", glyphs.transitional),
            "The registry has not been scanned yet; background discovery starts after the first frame.",
            theme,
        );
        return;
    }
    if matches!(app.snapshot.registry_state, RegistryState::Scanning) {
        render_empty(
            frame,
            layout[1],
            &format!("{}  Discovering local models", glyphs.transitional),
            "The registry will update automatically. You can keep using the interface while it scans.",
            theme,
        );
        return;
    }
    if let RegistryState::Failed { message } = &app.snapshot.registry_state {
        render_empty(frame, layout[1], "Model discovery failed", message, theme);
        return;
    }
    if app.snapshot.models.is_empty() {
        render_empty(
            frame,
            layout[1],
            &format!("{}  No models installed", glyphs.empty),
            "Open Discover to browse Hugging Face, or configure external model paths.",
            theme,
        );
        return;
    }
    let items = ui_layout.model_rows.iter().map(|(index, _)| {
        let model = &app.snapshot.models[*index];
        let runtime_override = app.runtime_list.as_ref().and_then(|snapshot| {
            let runtime_id = snapshot.selections.model_overrides.get(&model.id)?;
            let label = snapshot
                .installed
                .iter()
                .find(|status| &status.runtime.manifest.runtime_id == runtime_id)
                .map(|status| {
                    let identity = &status.runtime.manifest.identity;
                    format!("{} {}", identity.engine_id, identity.version)
                })
                .unwrap_or_else(|| runtime_id.to_string());
            Some(label)
        });
        let mut style = if app.selected_model == Some(*index) {
            theme.selected
        } else {
            ratatui::style::Style::default()
        };
        if app.hover == Some(HoverTarget::Model(*index)) {
            style = style.patch(theme.hovered);
        }
        ListItem::new(vec![
            Line::from(vec![
                Span::styled(
                    if app.control.as_ref().is_some_and(|control| {
                        control.backends.iter().any(|backend| {
                            matches!(
                                backend.lifecycle,
                                norted_engine::BackendLifecycle::Loading
                                    | norted_engine::BackendLifecycle::Running
                            ) && backend.model_id == model.id
                        })
                    }) {
                        format!("{}  ", glyphs.running)
                    } else {
                        "   ".to_owned()
                    },
                    theme.success,
                ),
                Span::styled(&model.display_name, theme.text),
                Span::styled(format!("  {}", model.format.as_str()), theme.accent),
                Span::styled(
                    runtime_override
                        .map(|runtime| format!("  override: {runtime}"))
                        .unwrap_or_default(),
                    theme.hint,
                ),
            ]),
            {
                let size_text = format_bytes(model.size_bytes);
                let path_width = (ui_layout.model_list.width as usize)
                    .saturating_sub(size_text.chars().count() + 2);
                let path = truncate_middle(
                    &model.path.display().to_string(),
                    path_width,
                    glyphs.ellipsis,
                );
                Line::from(vec![
                    Span::styled(size_text, theme.muted),
                    Span::styled(format!("  {path}"), theme.hint),
                ])
            },
            Line::from(Span::styled(
                format!(
                    "Artifact: {} · provenance: {} · click row to select",
                    model.format,
                    if model.norted_package.is_some() {
                        "Norted package"
                    } else if let Some(provenance) = &model.provenance {
                        provenance.provider.as_str()
                    } else {
                        "raw/local"
                    },
                ),
                theme.hint,
            )),
        ])
        .style(style)
    });
    frame.render_widget(List::new(items), ui_layout.model_list);
    render_installed_model_actions(frame, app, theme, ui_layout);
    if let Some(progress) = app.selected_model_load_progress() {
        render_load_progress(
            frame,
            ui_layout.model_progress,
            progress,
            app.load_animation_frame,
            theme,
            glyphs,
        );
    }
    render_model_downloads(frame, app, theme, glyphs, ui_layout.model_downloads);
}

fn render_installed_model_actions(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    ui_layout: &UiLayout,
) {
    let Some(model) = app
        .selected_model
        .and_then(|index| app.snapshot.models.get(index))
    else {
        return;
    };
    for (action, area) in &ui_layout.installed_model_actions {
        let active = app.selected_model_is_active();
        let (label, state) = match action {
            InstalledModelAction::CreateProfile => (
                if ui_layout.compact {
                    "[ Profile ]"
                } else {
                    "[ Create Profile ]"
                },
                if app.settings_busy() {
                    ActionState::Disabled
                } else {
                    ActionState::Primary
                },
            ),
            InstalledModelAction::Runtime => (
                "[ Runtime ]",
                if app.model_runtime_picker_available() {
                    ActionState::Normal
                } else {
                    ActionState::Disabled
                },
            ),
            InstalledModelAction::Unload => (
                "[ Unload ]",
                if active && !app.control_busy() {
                    ActionState::Normal
                } else {
                    ActionState::Disabled
                },
            ),
            InstalledModelAction::Remove => {
                let enabled = model.provenance.is_some() && !active && !app.model_removal_busy();
                (
                    if app.model_remove_armed() {
                        "[ Confirm Remove ]"
                    } else {
                        "[ Remove ]"
                    },
                    if !enabled {
                        ActionState::Disabled
                    } else if app.model_remove_armed() {
                        ActionState::Confirm
                    } else {
                        ActionState::Destructive
                    },
                )
            }
        };
        let hovered = app.hover == Some(HoverTarget::InstalledModelAction(*action));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_middle(label, area.width as usize, "…"),
                action_style(theme, state, hovered),
            ))),
            *area,
        );
    }
}

fn render_model_discover(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    ui_layout: &UiLayout,
) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("Format", theme.hint))),
        ui_layout.model_format_row,
    );
    for (format, area) in &ui_layout.model_format_filters {
        let label = match format {
            None => "[ All ]",
            Some(ArtifactFormat::Gguf) => "[ GGUF ]",
            Some(ArtifactFormat::Q27) => "[ Q27 ]",
            Some(ArtifactFormat::Ninfer) => "[ NInfer ]",
        };
        let mut style = if app.model_search_loading {
            theme.muted
        } else if app.model_search_format == *format {
            theme.selected
        } else {
            theme.nav_inactive
        };
        if !app.model_search_loading && app.hover == Some(HoverTarget::ModelFormatFilter(*format)) {
            style = style.patch(theme.hovered);
        }
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(label, style))),
            *area,
        );
    }

    if app.model_search_loading {
        render_empty(
            frame,
            ui_layout.model_list,
            &format!("{}  Searching Hugging Face", glyphs.transitional),
            "Relevant GGUF, q27, and NInfer files will appear as concrete downloadable variants.",
            theme,
        );
    } else if app.model_search.is_none() {
        render_empty(
            frame,
            ui_layout.model_list,
            "Search Hugging Face",
            "Type a model, publisher, or repository name and search. Formats: GGUF, q27, and NInfer.",
            theme,
        );
    } else if app.model_search_artifacts().is_empty() {
        render_empty(
            frame,
            ui_layout.model_list,
            &format!("{}  No matching artifacts", glyphs.empty),
            "Try a broader repository or publisher query, or cycle the format filter.",
            theme,
        );
    } else {
        let artifacts = app.model_search_artifacts();
        let items = ui_layout.model_rows.iter().map(|(index, _)| {
            let (repository, artifact) = artifacts[*index];
            let mut style = if app.selected_model_search_result == Some(*index) {
                theme.selected
            } else {
                ratatui::style::Style::default()
            };
            if app.hover == Some(HoverTarget::Model(*index)) {
                style = style.patch(theme.hovered);
            }
            let size = artifact
                .size_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "size unknown".to_owned());
            let companion = if let Some(manifest) = &artifact.package_manifest {
                format!("package candidate via {manifest}")
            } else if artifact.required_companions.is_empty() {
                "standalone/raw candidate".to_owned()
            } else {
                format!("requires {}", artifact.required_companions.join(", "))
            };
            let revision = truncate_middle(&repository.revision, 12, glyphs.ellipsis);
            let action_width = ui_layout
                .model_download_actions
                .iter()
                .find(|(action_index, _)| action_index == index)
                .map_or(0, |(_, area)| area.width as usize);
            let status_width = (ui_layout.model_list.width as usize)
                .saturating_sub(action_width.saturating_add(1));
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(artifact.format.as_str().to_ascii_uppercase(), theme.accent),
                    Span::styled(format!("  {}", artifact.filename), theme.text),
                ]),
                Line::from(vec![
                    Span::styled(&repository.repository, theme.text),
                    Span::styled(
                        format!(" · {size} · rev {revision} · {companion}"),
                        theme.muted,
                    ),
                ]),
                Line::from(Span::styled(
                    truncate_middle(
                        "Format candidate · runtime compatibility unverified",
                        status_width,
                        glyphs.ellipsis,
                    ),
                    theme.hint,
                )),
            ])
            .style(style)
        });
        frame.render_widget(List::new(items), ui_layout.model_list);
        for (index, area) in &ui_layout.model_download_actions {
            let label = "[ Download ]";
            let mut style = if app.selected_model_search_result == Some(*index) {
                theme.selected
            } else {
                ratatui::style::Style::default()
            };
            style = style.patch(theme.accent);
            if app.hover == Some(HoverTarget::ModelDownloadAction(*index)) {
                style = style.patch(theme.hovered);
            }
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(label, style))),
                *area,
            );
        }
    }
}

fn render_model_downloads(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    area: Rect,
) {
    if area.height == 0 || app.model_download_jobs.is_empty() {
        return;
    }
    let active = app
        .model_download_jobs
        .iter()
        .filter(|job| {
            !job.is_terminal() && job.phase != norted_model_library::ModelOperationPhase::Queued
        })
        .count();
    let queued = app
        .model_download_jobs
        .iter()
        .filter(|job| job.phase == norted_model_library::ModelOperationPhase::Queued)
        .count();
    let separator = if glyphs.unicode { " · " } else { " | " };
    let title = format!(
        "Downloads  {active} active{separator}{queued} queued{separator}limit {}",
        app.max_parallel_model_downloads()
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_middle(&title, area.width as usize, glyphs.ellipsis),
            theme.hint,
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let mut jobs = app
        .model_download_jobs
        .iter()
        .filter(|job| !job.is_terminal())
        .collect::<Vec<_>>();
    jobs.extend(
        app.model_download_jobs
            .iter()
            .rev()
            .filter(|job| job.is_terminal()),
    );
    let lines = jobs
        .into_iter()
        .take(area.height.saturating_sub(1) as usize)
        .map(|job| {
            let phase = match job.phase {
                norted_model_library::ModelOperationPhase::Queued => {
                    job.queue_position.map_or_else(
                        || "Queued".to_owned(),
                        |position| format!("Queued #{position}"),
                    )
                }
                norted_model_library::ModelOperationPhase::Resolving => "Resolving".to_owned(),
                norted_model_library::ModelOperationPhase::Downloading => "Downloading".to_owned(),
                norted_model_library::ModelOperationPhase::Verifying => "Verifying".to_owned(),
                norted_model_library::ModelOperationPhase::Validating => "Validating".to_owned(),
                norted_model_library::ModelOperationPhase::Installing => "Installing".to_owned(),
                norted_model_library::ModelOperationPhase::Installed => "Installed".to_owned(),
                norted_model_library::ModelOperationPhase::Failed => "Failed".to_owned(),
                norted_model_library::ModelOperationPhase::Cancelled => "Cancelled".to_owned(),
            };
            let identity = job
                .filename
                .as_deref()
                .or(job.repository.as_deref())
                .unwrap_or(&job.model_ref);
            let mut metrics = Vec::new();
            if job.phase != norted_model_library::ModelOperationPhase::Queued {
                metrics.push(job.total_bytes.map_or_else(
                    || format_bytes(job.downloaded_bytes),
                    |total| {
                        format!(
                            "{} / {}",
                            format_bytes(job.downloaded_bytes),
                            format_bytes(total)
                        )
                    },
                ));
            }
            if let Some(percent) = job.progress_percent {
                metrics.push(format!("{percent:.1}%"));
            }
            if let Some(rate) = job.transfer_bytes_per_second {
                metrics.push(format!("{}/s", format_bytes(rate as u64)));
            }
            if let Some(eta) = job.estimated_remaining {
                metrics.push(format!("~{}", compact_duration(eta)));
            }
            if matches!(
                job.phase,
                norted_model_library::ModelOperationPhase::Verifying
                    | norted_model_library::ModelOperationPhase::Validating
                    | norted_model_library::ModelOperationPhase::Installing
                    | norted_model_library::ModelOperationPhase::Installed
                    | norted_model_library::ModelOperationPhase::Failed
            ) {
                metrics.push(job.message.clone());
            }
            let progress_bar = job.progress_percent.map_or_else(String::new, |percent| {
                let width = if area.width >= 90 { 12 } else { 8 };
                let filled = ((percent / 100.0) * f64::from(width)).round() as usize;
                format!(
                    "[{}{}] ",
                    "=".repeat(filled.min(width as usize)),
                    ".".repeat(width as usize - filled.min(width as usize))
                )
            });
            let text = format!(
                "{phase:<11} {progress_bar}{identity}{}",
                if metrics.is_empty() {
                    String::new()
                } else {
                    format!("  {}", metrics.join("  "))
                }
            );
            let style = match job.phase {
                norted_model_library::ModelOperationPhase::Installed => theme.success,
                norted_model_library::ModelOperationPhase::Failed
                | norted_model_library::ModelOperationPhase::Cancelled => theme.error,
                norted_model_library::ModelOperationPhase::Queued => theme.muted,
                _ => theme.text,
            };
            Line::from(Span::styled(
                truncate_middle(&text, area.width as usize, glyphs.ellipsis),
                style,
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        ),
    );
}

fn compact_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        format!("{}h{}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m{}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn render_runtimes(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    ui_layout: &UiLayout,
) {
    let layout = content_layout(area);
    let subtitle = if app.runtime_list_loading {
        "Refreshing installed packs in the background"
    } else {
        "Installed packs, persisted format defaults, and upstream runtimes"
    };
    frame.render_widget(section_title("Runtimes", subtitle, theme), layout[0]);

    let summary = app.runtime_list.as_ref().map_or_else(
        || {
            vec![Line::from(vec![
                Span::styled("GGUF  ", theme.hint),
                Span::styled("not loaded", theme.muted),
                Span::styled("    Q27  ", theme.hint),
                Span::styled("not loaded", theme.muted),
                Span::styled("    NINFER  ", theme.hint),
                Span::styled("not loaded", theme.muted),
            ])]
        },
        |snapshot| {
            let gguf = selection_text(app, ArtifactFormat::Gguf, ui_layout.compact);
            let q27 = selection_text(app, ArtifactFormat::Q27, ui_layout.compact);
            let ninfer = selection_text(app, ArtifactFormat::Ninfer, ui_layout.compact);
            let accelerator = if snapshot.host.accelerators.is_empty() {
                "CPU"
            } else {
                "NVIDIA"
            };
            let mut lines = vec![
                Line::from(vec![
                    Span::styled("GGUF  ", theme.hint),
                    Span::styled(gguf, theme.text),
                    Span::styled("    Q27  ", theme.hint),
                    Span::styled(q27, theme.text),
                    Span::styled("    NINFER  ", theme.hint),
                    Span::styled(ninfer, theme.text),
                ]),
                Line::from(vec![
                    Span::styled("HOST  ", theme.hint),
                    Span::styled(
                        format!(
                            "{} / {} / {accelerator}",
                            snapshot.host.platform, snapshot.host.architecture
                        ),
                        theme.muted,
                    ),
                ]),
            ];
            if !snapshot.warnings.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("{} runtime warning(s); see Logs", snapshot.warnings.len()),
                    theme.warning,
                )));
            }
            lines
        },
    );
    frame.render_widget(
        Paragraph::new(summary).wrap(Wrap { trim: true }),
        ui_layout.runtime_summary,
    );

    if app.runtime_list_loading && app.runtime_list.is_none() {
        render_empty(
            frame,
            ui_layout.runtime_list,
            &format!("{}  Inspecting installed runtimes", glyphs.transitional),
            "Local pack manifests and configured external runtimes are being probed.",
            theme,
        );
    } else if let Some(error) = &app.runtime_list_error {
        render_empty(
            frame,
            ui_layout.runtime_list,
            "Installed runtime scan failed",
            error,
            theme,
        );
    } else if app
        .runtime_list
        .as_ref()
        .is_some_and(|snapshot| snapshot.installed.is_empty())
    {
        let guidance = if app.snapshot.models.is_empty() {
            "Use Search available to install a compatible runtime."
        } else {
            "Use Search available to install a compatible runtime. The s key remains a shortcut."
        };
        render_empty(
            frame,
            ui_layout.runtime_list,
            &format!("{}  No runtimes installed", glyphs.empty),
            guidance,
            theme,
        );
    } else if let Some(snapshot) = &app.runtime_list {
        let items = ui_layout.runtime_rows.iter().map(|(index, _)| {
            let status = &snapshot.installed[*index];
            let manifest = &status.runtime.manifest;
            let identity = &manifest.identity;
            let (compatibility, compatibility_style) =
                compatibility_label(&status.compatibility, theme);
            let formats = manifest
                .supported_formats
                .iter()
                .map(|format| format.as_str().to_ascii_uppercase())
                .collect::<Vec<_>>()
                .join("/");
            let selected = if status.selected_for.is_empty() {
                String::new()
            } else {
                format!("  default: {}", status.selected_for.join(", "))
            };
            let update = app
                .runtime_updates
                .get(&manifest.runtime_id)
                .map(runtime_update_text)
                .unwrap_or_default();
            let source_build = manifest
                .source_build
                .as_ref()
                .map_or_else(String::new, |build| {
                    let builder = match build.build_system {
                        RuntimeSourceBuildSystem::Cmake => {
                            format!("CMake {}", build.toolchain.cmake_version)
                        }
                        RuntimeSourceBuildSystem::Make => {
                            format!("Make {}", build.toolchain.make_version)
                        }
                    };
                    format!(
                        "  source {} tree {} · {} · {} · CUDA {}",
                        &build.source.commit_sha[..8],
                        &build.source.tree_sha[..8],
                        build.recipe_version,
                        builder,
                        build.toolchain.nvcc_version,
                    )
                });
            let mut style = if app.selected_runtime == Some(*index) {
                theme.selected
            } else {
                ratatui::style::Style::default()
            };
            if app.hover == Some(HoverTarget::Runtime(*index)) {
                style = style.patch(theme.hovered);
            }
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(format!("{}  ", glyphs.running), theme.success),
                    Span::styled(&identity.engine_id, theme.text),
                    Span::styled(format!("  {}", identity.version), theme.accent),
                    Span::styled(format!("  {compatibility}"), compatibility_style),
                ]),
                Line::from(vec![
                    Span::styled(
                        format!("{formats}  {} / {}", identity.accelerator, identity.variant),
                        theme.muted,
                    ),
                    Span::styled(selected, theme.hint),
                    Span::styled(update, theme.warning),
                    Span::styled(source_build, theme.hint),
                ]),
            ])
            .style(style)
        });
        frame.render_widget(List::new(items), ui_layout.runtime_list);
    }

    if ui_layout.runtime_actions.height > 0 {
        let search_disabled = app.runtime_mutation_busy() || app.runtime_search_loading;
        let search_style = action_style(
            theme,
            if search_disabled {
                ActionState::Disabled
            } else {
                ActionState::Primary
            },
            app.hover == Some(HoverTarget::RuntimeSearchAction),
        );
        let update_disabled = app.runtime_mutation_busy() || app.runtime_update_loading;
        let update_style = action_style(
            theme,
            if update_disabled {
                ActionState::Disabled
            } else {
                ActionState::Normal
            },
            app.hover == Some(HoverTarget::RuntimeUpdateAction),
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if ui_layout.compact {
                    "[ Search ]"
                } else {
                    "[ Search Available ]"
                },
                search_style,
            ))),
            ui_layout.runtime_search_action,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if app.runtime_update_loading {
                    "Checking…"
                } else if ui_layout.compact {
                    "[ Updates ]"
                } else {
                    "[ Check Updates ]"
                },
                update_style,
            ))),
            ui_layout.runtime_update_action,
        );
        render_selected_runtime_actions(frame, app, theme, ui_layout);
        if let Some(operation) = &app.runtime_operation {
            let progress_x = ui_layout.runtime_update_action.right().saturating_add(2);
            let progress_area = Rect::new(
                progress_x,
                ui_layout.runtime_actions.y,
                ui_layout.runtime_actions.right().saturating_sub(progress_x),
                1,
            );
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    progress_text(operation),
                    theme.muted,
                ))),
                progress_area,
            );
        }
    }
}

fn render_selected_runtime_actions(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    ui_layout: &UiLayout,
) {
    let Some(status) = app
        .selected_runtime
        .and_then(|index| app.runtime_list.as_ref()?.installed.get(index))
    else {
        return;
    };
    let runtime_id = &status.runtime.manifest.runtime_id;
    for (action, area) in &ui_layout.selected_runtime_actions {
        let (label, state) = match action {
            SelectedRuntimeAction::Default(format) => {
                let format_label = format.as_str().to_ascii_uppercase();
                let selected = app.runtime_list.as_ref().is_some_and(|snapshot| {
                    snapshot.selections.format_defaults.get(format) == Some(runtime_id)
                });
                let enabled = !app.runtime_mutation_busy() && status.compatibility.is_usable();
                (
                    if area.width <= 10 {
                        format!("[ {format_label} ]")
                    } else {
                        format!("[ {format_label} Default ]")
                    },
                    if !enabled {
                        ActionState::Disabled
                    } else if selected {
                        ActionState::Primary
                    } else {
                        ActionState::Normal
                    },
                )
            }
            SelectedRuntimeAction::Update => (
                "[ Update ]".to_owned(),
                if app.selected_runtime_update_available() && !app.runtime_mutation_busy() {
                    ActionState::Primary
                } else {
                    ActionState::Disabled
                },
            ),
            SelectedRuntimeAction::Remove => (
                if app.runtime_remove_armed() {
                    "[ Confirm Remove ]".to_owned()
                } else {
                    "[ Remove ]".to_owned()
                },
                if !app.selected_runtime_removable() {
                    ActionState::Disabled
                } else if app.runtime_remove_armed() {
                    ActionState::Confirm
                } else {
                    ActionState::Destructive
                },
            ),
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_middle(&label, area.width as usize, "…"),
                action_style(
                    theme,
                    state,
                    app.hover == Some(HoverTarget::SelectedRuntimeAction(*action)),
                ),
            ))),
            *area,
        );
    }
}

fn runtime_update_text(state: &RuntimeUpdateState) -> String {
    match state {
        RuntimeUpdateState::Current => "  current".to_owned(),
        RuntimeUpdateState::Unmanaged => "  unmanaged".to_owned(),
        RuntimeUpdateState::NewerCompatibleVersion { version, .. } => {
            format!("  update: {version}")
        }
        RuntimeUpdateState::Pinned {
            newer_version: Some(version),
            ..
        } => format!("  pinned; {version} available"),
        RuntimeUpdateState::Pinned { .. } => "  pinned".to_owned(),
        RuntimeUpdateState::CatalogUnavailable(_) => "  catalog unavailable".to_owned(),
        RuntimeUpdateState::ProviderError(_) => "  provider warning".to_owned(),
        RuntimeUpdateState::NoLongerPublished => "  no longer published".to_owned(),
        RuntimeUpdateState::ChannelUnavailable { .. } => "  channel unavailable".to_owned(),
    }
}

fn selection_text(app: &App, format: ArtifactFormat, compact: bool) -> String {
    let Some(snapshot) = &app.runtime_list else {
        return "none".to_owned();
    };
    let Some(runtime_id) = snapshot.selections.format_defaults.get(&format) else {
        return "none".to_owned();
    };
    snapshot
        .installed
        .iter()
        .find(|status| &status.runtime.manifest.runtime_id == runtime_id)
        .map(|status| {
            let identity = &status.runtime.manifest.identity;
            if compact {
                format!("{} {}", identity.engine_id, identity.version)
            } else {
                format!(
                    "{} {} {} / {}",
                    identity.engine_id, identity.version, identity.accelerator, identity.variant
                )
            }
        })
        .unwrap_or_else(|| runtime_id.to_string())
}

pub(crate) fn compatibility_label<'a>(
    compatibility: &'a RuntimeCompatibility,
    theme: &'a Theme,
) -> (&'a str, ratatui::style::Style) {
    match compatibility {
        RuntimeCompatibility::Recommended => ("recommended", theme.success),
        RuntimeCompatibility::Compatible => ("compatible", theme.accent),
        RuntimeCompatibility::NeedsAttention(_) => ("needs attention", theme.warning),
        RuntimeCompatibility::Incompatible(_) => ("incompatible", theme.error),
    }
}

fn render_server(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    ui_layout: &UiLayout,
) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title(
            "API Server",
            "Cross-process state verified through the local health endpoint",
            theme,
        ),
        layout[0],
    );
    let pending = app.control_observation_pending();
    let endpoint = app
        .control
        .as_ref()
        .and_then(|control| control.public_endpoint.as_deref())
        .or_else(|| app.snapshot.server.endpoint())
        .unwrap_or(if pending { "Observing" } else { "Not serving" });
    let lifecycle = app
        .control
        .as_ref()
        .map(|control| {
            format!(
                "{} resident / {} running",
                control.backends.len(),
                control.running_backend_count
            )
        })
        .unwrap_or_else(|| {
            if pending {
                "Observing".to_owned()
            } else {
                "Unavailable".to_owned()
            }
        });
    let active_profile = app
        .control
        .as_ref()
        .map(|control| {
            let value = control
                .backends
                .iter()
                .map(|backend| {
                    format!(
                        "{} [{:?}/{:?}/{:?}; req {}; leases {}]",
                        backend.model_profile_id,
                        backend.role,
                        backend.residency,
                        backend.lifecycle,
                        backend.active_request_count,
                        backend.primary_lease_count,
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            if value.is_empty() {
                "None".to_owned()
            } else {
                value
            }
        })
        .unwrap_or_else(|| if pending { "Unknown" } else { "None" }.to_owned());
    let active_model = app
        .control
        .as_ref()
        .map(|control| {
            let value = control
                .backends
                .iter()
                .map(|backend| backend.model_id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            if value.is_empty() {
                "None".to_owned()
            } else {
                value
            }
        })
        .unwrap_or_else(|| if pending { "Unknown" } else { "None" }.to_owned());
    let active_engine = app
        .control
        .as_ref()
        .map(|control| {
            let value = control
                .backends
                .iter()
                .filter_map(|backend| backend.engine_id.clone())
                .collect::<Vec<_>>()
                .join(", ");
            if value.is_empty() {
                "None".to_owned()
            } else {
                value
            }
        })
        .unwrap_or_else(|| if pending { "Unknown" } else { "None" }.to_owned());
    let active_runtime = app
        .control
        .as_ref()
        .map(|control| {
            let value = control
                .backends
                .iter()
                .filter_map(|backend| backend.runtime_id.as_ref().map(ToString::to_string))
                .collect::<Vec<_>>()
                .join(", ");
            if value.is_empty() {
                "None".to_owned()
            } else {
                value
            }
        })
        .unwrap_or_else(|| if pending { "Unknown" } else { "None" }.to_owned());
    let private_backend = app
        .control
        .as_ref()
        .map(|control| {
            let value = control
                .backends
                .iter()
                .filter_map(|backend| backend.private_endpoint.clone())
                .collect::<Vec<_>>()
                .join(", ");
            if value.is_empty() {
                "None".to_owned()
            } else {
                value
            }
        })
        .unwrap_or_else(|| if pending { "Unknown" } else { "None" }.to_owned());
    let auth = &app.public_auth_status;
    let active_key_count = if app.public_auth_loading {
        "Loading".to_owned()
    } else if app.public_auth_error.is_some() {
        "Unavailable".to_owned()
    } else {
        auth.active_key_count.to_string()
    };
    let exposure = if auth.loopback { "loopback" } else { "remote" };
    frame.render_widget(
        Paragraph::new(vec![
            key_value("STATE", app.snapshot.server.label(), theme),
            key_value("ENDPOINT", endpoint, theme),
            key_value("PROFILES", &active_profile, theme),
            key_value("PUBLIC BIND", &auth.bind, theme),
            key_value("EXPOSURE", exposure, theme),
            key_value("AUTH CONFIGURED", &auth.configured_mode.to_string(), theme),
            key_value("AUTH EFFECTIVE", &auth.effective_mode.to_string(), theme),
            key_value("ACTIVE API KEYS", &active_key_count, theme),
            key_value("RESIDENCY", &lifecycle, theme),
            key_value("MODELS", &active_model, theme),
            key_value("ENGINES", &active_engine, theme),
            key_value("RUNTIMES", &active_runtime, theme),
            key_value("PRIVATE ENDPOINTS", &private_backend, theme),
            if auth.insecure_remote {
                Line::from(Span::styled(
                    "SECURITY WARNING: remote authentication is disabled",
                    theme.error,
                ))
            } else if let Some(error) = &app.public_auth_error {
                Line::from(Span::styled(
                    format!("AUTH STATE UNAVAILABLE: {error}"),
                    theme.error,
                ))
            } else {
                Line::default()
            },
            Line::default(),
            Line::from(Span::styled("Available now", theme.text)),
            Line::from(Span::styled("GET  /health", theme.accent)),
            Line::from(Span::styled("GET  /v1/models", theme.accent)),
            Line::from(Span::styled("POST /v1/responses", theme.accent)),
            Line::from(Span::styled("POST /v1/chat/completions", theme.accent)),
            Line::from(Span::styled(
                "PRIVATE is the selected engine's internal loopback endpoint.",
                theme.muted,
            )),
        ])
        .wrap(Wrap { trim: true }),
        ui_layout.server_details,
    );
    if let Some(progress) = app.load_progress() {
        render_load_progress(
            frame,
            ui_layout.server_progress,
            progress,
            app.load_animation_frame,
            theme,
            glyphs,
        );
    }
}

fn render_logs(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, ui_layout: &UiLayout) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title("Logs", "Application and model-registry warnings", theme),
        layout[0],
    );
    if app.log_scroll > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "[ Follow latest ]",
                action_style(
                    theme,
                    ActionState::Primary,
                    app.hover == Some(HoverTarget::FollowLatest),
                ),
            ))),
            ui_layout.logs_follow_latest,
        );
    }
    let visible = layout[1].height as usize;
    let offset = app
        .log_scroll
        .min(app.logs.len().saturating_sub(ui_layout.log_capacity()));
    let end = app.logs.len().saturating_sub(offset);
    let start = end.saturating_sub(visible);
    let lines = app.logs[start..end].iter().map(|entry| {
        let (label, style) = match entry.level {
            norted_core::LogLevel::Info => ("INFO", theme.accent),
            norted_core::LogLevel::Warning => ("WARN", theme.warning),
            norted_core::LogLevel::Error => ("ERR ", theme.error),
        };
        Line::from(vec![
            Span::styled(label, style),
            Span::styled("  ", theme.muted),
            Span::styled(&entry.message, theme.text),
        ])
    });
    frame.render_widget(Paragraph::new(lines.collect::<Vec<_>>()), layout[1]);
}

fn render_settings(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    ui_layout: &UiLayout,
) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title(
            "Settings",
            "Global and engine defaults; Model Profiles are edited on their own screen",
            theme,
        ),
        layout[0],
    );
    let scopes = app.settings_scopes();
    for (index, rect) in &ui_layout.settings_scope_rows {
        let Some(scope) = scopes.get(*index) else {
            continue;
        };
        let label = match scope {
            crate::app::SettingsScope::Global => "Global".to_owned(),
            crate::app::SettingsScope::Engine(engine) => match engine.as_str() {
                "ninfer" => "NInfer".to_owned(),
                _ => engine.clone(),
            },
            crate::app::SettingsScope::ModelProfile(profile) => profile.to_string(),
        };
        let style = if app.settings_scope_index == *index {
            theme.selected
        } else if app.hover == Some(HoverTarget::SettingsScope(*index)) {
            theme.hovered
        } else {
            theme.muted
        };
        frame.render_widget(Paragraph::new(format!(" {label} ")).style(style), *rect);
    }
    let info = if app.settings_input.is_some() {
        String::new()
    } else {
        let definitions = app.settings_definitions();
        let definition = definitions.get(app.settings_setting_index);
        let description = definition.map_or("", |definition| definition.description.as_str());
        let default = definition.map_or_else(String::new, |definition| {
            app.settings_default_detail(&definition.id)
        });
        format!(
            "Rows select. Click a value to edit or change it; Inherit clears this layer's override.\n{description}\n{default}"
        )
    };
    frame.render_widget(
        Paragraph::new(info).style(theme.hint),
        Rect::new(
            ui_layout.settings_scopes.x,
            ui_layout.settings_scopes.y.saturating_add(1),
            ui_layout.settings_scopes.width,
            4,
        ),
    );
    render_settings_input(frame, app, theme, ui_layout);
    render_setting_rows(frame, app, theme, ui_layout);
}

fn render_model_profiles(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    ui_layout: &UiLayout,
) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title(
            "Model Profiles",
            "User-created model + engine + settings serving targets",
            theme,
        ),
        layout[0],
    );
    for (index, rect) in &ui_layout.settings_scope_rows {
        let Some(profile) = app.model_profile_values().get(*index).copied() else {
            continue;
        };
        let missing = !app
            .snapshot
            .models
            .iter()
            .any(|model| model.id == profile.model_id);
        let active = app
            .control
            .as_ref()
            .is_some_and(|status| status.backend(&profile.id).is_some());
        let label = format!(
            " {} · {:?}{}{} ",
            profile.display_name,
            profile.role,
            if active { " · active" } else { "" },
            if missing { " · missing" } else { "" },
        );
        let style = if app.selected_model_profile == Some(*index) {
            theme.selected
        } else if !app.settings_busy() && app.hover == Some(HoverTarget::SettingsScope(*index)) {
            theme.hovered
        } else if missing {
            theme.warning
        } else {
            theme.muted
        };
        frame.render_widget(Paragraph::new(label).style(style), *rect);
    }
    let info_area = Rect::new(
        ui_layout.settings_scopes.x,
        ui_layout.settings_scopes.y.saturating_add(1),
        ui_layout.settings_scopes.width,
        5,
    );
    let info = if app.settings_input.is_some() {
        Vec::new()
    } else if let Some(profile) = app.selected_model_profile_value() {
        let model = app.selected_profile_model();
        let definitions = app.settings_definitions();
        let selected_definition = definitions.get(app.settings_setting_index);
        let mut lines = vec![
            Line::from(vec![
                Span::styled(format!("{}  ", profile.id), theme.text),
                Span::styled(format!("engine {}  ", profile.engine_id), theme.accent),
                Span::styled(
                    model
                        .map(|model| model.display_name.clone())
                        .unwrap_or_else(|| format!("MISSING {}", profile.model_id)),
                    if model.is_some() {
                        theme.hint
                    } else {
                        theme.warning
                    },
                ),
            ]),
            Line::from(Span::styled(
                model
                    .map(|model| model.path.display().to_string())
                    .unwrap_or_default(),
                theme.muted,
            )),
            Line::from(Span::styled(
                if app.settings_busy() && app.settings_schema.is_none() {
                    "resolving runtime/model settings…".to_owned()
                } else {
                    app.settings_validation_error.as_deref().map_or_else(
                        || {
                            format!(
                                "runtime {} · rows select; explicit buttons act",
                                app.settings_runtime_id
                                    .as_ref()
                                    .map(ToString::to_string)
                                    .unwrap_or_else(|| "not validated".to_owned()),
                            )
                        },
                        ToOwned::to_owned,
                    )
                },
                if app.settings_validation_error.is_some() {
                    theme.warning
                } else {
                    theme.hint
                },
            )),
        ];
        if let Some(definition) = selected_definition {
            lines.push(Line::from(Span::styled(
                definition.description.clone(),
                theme.hint,
            )));
            lines.push(Line::from(Span::styled(
                app.settings_default_detail(&definition.id),
                theme.muted,
            )));
        }
        lines
    } else {
        vec![Line::from(Span::styled(
            "No Model Profiles. Select an artifact on Models and use Create Profile.",
            theme.muted,
        ))]
    };
    frame.render_widget(Paragraph::new(info).wrap(Wrap { trim: true }), info_area);
    render_settings_input(frame, app, theme, ui_layout);
    render_model_profile_actions(frame, app, theme, ui_layout);
    render_setting_rows(frame, app, theme, ui_layout);
}

fn render_settings_input(frame: &mut Frame<'_>, app: &App, theme: &Theme, ui_layout: &UiLayout) {
    let Some(input) = &app.settings_input else {
        return;
    };
    let prompt = match input.kind {
        crate::app::SettingsInputKind::ProfileName => "New Model Profile ID",
        crate::app::SettingsInputKind::DuplicateProfile => "Duplicate Model Profile ID",
        crate::app::SettingsInputKind::SettingValue => "Override value",
    };
    let field_width = ui_layout
        .settings_input_field
        .width
        .saturating_sub(prompt.len() as u16 + 4) as usize;
    let field = format!(
        "{prompt}: [{}]",
        truncate_middle(&format!("{}_", input.text), field_width, "…")
    );
    let field_style = if app.hover == Some(HoverTarget::SettingsInputField) {
        theme.focused.patch(theme.hovered)
    } else {
        theme.focused
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(field, field_style))),
        ui_layout.settings_input_field,
    );
    let submit_label = match input.kind {
        crate::app::SettingsInputKind::ProfileName => "[ Create ]",
        crate::app::SettingsInputKind::DuplicateProfile => "[ Duplicate ]",
        crate::app::SettingsInputKind::SettingValue => "[ Save ]",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            submit_label,
            action_style(
                theme,
                ActionState::Primary,
                app.hover == Some(HoverTarget::SettingsInputSubmit),
            ),
        ))),
        ui_layout.settings_input_submit,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "[ Cancel ]",
            action_style(
                theme,
                ActionState::Normal,
                app.hover == Some(HoverTarget::SettingsInputCancel),
            ),
        ))),
        ui_layout.settings_input_cancel,
    );
}

fn render_model_profile_actions(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    ui_layout: &UiLayout,
) {
    let has_model = app.selected_profile_model().is_some();
    let active = app.selected_profile_is_active();
    for (action, area) in &ui_layout.model_profile_actions {
        let (label, state) = match action {
            ModelProfileAction::Load => (
                "[ Load ]",
                if has_model && app.control.is_some() && !active && !app.control_busy() {
                    ActionState::Primary
                } else {
                    ActionState::Disabled
                },
            ),
            ModelProfileAction::Unload => (
                "[ Unload ]",
                if active && !app.control_busy() {
                    ActionState::Normal
                } else {
                    ActionState::Disabled
                },
            ),
            ModelProfileAction::Model => (
                "[ Model ]",
                if !app.settings_busy() && !app.snapshot.models.is_empty() {
                    ActionState::Normal
                } else {
                    ActionState::Disabled
                },
            ),
            ModelProfileAction::Engine => (
                "[ Engine ]",
                if !app.settings_busy() && has_model {
                    ActionState::Normal
                } else {
                    ActionState::Disabled
                },
            ),
            ModelProfileAction::Role => (
                "[ Role ]",
                if app.settings_busy() {
                    ActionState::Disabled
                } else {
                    ActionState::Normal
                },
            ),
            ModelProfileAction::Duplicate => (
                if ui_layout.compact {
                    "[ Copy ]"
                } else {
                    "[ Duplicate ]"
                },
                if app.settings_busy() {
                    ActionState::Disabled
                } else {
                    ActionState::Normal
                },
            ),
            ModelProfileAction::Delete => (
                if app.profile_delete_armed() {
                    "[ Confirm Delete ]"
                } else {
                    "[ Delete ]"
                },
                if app.settings_busy() {
                    ActionState::Disabled
                } else if app.profile_delete_armed() {
                    ActionState::Confirm
                } else {
                    ActionState::Destructive
                },
            ),
            ModelProfileAction::Refresh => (
                if ui_layout.compact {
                    "[ Sync ]"
                } else {
                    "[ Refresh ]"
                },
                if !app.settings_busy() && has_model {
                    ActionState::Normal
                } else {
                    ActionState::Disabled
                },
            ),
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_middle(label, area.width as usize, "…"),
                action_style(
                    theme,
                    state,
                    app.hover == Some(HoverTarget::ModelProfileAction(*action)),
                ),
            ))),
            *area,
        );
    }
}

fn render_setting_rows(frame: &mut Frame<'_>, app: &App, theme: &Theme, ui_layout: &UiLayout) {
    if app.settings_loading {
        frame.render_widget(
            Paragraph::new("Loading settings state…").style(theme.muted),
            ui_layout.settings_list,
        );
        return;
    }
    if let Some(error) = &app.settings_error {
        frame.render_widget(
            Paragraph::new(error.as_str())
                .style(theme.error)
                .wrap(Wrap { trim: true }),
            ui_layout.settings_list,
        );
        return;
    }
    if app.screen == crate::app::Screen::ModelProfiles && app.settings_schema.is_none() {
        let (message, style) = if app.settings_busy() {
            ("Resolving runtime/model settings…", theme.muted)
        } else if let Some(error) = &app.settings_validation_error {
            (error.as_str(), theme.warning)
        } else {
            ("Runtime/model settings are not resolved.", theme.muted)
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(style)
                .wrap(Wrap { trim: true }),
            ui_layout.settings_list,
        );
        return;
    }
    let definitions = app.settings_definitions();
    if definitions.is_empty() {
        frame.render_widget(
            Paragraph::new("No settings are available in this scope.").style(theme.muted),
            ui_layout.settings_list,
        );
        return;
    }
    for (index, rect) in &ui_layout.settings_rows {
        let Some(definition) = definitions.get(*index) else {
            continue;
        };
        let (value, source, set_here) = app.settings_value_display(&definition.id);
        let mut style = if app.settings_setting_index == *index {
            theme.selected
        } else {
            ratatui::style::Style::default()
        };
        if app.hover == Some(HoverTarget::Setting(*index)) {
            style = style.patch(theme.hovered);
        }
        let support = if definition.supported {
            if set_here { "override" } else { "inherited" }
        } else {
            "unsupported"
        };
        let label = match &definition.kind {
            norted_core::SettingKind::Choice { choices } => {
                format!("{} [{}]", definition.label, choices.join("|"))
            }
            norted_core::SettingKind::UnsignedIntegerOrChoice { choices, .. } => {
                format!("{} [number|{}]", definition.label, choices.join("|"))
            }
            _ => definition.label.clone(),
        };
        let starts_category = index == &0
            || definitions
                .get(index.saturating_sub(1))
                .is_none_or(|previous| previous.category != definition.category);
        let status = if definition.supported {
            format!("{label} · {source} · {support}")
        } else {
            format!("{label} · unsupported")
        };
        let heading = if starts_category {
            format!("{}  ──  {status}", definition.category)
        } else {
            format!("              {status}")
        };
        let value_area = ui_layout
            .setting_values
            .iter()
            .find(|(value_index, _)| value_index == index)
            .map(|(_, area)| *area)
            .unwrap_or_default();
        let id_width = value_area.x.saturating_sub(rect.x).saturating_sub(2) as usize;
        let lines = vec![
            Line::from(Span::styled(
                heading,
                if starts_category {
                    theme.hint
                } else {
                    theme.muted
                },
            )),
            Line::from(vec![Span::styled(
                format!(
                    "  {:<id_width$}",
                    truncate_middle(&definition.id.to_string(), id_width, "…")
                ),
                theme.text,
            )]),
        ];
        frame.render_widget(Paragraph::new(lines).style(style), *rect);
        let value_enabled = definition.supported && !app.settings_busy();
        let value_label = format!("[ {value} ]");
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_middle(&value_label, value_area.width as usize, "…"),
                action_style(
                    theme,
                    if value_enabled {
                        ActionState::Primary
                    } else {
                        ActionState::Disabled
                    },
                    app.hover == Some(HoverTarget::SettingValue(*index)),
                ),
            ))),
            value_area,
        );
        if let Some((_, inherit_area)) = ui_layout
            .setting_inherit_actions
            .iter()
            .find(|(inherit_index, _)| inherit_index == index)
        {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "[ Inherit ]",
                    action_style(
                        theme,
                        if app.settings_busy() {
                            ActionState::Disabled
                        } else {
                            ActionState::Normal
                        },
                        app.hover == Some(HoverTarget::SettingInherit(*index)),
                    ),
                ))),
                *inherit_area,
            );
        }
    }
}

fn render_help_content(frame: &mut Frame<'_>, area: Rect, theme: &Theme, glyphs: &Glyphs) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title("Help", "Navigate directly or use slash commands", theme),
        layout[0],
    );
    frame.render_widget(Paragraph::new(help_lines(theme, glyphs)), layout[1]);
}

pub fn help_lines<'a>(theme: &Theme, glyphs: &Glyphs) -> Vec<Line<'a>> {
    vec![
        Line::from(Span::styled("NAVIGATION", theme.hint)),
        key_value("Tab / Shift+Tab", "change focus", theme),
        key_value("Left / Right", "move navigation focus", theme),
        key_value("Enter", "open the focused page", theme),
        key_value("Mouse", "rows select; [ buttons ] perform actions", theme),
        key_value("Hover", "highlights interactive controls", theme),
        key_value("Keyboard", "shortcuts remain available everywhere", theme),
        Line::default(),
        Line::from(Span::styled("MODELS AND MODEL PROFILES", theme.hint)),
        key_value(
            "Models",
            "Installed inventory and Hugging Face Discover views",
            theme,
        ),
        key_value("Left / Right", "change the Model Library view", theme),
        key_value(
            "Mouse",
            "click rows, values, tabs, filters, and visible actions",
            theme,
        ),
        key_value(
            "e / Enter / f / d",
            "edit, search, filter, or download",
            theme,
        ),
        key_value(
            "Model Profiles",
            "normal load and per-profile override screen",
            theme,
        ),
        key_value(
            "l/u/e/Delete",
            "load, unload, rebind engine, or inherit",
            theme,
        ),
        Line::default(),
        Line::from(Span::styled("SETTINGS AND RUNTIMES", theme.hint)),
        key_value("Settings", "Global and per-engine defaults", theme),
        key_value("Up/Down or j/k", "select or scroll the current page", theme),
        key_value("s", "search available runtimes from Runtimes", theme),
        key_value("g / Q / N", "select runtime for GGUF / Q27 / NInfer", theme),
        key_value("u", "check for runtime updates", theme),
        key_value(
            "Shift+U",
            "install the selected reported update side by side",
            theme,
        ),
        key_value(
            "d twice",
            "confirm removal of an unselected inactive runtime",
            theme,
        ),
        Line::default(),
        Line::from(Span::styled("COMMANDS", theme.hint)),
        key_value("/", "open slash-command suggestions", theme),
        key_value(glyphs.up_down, "select a suggestion", theme),
        key_value("Enter", "run the selected command", theme),
        key_value("Esc", "close or cancel", theme),
        Line::default(),
        Line::from(Span::styled("GLOBAL", theme.hint)),
        key_value("?", "toggle this help", theme),
        key_value("Ctrl+C", "exit cleanly", theme),
        Line::default(),
        Line::from(Span::styled(
            "Slash commands: /load /unload /status /models /model-profiles /runtimes /server /logs /settings /help /quit",
            theme.muted,
        )),
    ]
}
