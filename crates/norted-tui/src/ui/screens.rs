use norted_core::{
    ArtifactFormat, RegistryState, RuntimeCompatibility, RuntimeSourceBuildSystem,
    RuntimeUpdatePreference, RuntimeUpdateState,
};
use norted_engine::{
    BackendLifecycle, BackendParallelism, BackendStatus, InferenceActivity, InferenceActivityPhase,
    InstalledRuntimeStatus,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, FocusArea, ModelLibraryView, Screen};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{
    ActionState, action_style, content_layout, format_bytes, key_value, key_value_width,
    load_progress_compact, marked_input_window, marquee_text, remaining_width, render_empty,
    render_load_progress, section_title, truncate_middle,
};
use crate::ui::layout::{
    DownloadJobAction, HoverTarget, InstalledModelAction, ModelProfileAction,
    SelectedRuntimeAction, UiLayout, overview_backend_card_inner,
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
        Screen::Help => render_help_content(frame, area, theme, glyphs, ui_layout),
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
    let title = content_layout(area, ui_layout.compact)[0];
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
    render_loaded_models(frame, app, theme, ui_layout);
}

fn render_loaded_models(frame: &mut Frame<'_>, app: &App, theme: &Theme, ui_layout: &UiLayout) {
    let backends = app.resident_backends();
    let header = if backends.is_empty() {
        "Loaded Models".to_owned()
    } else {
        format!("Loaded Models  {}", backends.len())
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(header, theme.accent))),
        Rect::new(
            ui_layout.overview_resident.x,
            ui_layout.overview_resident.y,
            ui_layout.overview_resident.width,
            1,
        ),
    );

    if backends.is_empty() {
        let (title, detail) = match &app.snapshot.registry_state {
            RegistryState::NotScanned | RegistryState::Scanning => (
                "No models loaded",
                "Model discovery is running; loaded backends will appear here automatically.",
            ),
            RegistryState::Failed { .. } => (
                "No models loaded",
                "Model discovery failed. Open Logs for details.",
            ),
            RegistryState::Ready | RegistryState::ReadyWithWarnings { .. }
                if app.snapshot.models.is_empty() =>
            {
                (
                    "No models loaded",
                    "Open Models > Discover to add an artifact, then load a Model Profile.",
                )
            }
            RegistryState::Ready | RegistryState::ReadyWithWarnings { .. } => (
                "No models loaded",
                "Open Model Profiles to load a model backend.",
            ),
        };
        render_empty(
            frame,
            Rect::new(
                ui_layout.overview_resident.x,
                ui_layout.overview_resident.y.saturating_add(1),
                ui_layout.overview_resident.width,
                ui_layout.overview_resident.height.saturating_sub(1),
            ),
            title,
            detail,
            theme,
        );
        return;
    }

    for (index, area) in &ui_layout.overview_backend_rows {
        let Some(backend) = backends.get(*index) else {
            continue;
        };
        render_backend_card(frame, *area, *index, backend, app, theme, ui_layout);
    }

    let shown = ui_layout.overview_backend_rows.len();
    if shown < backends.len() && ui_layout.overview_resident.width >= 12 {
        let first = app.overview_scroll.saturating_add(1);
        let last = app
            .overview_scroll
            .saturating_add(shown)
            .min(backends.len());
        let text = format!("{first}-{last} / {}", backends.len());
        let width = text.len().min(ui_layout.overview_resident.width as usize) as u16;
        frame.render_widget(
            Paragraph::new(Span::styled(text, theme.hint)),
            Rect::new(
                ui_layout.overview_resident.right().saturating_sub(width),
                ui_layout.overview_resident.y,
                width,
                1,
            ),
        );
    }
}

fn render_backend_card(
    frame: &mut Frame<'_>,
    area: Rect,
    index: usize,
    backend: &BackendStatus,
    app: &App,
    theme: &Theme,
    ui_layout: &UiLayout,
) {
    let glyphs = Glyphs::current(app.unicode);
    let selected = app.focus == FocusArea::Content && app.overview_selected == Some(index);
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_set(glyphs.border)
        .border_style(if selected { theme.accent } else { theme.border })
        .style(if selected {
            theme.selected
        } else {
            theme.panel
        })
        .padding(Padding::horizontal(1));
    let inner = overview_backend_card_inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let status = backend_activity_label(backend, app.load_animation_frame, inner.width, &glyphs);
    let status_style = match backend.lifecycle {
        BackendLifecycle::Failed => theme.error,
        BackendLifecycle::Stopping => theme.warning,
        BackendLifecycle::Loading => theme.accent,
        BackendLifecycle::Running if backend.active_request_count > 0 => theme.success,
        BackendLifecycle::Running | BackendLifecycle::Stopped => theme.muted,
    };
    let mut lines = vec![Line::from(Span::styled(status, status_style))];

    let profile = app
        .model_profiles
        .as_ref()
        .and_then(|profiles| profiles.profiles.get(&backend.model_profile_id));
    let model = app
        .snapshot
        .models
        .iter()
        .find(|model| model.id == backend.model_id);
    let profile_name = profile.map_or_else(
        || {
            backend.provenance.as_ref().map_or_else(
                || backend.model_profile_id.to_string(),
                |provenance| provenance.model_profile.display_name.clone(),
            )
        },
        |profile| profile.display_name.clone(),
    );
    let model_name = model.map_or_else(
        || backend.model_id.to_string(),
        |model| model.display_name.clone(),
    );
    let identity = if profile_name == backend.model_profile_id.as_str() {
        format!("{profile_name} / {model_name}")
    } else {
        format!(
            "{profile_name} [{}] / {model_name}",
            backend.model_profile_id
        )
    };
    let identity = if ui_layout.compact {
        format!("{identity}  /  {}", backend_runtime_label(backend))
    } else {
        identity
    };
    lines.push(Line::from(Span::styled(
        marquee_text(&identity, inner.width as usize, app.marquee_animation_frame),
        theme.text,
    )));

    if !ui_layout.compact {
        let runtime = backend_runtime_label(backend);
        lines.push(Line::from(Span::styled(
            marquee_text(&runtime, inner.width as usize, app.marquee_animation_frame),
            theme.muted,
        )));
    }

    let mut metadata = model
        .map(|model| format!("Size {}", format_bytes(model.size_bytes)))
        .unwrap_or_else(|| "Size unknown".to_owned());
    if let Some(parallel) = &backend.parallel_requests {
        match parallel {
            BackendParallelism::Exact(value) => {
                metadata.push_str(&format!("   Parallel {value}"));
            }
            BackendParallelism::Auto => metadata.push_str("   Parallel auto"),
        }
    }
    let action = match backend.lifecycle {
        BackendLifecycle::Loading => Some("[ Cancel ]"),
        BackendLifecycle::Running | BackendLifecycle::Failed => Some("[ Unload ]"),
        BackendLifecycle::Stopped | BackendLifecycle::Stopping => None,
    };
    if let Some(action) = action {
        let available = (inner.width as usize).saturating_sub(action.len() + 1);
        let metadata = truncate_middle(&metadata, available, glyphs.ellipsis);
        let gap = (inner.width as usize)
            .saturating_sub(UnicodeWidthStr::width(metadata.as_str()) + action.len());
        let hovered = app.hover == Some(HoverTarget::OverviewBackendAction(index));
        lines.push(Line::from(vec![
            Span::styled(metadata, theme.hint),
            Span::raw(" ".repeat(gap)),
            Span::styled(
                action,
                action_style(theme, ActionState::Destructive, hovered),
            ),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            truncate_middle(&metadata, inner.width as usize, glyphs.ellipsis),
            theme.hint,
        )));
    }

    if !ui_layout.compact {
        let request_capacity = inner.height.saturating_sub(lines.len() as u16) as usize;
        let needs_summary =
            backend.active_request_count > backend.activities.len().min(request_capacity);
        let activity_capacity = if needs_summary {
            request_capacity.saturating_sub(1)
        } else {
            request_capacity
        };
        let shown_requests = backend.activities.len().min(activity_capacity);
        for activity in backend.activities.iter().take(shown_requests) {
            lines.push(Line::from(Span::styled(
                format_activity(activity),
                theme.text,
            )));
        }
        let hidden = backend.active_request_count.saturating_sub(shown_requests);
        if hidden > 0 && request_capacity > 0 {
            lines.push(Line::from(Span::styled(
                format!("+{hidden} more requests"),
                theme.hint,
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn backend_activity_label(
    backend: &BackendStatus,
    animation_frame: u32,
    width: u16,
    glyphs: &Glyphs,
) -> String {
    match backend.lifecycle {
        BackendLifecycle::Loading => backend.load_progress.as_ref().map_or_else(
            || "LOADING".to_owned(),
            |progress| {
                format!(
                    "LOADING  {}",
                    load_progress_compact(
                        progress,
                        animation_frame,
                        width.saturating_sub(9),
                        glyphs
                    )
                )
            },
        ),
        BackendLifecycle::Running if backend.active_request_count == 0 => "IDLE".to_owned(),
        BackendLifecycle::Running => {
            if backend.active_request_count == 1 && backend.activities.len() == 1 {
                format_activity_state(&backend.activities[0])
            } else if backend.active_request_count == 1 {
                "ACTIVE".to_owned()
            } else {
                format!("ACTIVE {} requests", backend.active_request_count)
            }
        }
        BackendLifecycle::Stopping => "STOPPING".to_owned(),
        BackendLifecycle::Failed => backend.failure.as_ref().map_or_else(
            || "FAILED".to_owned(),
            |failure| format!("FAILED  {failure}"),
        ),
        BackendLifecycle::Stopped => "STOPPED".to_owned(),
    }
}

pub(super) fn backend_runtime_label(backend: &BackendStatus) -> String {
    let mut parts = Vec::new();
    if let Some(engine) = &backend.engine_id {
        parts.push(engine.clone());
    }
    if let Some(runtime) = backend.runtime_id.as_ref().or_else(|| {
        backend
            .provenance
            .as_ref()
            .map(|provenance| &provenance.runtime.runtime_id)
    }) {
        parts.push(runtime.to_string());
    }
    if let Some(version) = &backend.runtime_version {
        parts.push(format!("v{version}"));
    }
    if let Some(variant) = &backend.runtime_variant {
        parts.push(variant.clone());
    }
    if parts.is_empty() {
        "Runtime resolving".to_owned()
    } else {
        parts.join("  /  ")
    }
}

fn format_activity(activity: &InferenceActivity) -> String {
    format!("{}  {}", activity.id, format_activity_state(activity))
}

fn format_activity_state(activity: &InferenceActivity) -> String {
    match activity.phase {
        InferenceActivityPhase::Active => "ACTIVE".to_owned(),
        InferenceActivityPhase::ProcessingPrompt => {
            if let (Some(current), Some(total)) = (activity.prompt_current, activity.prompt_total)
                && total > 0
                && current <= total
            {
                #[allow(clippy::cast_precision_loss)]
                let percent = current as f64 / total as f64 * 100.0;
                format!("PROCESSING PROMPT {percent:.1}%")
            } else {
                "PROCESSING PROMPT".to_owned()
            }
        }
        InferenceActivityPhase::Generating => activity.generated_tokens.map_or_else(
            || "GENERATING".to_owned(),
            |tokens| format!("GEN {tokens} tok"),
        ),
    }
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
                    .padding(Padding::new(2, 1, 1, 0)),
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
        Paragraph::new(Line::from(Span::styled(format!("[{query}]"), field_style))),
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

fn editable_query_text(query: &str, cursor: usize, max_width: usize, _ellipsis: &str) -> String {
    marked_input_window(query, cursor, max_width, "_")
}

fn render_models(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    ui_layout: &UiLayout,
) {
    let layout = content_layout(area, ui_layout.compact);
    render_model_header(frame, app, theme, glyphs, ui_layout, layout[0]);
    if app.model_library_view == ModelLibraryView::Discover {
        render_model_discover(frame, app, theme, glyphs, ui_layout);
        render_model_downloads(frame, app, theme, glyphs, ui_layout);
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
    let items = ui_layout.model_rows.iter().map(|(index, row)| {
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
        let active_row =
            app.selected_model == Some(*index) || app.hover == Some(HoverTarget::Model(*index));
        let marker = if app.control.as_ref().is_some_and(|control| {
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
        };
        let format_span = format!("  {}", model.format.as_str());
        let primary_width = installed_model_name_width(row.width, &marker, &format_span);
        let model_name = if active_row {
            marquee_text(
                &model.display_name,
                primary_width,
                app.marquee_animation_frame / 3,
            )
        } else {
            truncate_middle(&model.display_name, primary_width, glyphs.ellipsis)
        };
        let override_width =
            remaining_width(row.width, &[&marker, &format_span]).saturating_sub(primary_width);
        let runtime_override = runtime_override
            .map(|runtime| {
                truncate_middle(
                    &format!("  override: {runtime}"),
                    override_width,
                    glyphs.ellipsis,
                )
            })
            .unwrap_or_default();
        let mut lines = vec![
            Line::from(vec![
                Span::styled(marker, theme.success),
                Span::styled(model_name, theme.text),
                Span::styled(format_span, theme.accent),
                Span::styled(runtime_override, theme.hint),
            ]),
            {
                let size_text = format_bytes(model.size_bytes);
                let path_width = (row.width as usize)
                    .saturating_sub(UnicodeWidthStr::width(size_text.as_str()) + 2);
                let full_path = model.path.display().to_string();
                let path = if active_row {
                    marquee_text(&full_path, path_width, app.marquee_animation_frame / 3)
                } else {
                    truncate_middle(&full_path, path_width, glyphs.ellipsis)
                };
                Line::from(vec![
                    Span::styled(size_text, theme.muted),
                    Span::styled(format!("  {path}"), theme.hint),
                ])
            },
            Line::from(Span::styled(
                truncate_middle(
                    &format!(
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
                    row.width as usize,
                    glyphs.ellipsis,
                ),
                theme.hint,
            )),
        ];
        if ui_layout.model_row_height > 3 {
            lines.push(Line::default());
        }
        ListItem::new(lines).style(style)
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
    render_model_downloads(frame, app, theme, glyphs, ui_layout);
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
        let items = ui_layout.model_rows.iter().map(|(index, row)| {
            let (repository, artifact) = artifacts[*index];
            let mut style = if app.selected_model_search_result == Some(*index) {
                theme.selected
            } else {
                ratatui::style::Style::default()
            };
            if app.hover == Some(HoverTarget::Model(*index)) {
                style = style.patch(theme.hovered);
            }
            let active_row = app.selected_model_search_result == Some(*index)
                || app.hover == Some(HoverTarget::Model(*index));
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
            let format_span = available_format_span(artifact.format.as_str());
            let filename_width = remaining_width(row.width, &[&format_span]);
            let filename = if active_row {
                marquee_text(
                    &artifact.filename,
                    filename_width,
                    app.marquee_animation_frame / 3,
                )
            } else {
                truncate_middle(&artifact.filename, filename_width, glyphs.ellipsis)
            };
            let metadata = format!(
                "{} · {size} · rev {revision} · {companion}",
                repository.repository
            );
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(format_span, theme.accent),
                    Span::styled(filename, theme.text),
                ]),
                Line::from(Span::styled(
                    truncate_middle(&metadata, row.width as usize, glyphs.ellipsis),
                    theme.muted,
                )),
                Line::from(Span::styled(
                    truncate_middle(
                        "Format candidate · runtime compatibility unverified",
                        status_width,
                        glyphs.ellipsis,
                    ),
                    theme.hint,
                )),
            ];
            if ui_layout.model_row_height > 3 {
                lines.push(Line::default());
            }
            ListItem::new(lines).style(style)
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
    ui_layout: &UiLayout,
) {
    let area = ui_layout.model_downloads;
    if area.height == 0 || app.model_download_jobs.is_empty() {
        return;
    }
    let active = app
        .model_download_jobs
        .iter()
        .filter(|job| {
            !job.is_terminal()
                && !matches!(
                    job.phase,
                    norted_model_library::ModelOperationPhase::Queued
                        | norted_model_library::ModelOperationPhase::Paused
                )
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

    for (index, card) in &ui_layout.download_job_rows {
        let Some(job) = app.model_download_jobs.get(*index) else {
            continue;
        };
        render_model_download_card(frame, app, theme, glyphs, (*index, job, *card), ui_layout);
    }
}

fn render_model_download_card(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    card: (usize, &norted_model_library::ModelDownloadJob, Rect),
    ui_layout: &UiLayout,
) {
    let (index, job, area) = card;
    let selected = app.selected_model_download_job.as_ref() == Some(&job.id);
    let controls = download_job_controls(job);
    let action_width = controls
        .iter()
        .map(|action| download_action_label(*action, glyphs, ui_layout.compact).width() + 1)
        .sum::<usize>();
    let prefix = format!("{}  ", glyphs.download);
    let identity_width = (area.width as usize)
        .saturating_sub(prefix.width())
        .saturating_sub(action_width);
    let identity = download_identity(job);
    let identity = marquee_text(&identity, identity_width, app.marquee_animation_frame / 3);
    let identity_style = if selected && app.focus == FocusArea::Content {
        theme.text.patch(theme.focused)
    } else {
        theme.text
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prefix, download_phase_style(job.phase, theme)),
            Span::styled(identity, identity_style),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );

    for action in controls {
        let Some((_, _, action_area)) = ui_layout
            .download_job_actions
            .iter()
            .find(|(action_index, candidate, _)| *action_index == index && *candidate == action)
        else {
            continue;
        };
        let hovered = app.hover == Some(HoverTarget::DownloadJobAction(index, action));
        let state = if action == DownloadJobAction::Cancel {
            ActionState::Destructive
        } else {
            ActionState::Primary
        };
        let mut style = action_style(theme, state, hovered);
        if selected && app.focus == FocusArea::Content {
            style = style.patch(theme.focused);
        }
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                download_action_label(action, glyphs, ui_layout.compact),
                style,
            ))),
            *action_area,
        );
    }

    let bar_area = Rect::new(area.x, area.y.saturating_add(1), area.width, 1);
    frame.render_widget(
        Paragraph::new(download_progress_bar(
            job,
            bar_area.width,
            app.marquee_animation_frame,
            theme,
            glyphs,
        )),
        bar_area,
    );
    let status = download_status(job, glyphs);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_middle(&status, area.width as usize, glyphs.ellipsis),
            download_phase_style(job.phase, theme),
        ))),
        Rect::new(area.x, area.y.saturating_add(2), area.width, 1),
    );
}

pub(crate) fn download_identity(job: &norted_model_library::ModelDownloadJob) -> String {
    match (job.repository.as_deref(), job.filename.as_deref()) {
        (Some(repository), Some(filename)) => format!("{repository}  {filename}"),
        (Some(repository), None) => repository.to_owned(),
        (None, Some(filename)) => filename.to_owned(),
        (None, None) => job.model_ref.clone(),
    }
}

fn download_job_controls(job: &norted_model_library::ModelDownloadJob) -> Vec<DownloadJobAction> {
    use norted_model_library::ModelOperationPhase;
    match job.phase {
        ModelOperationPhase::Queued
        | ModelOperationPhase::Resolving
        | ModelOperationPhase::Downloading => {
            vec![DownloadJobAction::Cancel, DownloadJobAction::Pause]
        }
        ModelOperationPhase::Paused => {
            vec![DownloadJobAction::Cancel, DownloadJobAction::Resume]
        }
        _ => Vec::new(),
    }
}

fn download_action_label(action: DownloadJobAction, glyphs: &Glyphs, compact: bool) -> String {
    let glyph = match action {
        DownloadJobAction::Pause => glyphs.pause,
        DownloadJobAction::Resume => glyphs.resume,
        DownloadJobAction::Cancel => glyphs.cancel,
    };
    if compact {
        format!("[{glyph}]")
    } else {
        format!("[ {glyph} ]")
    }
}

fn download_progress_bar(
    job: &norted_model_library::ModelDownloadJob,
    width: u16,
    animation_frame: u32,
    theme: &Theme,
    glyphs: &Glyphs,
) -> Line<'static> {
    let width = width as usize;
    let determinate = job.progress_percent.map(|percent| percent / 100.0);
    let filled = if let Some(fraction) = determinate {
        (fraction.clamp(0.0, 1.0) * width as f64).round() as usize
    } else if matches!(
        job.phase,
        norted_model_library::ModelOperationPhase::Resolving
            | norted_model_library::ModelOperationPhase::Downloading
    ) && width > 0
    {
        let pulse = (width / 5).max(1);
        ((animation_frame as usize) % (width + pulse)).saturating_sub(pulse)
    } else {
        0
    };
    let pulse_width = if determinate.is_none()
        && matches!(
            job.phase,
            norted_model_library::ModelOperationPhase::Resolving
                | norted_model_library::ModelOperationPhase::Downloading
        ) {
        (width / 5).max(1).min(width.saturating_sub(filled))
    } else {
        filled.min(width)
    };
    let leading = if determinate.is_some() {
        0
    } else {
        filled.min(width)
    };
    let empty = width.saturating_sub(leading + pulse_width);
    if determinate.is_some() {
        Line::from(vec![
            Span::styled(glyphs.progress_full.repeat(pulse_width), theme.accent),
            Span::styled(glyphs.progress_empty.repeat(empty), theme.border),
        ])
    } else {
        Line::from(vec![
            Span::styled(glyphs.progress_empty.repeat(leading), theme.border),
            Span::styled(glyphs.progress_full.repeat(pulse_width), theme.accent),
            Span::styled(glyphs.progress_empty.repeat(empty), theme.border),
        ])
    }
}

fn download_status(job: &norted_model_library::ModelDownloadJob, glyphs: &Glyphs) -> String {
    use norted_model_library::ModelOperationPhase;
    let separator = if glyphs.unicode { " · " } else { " | " };
    let transferred = job.total_bytes.map_or_else(
        || format_bytes(job.downloaded_bytes),
        |total| {
            format!(
                "{} of {}",
                format_bytes(job.downloaded_bytes),
                format_bytes(total)
            )
        },
    );
    match job.phase {
        ModelOperationPhase::Queued => job.queue_position.map_or_else(
            || "Queued".to_owned(),
            |position| format!("Queued{separator}position {position}"),
        ),
        ModelOperationPhase::Resolving => format!("Resolving{separator}{}", job.message),
        ModelOperationPhase::Downloading => {
            let mut details = vec![transferred];
            if let Some(rate) = job.transfer_bytes_per_second {
                details.push(format!("{}/s", format_bytes(rate as u64)));
            }
            if let Some(remaining) = job.estimated_remaining {
                details.push(friendly_remaining(remaining));
            }
            details.join(separator)
        }
        ModelOperationPhase::Paused => format!("{transferred}{separator}Paused"),
        ModelOperationPhase::Verifying => format!("Verifying{separator}{}", job.message),
        ModelOperationPhase::Validating => format!("Validating{separator}{}", job.message),
        ModelOperationPhase::Installing => format!("Installing{separator}{}", job.message),
        ModelOperationPhase::Installed => format!("Installed{separator}{}", job.message),
        ModelOperationPhase::Failed => format!("Failed{separator}{}", job.message),
        ModelOperationPhase::Cancelled => "Cancelled".to_owned(),
    }
}

fn friendly_remaining(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        format!("{} hr {} min left", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{} min left", (seconds + 30) / 60)
    } else {
        format!("{} sec left", seconds.max(1))
    }
}

fn download_phase_style(
    phase: norted_model_library::ModelOperationPhase,
    theme: &Theme,
) -> ratatui::style::Style {
    use norted_model_library::ModelOperationPhase;
    match phase {
        ModelOperationPhase::Installed => theme.success,
        ModelOperationPhase::Failed | ModelOperationPhase::Cancelled => theme.error,
        ModelOperationPhase::Queued | ModelOperationPhase::Paused => theme.muted,
        ModelOperationPhase::Resolving
        | ModelOperationPhase::Verifying
        | ModelOperationPhase::Validating
        | ModelOperationPhase::Installing => theme.warning,
        ModelOperationPhase::Downloading => theme.text,
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
    let layout = content_layout(area, ui_layout.compact);
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
        let items = ui_layout.runtime_rows.iter().map(|(index, row)| {
            let status = &snapshot.installed[*index];
            let manifest = &status.runtime.manifest;
            let identity = &manifest.identity;
            let (compatibility, compatibility_style) =
                compatibility_label(&status.compatibility, theme);
            let mut style = if app.selected_runtime == Some(*index) {
                theme.selected
            } else {
                ratatui::style::Style::default()
            };
            if app.hover == Some(HoverTarget::Runtime(*index)) {
                style = style.patch(theme.hovered);
            }
            let active_row = app.selected_runtime == Some(*index)
                || app.hover == Some(HoverTarget::Runtime(*index));
            let identity_text = format!("{}  {}", identity.engine_id, identity.version);
            let marker = format!("{}  ", glyphs.running);
            let identity_width = runtime_identity_width(row.width, &marker, compatibility);
            let identity_text = if active_row {
                marquee_text(
                    &identity_text,
                    identity_width,
                    app.marquee_animation_frame / 3,
                )
            } else {
                truncate_middle(&identity_text, identity_width, glyphs.ellipsis)
            };
            let (metadata, selected, update) = runtime_secondary_text(app, status, row.width);
            let stationary_state = format!("{selected}{update}");
            let metadata_width = runtime_metadata_width(row.width, &stationary_state);
            let metadata = if active_row {
                marquee_text(&metadata, metadata_width, app.marquee_animation_frame / 3)
            } else {
                truncate_middle(&metadata, metadata_width, glyphs.ellipsis)
            };
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(marker, theme.success),
                    Span::styled(identity_text, theme.text),
                    Span::styled(format!("  {compatibility}"), compatibility_style),
                ]),
                Line::from(vec![
                    Span::styled(metadata, theme.muted),
                    Span::styled(selected, theme.hint),
                    Span::styled(update, theme.warning),
                ]),
            ];
            if ui_layout.runtime_row_height > 2 {
                lines.push(Line::default());
            }
            ListItem::new(lines).style(style)
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
    if ui_layout.runtime_actions.height > 1 {
        let detail = runtime_policy_detail(app, status);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                marquee_text(
                    &detail,
                    ui_layout.runtime_actions.width as usize,
                    app.marquee_animation_frame / 3,
                ),
                theme.hint,
            ))),
            Rect::new(
                ui_layout.runtime_actions.x,
                ui_layout.runtime_actions.y + 1,
                ui_layout.runtime_actions.width,
                1,
            ),
        );
    }
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

pub(super) fn runtime_policy_detail(app: &App, status: &InstalledRuntimeStatus) -> String {
    let runtime_id = &status.runtime.manifest.runtime_id;
    let policy = app
        .runtime_list
        .as_ref()
        .and_then(|snapshot| snapshot.selections.update_preferences.get(runtime_id));
    let policy = match policy {
        Some(RuntimeUpdatePreference::Pinned) => "Pinned",
        Some(RuntimeUpdatePreference::Stable) => "Stable",
        Some(RuntimeUpdatePreference::Latest) => "Latest",
        None => "Latest",
    };
    let update = app
        .runtime_updates
        .get(runtime_id)
        .map_or_else(|| "  unchecked".to_owned(), runtime_update_text);
    format!("UPDATE POLICY  {policy}    STATUS{update}")
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
        } => format!("  update: {version}"),
        RuntimeUpdateState::Pinned { .. } => "  current".to_owned(),
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

pub(super) fn installed_model_name_width(row_width: u16, marker: &str, format_span: &str) -> usize {
    let preferred = row_width.saturating_sub(3) as usize * 3 / 5;
    preferred.min(remaining_width(row_width, &[marker, format_span]))
}

pub(super) fn available_format_span(format: &str) -> String {
    format!("{}  ", format.to_ascii_uppercase())
}

pub(super) fn runtime_identity_width(row_width: u16, marker: &str, compatibility: &str) -> usize {
    let suffix = format!("  {compatibility}");
    remaining_width(row_width, &[marker, &suffix])
}

pub(super) fn split_runtime_metadata(
    row_width: u16,
    metadata: String,
    selected: String,
    update: String,
) -> (String, String, String) {
    if UnicodeWidthStr::width(format!("{selected}{update}").as_str()) <= row_width as usize / 2 {
        (metadata, selected, update)
    } else {
        (
            format!("{metadata}{selected}{update}"),
            String::new(),
            String::new(),
        )
    }
}

pub(super) fn runtime_secondary_text(
    app: &App,
    status: &InstalledRuntimeStatus,
    row_width: u16,
) -> (String, String, String) {
    let manifest = &status.runtime.manifest;
    let identity = &manifest.identity;
    let formats = manifest
        .supported_formats
        .iter()
        .map(|format| format.as_str().to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join("/");
    let source_build = manifest
        .source_build
        .as_ref()
        .map_or_else(String::new, |build| {
            let builder = match build.build_system {
                RuntimeSourceBuildSystem::Cmake => {
                    format!("CMake {}", build.toolchain.cmake_version)
                }
                RuntimeSourceBuildSystem::Make => format!("Make {}", build.toolchain.make_version),
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
    let metadata = format!(
        "{formats}  {} / {}{source_build}",
        identity.accelerator, identity.variant
    );
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
    let badge = if status.latest_installed {
        "  LATEST INSTALLED"
    } else {
        ""
    };
    let (metadata, selected, update) = split_runtime_metadata(
        row_width.saturating_sub(badge.len() as u16),
        metadata,
        selected,
        update,
    );
    (metadata, format!("{badge}{selected}"), update)
}

pub(super) fn runtime_metadata_width(row_width: u16, stationary_state: &str) -> usize {
    remaining_width(row_width, &[stationary_state])
}

pub(crate) fn compatibility_label<'a>(
    compatibility: &'a RuntimeCompatibility,
    theme: &'a Theme,
) -> (&'a str, ratatui::style::Style) {
    let style = match compatibility {
        RuntimeCompatibility::Recommended => theme.success,
        RuntimeCompatibility::Compatible => theme.accent,
        RuntimeCompatibility::NeedsAttention(_) => theme.warning,
        RuntimeCompatibility::Incompatible(_) => theme.error,
    };
    (compatibility_text(compatibility), style)
}

pub(super) fn compatibility_text(compatibility: &RuntimeCompatibility) -> &str {
    match compatibility {
        RuntimeCompatibility::Recommended => "recommended",
        RuntimeCompatibility::Compatible => "compatible",
        RuntimeCompatibility::NeedsAttention(_) => "needs attention",
        RuntimeCompatibility::Incompatible(_) => "incompatible",
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
    let layout = content_layout(area, ui_layout.compact);
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
    let accelerators = app
        .control
        .as_ref()
        .map(|control| {
            let value = control
                .backends
                .iter()
                .filter_map(|backend| {
                    backend.accelerator_binding.as_ref().map(|binding| {
                        let devices = binding
                            .devices
                            .iter()
                            .enumerate()
                            .map(|(index, device)| {
                                format!(
                                    "[{index}] {}",
                                    device.stable_id.as_deref().unwrap_or("unknown-id")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{}: {devices}", backend.model_profile_id)
                    })
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
    let configured_auth = auth.configured_mode.to_string();
    let effective_auth = auth.effective_mode.to_string();
    let value_width = key_value_width(ui_layout.server_details.width);
    let state = truncate_middle(app.snapshot.server.label(), value_width, glyphs.ellipsis);
    let exposure = truncate_middle(exposure, value_width, glyphs.ellipsis);
    let configured_auth = truncate_middle(&configured_auth, value_width, glyphs.ellipsis);
    let effective_auth = truncate_middle(&effective_auth, value_width, glyphs.ellipsis);
    let active_key_count = truncate_middle(&active_key_count, value_width, glyphs.ellipsis);
    let lifecycle = truncate_middle(&lifecycle, value_width, glyphs.ellipsis);
    let endpoint = marquee_text(endpoint, value_width, app.marquee_animation_frame / 3);
    let active_profile = marquee_text(
        &active_profile,
        value_width,
        app.marquee_animation_frame / 3,
    );
    let bind = marquee_text(&auth.bind, value_width, app.marquee_animation_frame / 3);
    let active_model = marquee_text(&active_model, value_width, app.marquee_animation_frame / 3);
    let active_engine = marquee_text(&active_engine, value_width, app.marquee_animation_frame / 3);
    let active_runtime = marquee_text(
        &active_runtime,
        value_width,
        app.marquee_animation_frame / 3,
    );
    let accelerators = marquee_text(&accelerators, value_width, app.marquee_animation_frame / 3);
    let private_backend = marquee_text(
        &private_backend,
        value_width,
        app.marquee_animation_frame / 3,
    );
    let security = if auth.insecure_remote {
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
    };
    let lines = if ui_layout.server_details.height < 20 {
        vec![
            key_value("STATE", &state, theme),
            key_value("ENDPOINT", &endpoint, theme),
            key_value("PROFILES", &active_profile, theme),
            key_value("EXPOSURE", &exposure, theme),
            key_value("AUTH EFFECTIVE", &effective_auth, theme),
            key_value("RESIDENCY", &lifecycle, theme),
            key_value("MODELS", &active_model, theme),
            key_value("ENGINES", &active_engine, theme),
            key_value("RUNTIMES", &active_runtime, theme),
            key_value("GPUS", &accelerators, theme),
            security,
            Line::from(Span::styled("Available now", theme.text)),
            Line::from(Span::styled(
                "GET  /health    GET  /v1/models",
                theme.accent,
            )),
            Line::from(Span::styled("POST /v1/responses", theme.accent)),
            Line::from(Span::styled("POST /v1/chat/completions", theme.accent)),
        ]
    } else {
        vec![
            key_value("STATE", &state, theme),
            key_value("ENDPOINT", &endpoint, theme),
            key_value("PROFILES", &active_profile, theme),
            key_value("PUBLIC BIND", &bind, theme),
            key_value("EXPOSURE", &exposure, theme),
            key_value("AUTH CONFIGURED", &configured_auth, theme),
            key_value("AUTH EFFECTIVE", &effective_auth, theme),
            key_value("ACTIVE API KEYS", &active_key_count, theme),
            key_value("RESIDENCY", &lifecycle, theme),
            key_value("MODELS", &active_model, theme),
            key_value("ENGINES", &active_engine, theme),
            key_value("RUNTIMES", &active_runtime, theme),
            key_value("GPUS", &accelerators, theme),
            key_value("PRIVATE ENDPOINTS", &private_backend, theme),
            security,
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
        ]
    };
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }),
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
    let layout = content_layout(area, ui_layout.compact);
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
    let layout = content_layout(area, ui_layout.compact);
    frame.render_widget(
        section_title(
            "Settings",
            "Server Settings and runtime defaults; Model Profiles are edited on their own screen",
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
            crate::app::SettingsScope::Server => "Server".to_owned(),
            crate::app::SettingsScope::Runtime(engine) => match engine.as_str() {
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
    let info_y = ui_layout.settings_scopes.y.saturating_add(1);
    frame.render_widget(
        Paragraph::new(info).style(theme.hint),
        Rect::new(
            ui_layout.settings_scopes.x,
            info_y,
            ui_layout.settings_scopes.width,
            ui_layout.settings_list.y.saturating_sub(info_y),
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
    let layout = content_layout(area, ui_layout.compact);
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
        frame.render_widget(
            Paragraph::new(truncate_middle(&label, rect.width as usize, "…")).style(style),
            *rect,
        );
    }
    let info_area = Rect::new(
        ui_layout.settings_scopes.x,
        ui_layout.settings_scopes.y.saturating_add(1),
        ui_layout.settings_scopes.width,
        if ui_layout.compact { 5 } else { 6 },
    );
    let info = if app.settings_input.is_some() {
        Vec::new()
    } else if let Some(profile) = app.selected_model_profile_value() {
        let model = app.selected_profile_model();
        let model_path = model
            .map(|model| model.path.display().to_string())
            .unwrap_or_default();
        let model_path = marquee_text(
            &model_path,
            info_area.width as usize,
            app.marquee_animation_frame / 3,
        );
        let definitions = app.settings_definitions();
        let selected_definition = definitions.get(app.settings_setting_index);
        let profile_id = format!(
            "{}{}",
            profile.id,
            if ui_layout.compact { " · " } else { "  " }
        );
        let engine = if ui_layout.compact {
            format!("{} · ", profile.engine_id)
        } else {
            format!("engine {}  ", profile.engine_id)
        };
        let model_name = model
            .map(|model| model.display_name.clone())
            .unwrap_or_else(|| format!("MISSING {}", profile.model_id));
        let model_name_width = remaining_width(info_area.width, &[&profile_id, &engine]);
        let identity = if model_name_width == 0 {
            Line::from(Span::styled(
                truncate_middle(
                    &format!("{profile_id}{engine}{model_name}"),
                    info_area.width as usize,
                    "…",
                ),
                if model.is_some() {
                    theme.text
                } else {
                    theme.warning
                },
            ))
        } else {
            Line::from(vec![
                Span::styled(profile_id, theme.text),
                Span::styled(engine, theme.accent),
                Span::styled(
                    truncate_middle(&model_name, model_name_width, "…"),
                    if model.is_some() {
                        theme.hint
                    } else {
                        theme.warning
                    },
                ),
            ])
        };
        let mut lines = vec![
            identity,
            Line::from(Span::styled(model_path, theme.muted)),
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
            lines.extend(
                app.settings_default_detail(&definition.id)
                    .lines()
                    .map(|detail| Line::from(Span::styled(detail.to_owned(), theme.muted))),
            );
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
    if app.selected_model_profile_value().is_none() {
        return;
    }
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
        marked_input_window(&input.text, input.cursor, field_width, "_")
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
        let message = match app.selected_settings_scope() {
            Some(crate::app::SettingsScope::Runtime(engine))
                if !app.runtime_settings_schemas.contains_key(&engine) =>
            {
                format!(
                    "Exact runtime settings for `{engine}` are unavailable. Select or install a compatible exact runtime, then refresh Settings."
                )
            }
            _ => "No settings are available in this scope.".to_owned(),
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(theme.warning)
                .wrap(Wrap { trim: true }),
            ui_layout.settings_list,
        );
        return;
    }
    for (index, rect) in &ui_layout.settings_rows {
        let Some(definition) = definitions.get(*index) else {
            continue;
        };
        let display = app.settings_value_display(&definition.id);
        let mut style = if app.settings_setting_index == *index {
            theme.selected
        } else {
            ratatui::style::Style::default()
        };
        if app.hover == Some(HoverTarget::Setting(*index)) {
            style = style.patch(theme.hovered);
        }
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
        let status = format!("{label} · {}", display.source);
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
        let active_row =
            app.settings_setting_index == *index || app.hover == Some(HoverTarget::Setting(*index));
        let setting_id = definition.id.to_string();
        let id_text = if active_row {
            marquee_text(&setting_id, id_width, app.marquee_animation_frame / 3)
        } else {
            truncate_middle(&setting_id, id_width, "…")
        };
        let lines = vec![
            Line::from(Span::styled(
                heading,
                if starts_category {
                    theme.hint
                } else {
                    theme.muted
                },
            )),
            Line::from(vec![Span::styled(format!("  {id_text}"), theme.text)]),
        ];
        frame.render_widget(Paragraph::new(lines).style(style), *rect);
        let value_enabled = !app.settings_busy();
        let value_label = format!("[ {} ]", display.value);
        let value_text = if active_row || app.hover == Some(HoverTarget::SettingValue(*index)) {
            marquee_text(
                &value_label,
                value_area.width as usize,
                app.marquee_animation_frame / 3,
            )
        } else {
            truncate_middle(&value_label, value_area.width as usize, "…")
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                value_text,
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

fn render_help_content(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    glyphs: &Glyphs,
    ui_layout: &UiLayout,
) {
    let layout = content_layout(area, ui_layout.compact);
    frame.render_widget(
        section_title("Help", "Navigate directly or use slash commands", theme),
        layout[0],
    );
    render_help_body(frame, layout[1], theme, glyphs, ui_layout.compact);
}

pub fn render_help_body(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    glyphs: &Glyphs,
    compact: bool,
) {
    if compact {
        frame.render_widget(Paragraph::new(compact_help_lines(theme, glyphs)), area);
        return;
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .spacing(4)
        .split(area);
    if area.height < 24 || area.width < 110 {
        let (left, right) = concise_help_columns(theme, glyphs);
        frame.render_widget(Paragraph::new(left), columns[0]);
        frame.render_widget(Paragraph::new(right), columns[1]);
    } else {
        let lines = help_lines(theme, glyphs);
        let split = 17.min(lines.len());
        frame.render_widget(Paragraph::new(lines[..split].to_vec()), columns[0]);
        frame.render_widget(Paragraph::new(lines[split..].to_vec()), columns[1]);
    }
}

fn concise_help_columns<'a>(theme: &Theme, glyphs: &Glyphs) -> (Vec<Line<'a>>, Vec<Line<'a>>) {
    let left = vec![
        Line::from(Span::styled("NAVIGATION", theme.hint)),
        key_value("Tab / Shift+Tab", "change focus", theme),
        key_value("Left / Right", "move navigation focus", theme),
        key_value("Enter", "open or activate", theme),
        key_value("Mouse", "select rows / actions", theme),
        Line::default(),
        Line::from(Span::styled("MODELS AND PROFILES", theme.hint)),
        key_value("Models", "Installed / Discover", theme),
        key_value("Left / Right", "change library view", theme),
        key_value("e / f / d", "search, filter, download", theme),
        key_value("p / x", "pause/resume or cancel download", theme),
        key_value("Model Profiles", "serving targets", theme),
        key_value("l/u/e/Delete", "load, unload, bind, inherit", theme),
    ];
    let right = vec![
        Line::from(Span::styled("SETTINGS AND RUNTIMES", theme.hint)),
        key_value("Settings", "Server Settings and runtime defaults", theme),
        key_value(glyphs.up_down, "select or scroll", theme),
        key_value("s", "search available runtimes", theme),
        key_value("g / Q / N", "set format default", theme),
        key_value("u / Shift+U", "check / install update", theme),
        key_value("d twice", "confirm runtime removal", theme),
        Line::default(),
        Line::from(Span::styled("COMMANDS", theme.hint)),
        key_value("/", "open suggestions", theme),
        key_value(glyphs.up_down, "select a suggestion", theme),
        key_value("Enter / Esc", "run / cancel", theme),
        key_value("? / Ctrl+C", "help / exit", theme),
        Line::from(Span::styled("/status /models /runtimes /help", theme.muted)),
    ];
    (left, right)
}

fn compact_help_lines<'a>(theme: &Theme, glyphs: &Glyphs) -> Vec<Line<'a>> {
    vec![
        Line::from(Span::styled("ESSENTIAL KEYS", theme.hint)),
        key_value("Tab / arrows", "move focus and selection", theme),
        key_value("Enter", "open or activate", theme),
        key_value("Models", "e search, f format, d download", theme),
        key_value("p / x", "pause/resume or cancel download", theme),
        key_value("Runtimes", "s search, g/Q/N default", theme),
        key_value("Settings", "Enter edit, Delete inherit", theme),
        key_value("/", "commands", theme),
        key_value("? / Ctrl+C", "help / exit", theme),
        key_value(glyphs.up_down, "select or scroll", theme),
    ]
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
        key_value("Shift+Up/Down", "select a visible download card", theme),
        key_value("p / x", "pause/resume or cancel that download", theme),
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
        key_value("Settings", "Server Settings and runtime defaults", theme),
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
