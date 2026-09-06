use norted_core::{
    ArtifactFormat, RegistryState, RuntimeCompatibility, RuntimeUpdatePreference,
    RuntimeUpdateState,
};
use norted_engine::{
    BackendLifecycle, BackendParallelism, BackendStatus, InferenceActivity, InferenceActivityPhase,
    InstalledRuntimeStatus,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, FocusArea, ModelLibraryView, Screen};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{
    ActionState, action_style, content_layout, format_bytes, inventory_columns, inventory_row,
    key_value, load_progress_compact, marked_input_window, marquee_text, model_content_layout,
    render_empty, render_load_progress, section_title, truncate_middle,
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
    let mut title = content_layout(area, ui_layout.compact)[0];
    if area.height < 10 {
        title.height = 2;
    }
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
    lines.push(Line::from(Span::styled(
        truncate_middle(
            &format!(
                "Profile: {profile_name} | {:?} / {:?}",
                backend.role, backend.residency
            ),
            inner.width as usize,
            glyphs.ellipsis,
        ),
        theme.text,
    )));
    if !ui_layout.compact {
        lines.push(Line::from(Span::styled(
            truncate_middle(
                &format!("Runtime: {}", backend_runtime_label(backend)),
                inner.width as usize,
                glyphs.ellipsis,
            ),
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
        BackendLifecycle::Running if backend.active_request_count == 0 => {
            "RUNNING | Idle | 0 requests".to_owned()
        }
        BackendLifecycle::Running => {
            if backend.active_request_count == 1 && backend.activities.len() == 1 {
                format_activity_state(&backend.activities[0])
            } else if backend.active_request_count == 1 {
                "RUNNING | 1 active request".to_owned()
            } else {
                format!("RUNNING | {} requests", backend.active_request_count)
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
    if let Some(version) = &backend.runtime_version {
        parts.push(version.clone());
    }
    if let Some(variant) = &backend.runtime_variant {
        parts.push(variant.clone());
    }
    if parts.is_empty() {
        "Runtime resolving".to_owned()
    } else {
        parts.join(" / ")
    }
}

fn format_activity(activity: &InferenceActivity) -> String {
    format!("{}  {}", activity.id, format_activity_state(activity))
}

fn format_activity_state(activity: &InferenceActivity) -> String {
    match activity.phase {
        InferenceActivityPhase::Active => "RUNNING | 1 active request".to_owned(),
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
        let detail = if app.model_filter_editing || !app.model_filter.is_empty() {
            format!(
                "Filter: {}{} | Esc: {}",
                app.model_filter,
                if app.model_filter_editing { "_" } else { "" },
                if app.model_filter_editing {
                    "finish"
                } else {
                    "clear"
                }
            )
        } else {
            match &app.snapshot.registry_state {
                RegistryState::NotScanned => "Local discovery is preparing".to_owned(),
                RegistryState::Scanning => "Scanning configured external model paths".to_owned(),
                RegistryState::Failed { .. } => "Local discovery failed; see Logs".to_owned(),
                RegistryState::Ready if app.snapshot.registry_warnings.is_empty() => {
                    "f: filter | J: downloads | D: details".to_owned()
                }
                RegistryState::Ready | RegistryState::ReadyWithWarnings { .. } => format!(
                    "f: filter | J: jobs | {} warning(s): Logs",
                    app.snapshot.registry_warnings.len()
                ),
            }
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
    let layout = model_content_layout(area, ui_layout.compact);
    if app.downloads_focused {
        frame.render_widget(
            Paragraph::new("Downloads").style(theme.accent),
            Rect { height: 1, ..area },
        );
        render_model_downloads(frame, app, theme, glyphs, ui_layout);
        return;
    }
    render_model_header(frame, app, theme, glyphs, ui_layout, layout[0]);
    if app.model_library_view == ModelLibraryView::Discover {
        render_model_discover(frame, app, theme, glyphs, ui_layout);
        render_model_downloads(frame, app, theme, glyphs, ui_layout);
        return;
    }
    if app.snapshot.models.is_empty()
        && matches!(app.snapshot.registry_state, RegistryState::NotScanned)
    {
        render_empty(
            frame,
            layout[1],
            &format!("{}  Preparing model discovery", glyphs.transitional),
            "The registry has not been scanned yet; background discovery starts after the first frame.",
            theme,
        );
        return;
    }
    if app.snapshot.models.is_empty()
        && matches!(app.snapshot.registry_state, RegistryState::Scanning)
    {
        render_empty(
            frame,
            layout[1],
            &format!("{}  Discovering local models", glyphs.transitional),
            "The registry will update automatically. You can keep using the interface while it scans.",
            theme,
        );
        return;
    }
    if app.snapshot.models.is_empty()
        && let RegistryState::Failed { message } = &app.snapshot.registry_state
    {
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
    let columns = inventory_columns(ui_layout.model_list, &[10, 10, 14], &[7, 9]);
    let wide = ui_layout.model_list.width >= 85;
    inventory_row(
        frame,
        ui_layout.inventory_header,
        &columns,
        &if wide {
            vec!["Model", "Format", "Size", "Usage"]
        } else {
            vec!["Model", "Format", "Usage"]
        }
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
        theme.hint,
        glyphs,
    );
    for (index, row) in &ui_layout.model_rows {
        let model = &app.snapshot.models[*index];
        let selected = app.selected_model == Some(*index);
        let usage =
            app.control
                .as_ref()
                .and_then(|control| {
                    control.backends.iter().find(|b| {
                        b.model_id == model.id && b.lifecycle != BackendLifecycle::Stopped
                    })
                })
                .map_or_else(|| "Installed".to_owned(), |b| format!("{:?}", b.lifecycle));
        let mut values = vec![
            format!(
                "{} {}",
                if selected { ">" } else { " " },
                model.display_name
            ),
            model.format.to_string(),
        ];
        if wide {
            values.push(format_bytes(model.size_bytes));
        }
        values.push(usage);
        let mut style = if selected { theme.selected } else { theme.text };
        if app.hover == Some(HoverTarget::Model(*index)) {
            style = style.patch(theme.hovered);
        }
        inventory_row(frame, *row, &columns, &values, style, glyphs);
    }
    if app.installed_model_indices().is_empty() {
        render_empty(
            frame,
            ui_layout.model_list,
            "No matching local models",
            "f: edit filter | Esc: clear filter",
            theme,
        );
    }
    render_inventory_detail(frame, app, theme, ui_layout);
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

fn discovered_state(
    app: &App,
    repository: &norted_model_library::CatalogRepository,
    artifact: &norted_model_library::CatalogFile,
) -> String {
    if let Some(job) = app
        .model_download_jobs
        .iter()
        .find(|job| job.model_ref == artifact.model_ref && !job.is_terminal())
    {
        return format!("{:?}", job.phase);
    }
    if app
        .snapshot
        .models
        .iter()
        .filter_map(|m| m.provenance.as_ref())
        .any(|p| {
            p.provider == repository.provider
                && p.repository.as_deref() == Some(repository.repository.as_str())
                && p.revision.as_deref() == Some(repository.revision.as_str())
                && p.remote_filename.as_deref() == Some(artifact.filename.as_str())
        })
    {
        "Installed".to_owned()
    } else {
        "Candidate".to_owned()
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

    if app.model_search_loading && app.model_search.is_none() {
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
        let table = Rect {
            width: ui_layout.model_list.width.saturating_sub(13),
            ..ui_layout.model_list
        };
        let wide = table.width >= 70;
        let columns = inventory_columns(
            table,
            &[18, 7, 9, 10],
            if wide { &[18, 7, 9, 10] } else { &[7] },
        );
        let headings = if wide {
            vec!["Artifact", "Repository", "Format", "Size", "State"]
        } else {
            vec!["Artifact", "Format"]
        };
        inventory_row(
            frame,
            ui_layout.inventory_header,
            &columns,
            &headings.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            theme.hint,
            glyphs,
        );
        for (index, row) in &ui_layout.model_rows {
            let (repository, artifact) = artifacts[*index];
            let selected = app.selected_model_search_result == Some(*index);
            let name = format!("{} {}", if selected { ">" } else { " " }, artifact.filename);
            let values = if wide {
                vec![
                    name,
                    repository.repository.clone(),
                    artifact.format.to_string(),
                    artifact
                        .size_bytes
                        .map(format_bytes)
                        .unwrap_or_else(|| "Unknown".to_owned()),
                    discovered_state(app, repository, artifact),
                ]
            } else {
                vec![name, artifact.format.to_string()]
            };
            let mut style = if selected { theme.selected } else { theme.text };
            if app.hover == Some(HoverTarget::Model(*index)) {
                style = style.patch(theme.hovered);
            }
            inventory_row(frame, *row, &columns, &values, style, glyphs);
        }
        render_inventory_detail(frame, app, theme, ui_layout);
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
    let prefix = format!("{} {} ", if selected { ">" } else { " " }, glyphs.download);
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
        (Some(repository), Some(filename)) => format!("{filename}  {repository}"),
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
        || format!("{} / unknown", format_bytes(job.downloaded_bytes)),
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
            let mut details = vec!["Downloading".to_owned(), transferred];
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
    let subtitle = if app.runtime_list_error.is_some() {
        if app.runtime_list.is_some() {
            "Refresh failed | cached rows | D: details"
        } else {
            "Scan failed | r: retry | D: details"
        }
    } else if app.runtime_list_loading {
        "Inspecting local runtimes in the background"
    } else {
        "Installed packs, persisted format defaults, and upstream runtimes"
    };
    frame.render_widget(section_title("Runtimes", subtitle, theme), layout[0]);

    let summary = [
        ArtifactFormat::Gguf,
        ArtifactFormat::Q27,
        ArtifactFormat::Ninfer,
    ]
    .into_iter()
    .map(|format| {
        Line::from(vec![
            Span::styled(format!("{:<8} default  ", format.as_str()), theme.hint),
            Span::styled(selection_text(app, format, ui_layout.compact), theme.text),
        ])
    })
    .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(summary), ui_layout.runtime_summary);
    if app.runtime_list_loading && app.runtime_list.is_none() {
        render_empty(
            frame,
            ui_layout.runtime_list,
            &format!("{}  Inspecting installed runtimes", glyphs.transitional),
            "Local pack manifests and configured external runtimes are being probed.",
            theme,
        );
    } else if let Some(error) = app
        .runtime_list_error
        .as_ref()
        .filter(|_| app.runtime_list.is_none())
    {
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
        let columns = inventory_columns(ui_layout.runtime_list, &[10, 14, 15, 12, 12], &[12, 12]);
        let wide = ui_layout.runtime_list.width >= 85;
        let headings = if wide {
            vec![
                "Engine",
                "Version",
                "Variant",
                "Compatibility",
                "State",
                "Update",
            ]
        } else {
            vec!["Engine / version", "Fit", "State"]
        };
        inventory_row(
            frame,
            ui_layout.inventory_header,
            &columns,
            &headings.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            theme.hint,
            glyphs,
        );
        for (index, row) in &ui_layout.runtime_rows {
            let status = &snapshot.installed[*index];
            let manifest = &status.runtime.manifest;
            let identity = &manifest.identity;
            let selected = app.selected_runtime == Some(*index);
            let in_use = app.control.as_ref().is_some_and(|c| {
                c.backends.iter().any(|b| {
                    b.runtime_id.as_ref() == Some(&manifest.runtime_id)
                        && b.lifecycle != BackendLifecycle::Stopped
                })
            });
            let defaults = snapshot
                .selections
                .format_defaults
                .values()
                .any(|id| id == &manifest.runtime_id);
            let bound = snapshot
                .selections
                .model_overrides
                .values()
                .any(|id| id == &manifest.runtime_id);
            let state = [
                if in_use { "In use" } else { "" },
                if defaults { "Default" } else { "" },
                if bound { "Model" } else { "" },
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" / ");
            let state = if state.is_empty() {
                "Installed".to_owned()
            } else {
                state
            };
            let name = format!(
                "{} {}",
                if selected { ">" } else { " " },
                identity.engine_id
            );
            let values = if wide {
                vec![
                    name,
                    identity.version.clone(),
                    format!("{} / {}", identity.accelerator, identity.variant),
                    compatibility_text(&status.compatibility).to_owned(),
                    state,
                    app.runtime_updates
                        .get(&manifest.runtime_id)
                        .map_or_else(|| "Unchecked".to_owned(), runtime_update_text),
                ]
            } else {
                vec![
                    format!("{name} {}", identity.version),
                    compatibility_text(&status.compatibility).to_owned(),
                    state,
                ]
            };
            let mut style = if selected {
                theme.selected
            } else {
                compatibility_label(&status.compatibility, theme).1
            };
            if app.hover == Some(HoverTarget::Runtime(*index)) {
                style = style.patch(theme.hovered);
            }
            inventory_row(frame, *row, &columns, &values, style, glyphs);
        }
        render_inventory_detail(frame, app, theme, ui_layout);
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
    if ui_layout.runtime_actions.height > 2 {
        let detail = format!(
            "Selected: {} {} | D: identity / update policy",
            status.runtime.manifest.identity.engine_id, status.runtime.manifest.identity.version
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_middle(
                    &detail,
                    ui_layout.runtime_actions.width as usize,
                    Glyphs::current(app.unicode).ellipsis,
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
        } => format!("Pinned: {version}"),
        RuntimeUpdateState::Pinned { .. } => "Pinned".to_owned(),
        RuntimeUpdateState::CatalogUnavailable(_) => "  catalog unavailable".to_owned(),
        RuntimeUpdateState::ProviderError(_) => "Check failed".to_owned(),
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
    let auth = &app.public_auth_status;
    let endpoint = app
        .control
        .as_ref()
        .and_then(|c| c.public_endpoint.as_deref())
        .or_else(|| app.snapshot.server.endpoint())
        .unwrap_or("Not observed");
    let mut lines = vec![];
    if auth.insecure_remote {
        lines.push(Line::from(Span::styled(
            "WARNING: remote authentication is disabled",
            theme.error,
        )));
    }
    if let Some(error) = &app.public_auth_error {
        lines.push(Line::from(Span::styled(
            format!("Auth unavailable: {error}"),
            theme.error,
        )));
    }
    lines.extend([
        Line::from(vec![
            Span::styled("PUBLIC  ", theme.hint),
            Span::styled(
                format!("{} | {endpoint}", app.snapshot.server.label()),
                theme.text,
            ),
        ]),
        Line::from(vec![
            Span::styled("EXPOSURE  ", theme.hint),
            Span::styled(
                format!(
                    "{} | Auth: {}",
                    if auth.loopback { "Loopback" } else { "Remote" },
                    auth.effective_mode
                ),
                theme.text,
            ),
        ]),
    ]);
    if ui_layout.server_details.height >= 10 {
        lines.push(Line::from(format!(
            "CONFIGURED  {} | Auth: {} | Keys: {}",
            auth.bind,
            auth.configured_mode,
            if app.public_auth_loading {
                "Loading".to_owned()
            } else if app.public_auth_error.is_some() {
                "Unavailable".to_owned()
            } else {
                auth.active_key_count.to_string()
            }
        )));
        lines.push(Line::from(Span::styled("API ROUTES", theme.hint)));
        lines.push(Line::from(
            "GET /health   GET /v1/models   GET /v1/models/{model}",
        ));
        lines.push(Line::from("POST /v1/responses   POST /v1/chat/completions"));
        lines.push(Line::from("POST /v1/completions   POST /v1/embeddings"));
    }
    let used = lines.len() as u16;
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            height: used.min(ui_layout.server_details.height),
            ..ui_layout.server_details
        },
    );
    let table = Rect::new(
        ui_layout.server_details.x,
        ui_layout.server_details.y + used,
        ui_layout.server_details.width,
        ui_layout.server_details.height.saturating_sub(used),
    );
    let columns = inventory_columns(table, &[18, 14, 8, 22], &[12, 5]);
    let wide = table.width >= 85;
    let headings = if wide {
        vec![
            "Resident profile",
            "Engine / version",
            "Lifecycle",
            "Requests",
            "Private endpoint",
        ]
    } else {
        vec!["Resident profile", "Lifecycle", "Req"]
    };
    inventory_row(
        frame,
        table,
        &columns,
        &headings.into_iter().map(str::to_owned).collect::<Vec<_>>(),
        theme.hint,
        glyphs,
    );
    if let Some(control) = &app.control {
        for (index, backend) in control
            .backends
            .iter()
            .take(table.height.saturating_sub(1) as usize)
            .enumerate()
        {
            let values = if wide {
                vec![
                    backend.model_profile_id.to_string(),
                    format!(
                        "{} / {}",
                        backend.engine_id.as_deref().unwrap_or("Unknown"),
                        backend.runtime_version.as_deref().unwrap_or("Unknown")
                    ),
                    format!("{:?}", backend.lifecycle),
                    backend.active_request_count.to_string(),
                    backend
                        .private_endpoint
                        .clone()
                        .unwrap_or_else(|| "Not observed".to_owned()),
                ]
            } else {
                vec![
                    backend.model_profile_id.to_string(),
                    format!("{:?}", backend.lifecycle),
                    backend.active_request_count.to_string(),
                ]
            };
            inventory_row(
                frame,
                Rect::new(table.x, table.y + 1 + index as u16, table.width, 1),
                &columns,
                &values,
                theme.text,
                glyphs,
            );
        }
    }

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
        section_title(
            "Logs",
            if app.log_scroll == 0 {
                "Following latest | D: complete messages"
            } else {
                "History paused | End: follow | D: messages"
            },
            theme,
        ),
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
            Span::styled(
                {
                    let mut lines = entry.message.lines();
                    let first = lines.next().unwrap_or_default();
                    let remaining = lines.count();
                    let message = if remaining > 0 {
                        format!("{first} [D: +{remaining} lines]")
                    } else {
                        first.to_owned()
                    };
                    truncate_middle(
                        &message,
                        layout[1].width.saturating_sub(6) as usize,
                        Glyphs::current(app.unicode).ellipsis,
                    )
                },
                theme.text,
            ),
        ])
    });
    frame.render_widget(Paragraph::new(lines.collect::<Vec<_>>()), layout[1]);
}

fn runtime_summary(app: &App, id: &norted_core::RuntimeId) -> String {
    app.runtime_list
        .as_ref()
        .and_then(|list| {
            list.installed
                .iter()
                .find(|item| &item.runtime.manifest.runtime_id == id)
        })
        .map(|item| {
            let identity = &item.runtime.manifest.identity;
            format!(
                "{} / {} / {}",
                identity.version,
                identity.accelerator.to_uppercase(),
                identity.variant
            )
        })
        .unwrap_or_else(|| id.to_string())
}

fn render_metadata(frame: &mut Frame<'_>, area: Rect, fields: &[(&str, String)], theme: &Theme) {
    let columns = if fields.len() > area.height as usize {
        2
    } else {
        1
    };
    for (index, (label, value)) in fields.iter().enumerate() {
        let y = area.y + (index / columns) as u16;
        if y >= area.bottom() {
            break;
        }
        let width = area.width / columns as u16;
        let x = area.x + (index % columns) as u16 * width;
        let cells = Layout::horizontal([Constraint::Length(14.min(width / 2)), Constraint::Min(0)])
            .split(Rect::new(x, y, width, 1));
        frame.render_widget(Paragraph::new(*label).style(theme.hint), cells[0]);
        frame.render_widget(
            Paragraph::new(truncate_middle(
                value,
                cells[1].width.saturating_sub(2) as usize,
                "…",
            ))
            .style(theme.text),
            cells[1],
        );
    }
}

fn render_settings(
    frame: &mut Frame<'_>,
    _area: Rect,
    app: &App,
    theme: &Theme,
    ui_layout: &UiLayout,
) {
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
    if app.settings_input.is_none() && ui_layout.settings_list.y >= ui_layout.settings_scopes.y + 4
    {
        let runtime = match app.selected_settings_scope() {
            Some(crate::app::SettingsScope::Runtime(engine)) => app
                .runtime_settings_schemas
                .get(&engine)
                .and_then(|schema| schema.runtime_id.as_ref())
                .map(|id| runtime_summary(app, id))
                .unwrap_or_else(|| "No runtime selected/installed".to_owned()),
            _ => "Server operational defaults".to_owned(),
        };
        render_metadata(
            frame,
            Rect::new(
                ui_layout.settings_scopes.x,
                ui_layout.settings_scopes.y + 1,
                ui_layout.settings_scopes.width,
                2,
            ),
            &[
                ("Baseline", runtime),
                (
                    "Applies",
                    if app.selected_settings_scope() == Some(crate::app::SettingsScope::Server) {
                        "Immediately".to_owned()
                    } else {
                        "Next load".to_owned()
                    },
                ),
            ],
            theme,
        );
    }
    render_setting_rows(frame, app, theme, ui_layout);
    render_settings_input(frame, app, theme, ui_layout);
}

fn render_model_profiles(
    frame: &mut Frame<'_>,
    _area: Rect,
    app: &App,
    theme: &Theme,
    ui_layout: &UiLayout,
) {
    for (index, rect) in &ui_layout.settings_scope_rows {
        let Some(profile) = app.model_profile_values().get(*index).copied() else {
            continue;
        };
        let missing = !app
            .snapshot
            .models
            .iter()
            .any(|model| model.id == profile.model_id);
        let label = if rect.width == 3 {
            if rect.x == ui_layout.settings_scopes.x {
                " ‹ ".to_owned()
            } else {
                " › ".to_owned()
            }
        } else {
            format!(" {} ", profile.id)
        };
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
        3,
    );
    if app.settings_input.is_none()
        && !app.profile_actions_open
        && ui_layout.settings_list.y >= ui_layout.settings_scopes.y + 8
    {
        if let Some(profile) = app.selected_model_profile_value() {
            let model = app.selected_profile_model();
            let runtime = app
                .settings_runtime_id
                .as_ref()
                .map(|id| runtime_summary(app, id))
                .unwrap_or_else(|| "Unresolved / missing runtime".to_owned());
            let fields = [
                ("Profile", profile.id.to_string()),
                ("Role", format!("{:?}", profile.role)),
                (
                    "Model",
                    model
                        .map(|m| m.display_name.clone())
                        .unwrap_or_else(|| format!("Missing {}", profile.model_id)),
                ),
                ("Engine", profile.engine_id.to_string()),
                ("Runtime", runtime),
                (
                    "Configuration",
                    if app.settings_validation_error.is_some() {
                        "Next load · invalid (see details)".to_owned()
                    } else {
                        "Next load".to_owned()
                    },
                ),
            ];
            for (row, indices) in [&[0, 1][..], &[2][..], &[3, 4][..], &[5][..]]
                .iter()
                .enumerate()
            {
                let fields = indices
                    .iter()
                    .map(|index| fields[*index].clone())
                    .collect::<Vec<_>>();
                render_metadata(
                    frame,
                    Rect::new(info_area.x, info_area.y + row as u16, info_area.width, 1),
                    &fields,
                    theme,
                );
            }
        } else {
            frame.render_widget(
                Paragraph::new("No profiles. Create a profile from Models.").style(theme.muted),
                info_area,
            );
        }
    }
    render_model_profile_actions(frame, app, theme, ui_layout);
    if app.selected_model_profile_value().is_none() {
        render_settings_input(frame, app, theme, ui_layout);
        return;
    }
    render_setting_rows(frame, app, theme, ui_layout);
    render_settings_input(frame, app, theme, ui_layout);
}

pub(super) fn render_settings_input(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    ui_layout: &UiLayout,
) {
    let Some(input) = &app.settings_input else {
        return;
    };
    let prompt = if input.kind == crate::app::SettingsInputKind::Reset {
        let scope = if app.screen == crate::app::Screen::ModelProfiles {
            app.selected_model_profile_value()
                .map(|profile| profile.id.to_string())
                .unwrap_or_default()
        } else {
            match app.selected_settings_scope() {
                Some(crate::app::SettingsScope::Server) => "Server".to_owned(),
                Some(crate::app::SettingsScope::Runtime(engine)) => engine,
                Some(crate::app::SettingsScope::ModelProfile(id)) => id.to_string(),
                None => "selected scope".to_owned(),
            }
        };
        format!("Reset {scope}: type RESET")
    } else {
        match input.kind {
            crate::app::SettingsInputKind::Search => "Search settings (empty clears filter)",
            crate::app::SettingsInputKind::Reset => "Reset this scope: type RESET",
            crate::app::SettingsInputKind::ProfileName => "New Model Profile ID",
            crate::app::SettingsInputKind::DuplicateProfile => "Duplicate Model Profile ID",
            crate::app::SettingsInputKind::SettingValue => "Override value",
        }
        .to_owned()
    };
    let panel = if input.editor.is_some() {
        ui_layout.settings_editor_panel
    } else {
        Rect::new(
            ui_layout.settings_scopes.x,
            ui_layout.settings_scopes.y + 1,
            ui_layout.settings_scopes.width,
            ui_layout
                .content
                .bottom()
                .saturating_sub(ui_layout.settings_scopes.y + 1),
        )
    };
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Paragraph::new(truncate_middle(&prompt, panel.width as usize, "…")).style(theme.hint),
        Rect::new(panel.x, panel.y, panel.width, 1),
    );
    if let Some(editor) = &input.editor {
        let scope = match &editor.scope {
            crate::app::SettingsScope::Server => "Server operations".to_owned(),
            crate::app::SettingsScope::Runtime(engine) => format!("Settings / {engine}"),
            crate::app::SettingsScope::ModelProfile(id) => format!("Profile / {id}"),
        };
        let detail: Vec<_> = editor.metadata.lines().collect();
        let parent = detail
            .iter()
            .find(|s| {
                s.starts_with("Settings layer:")
                    && matches!(editor.scope, crate::app::SettingsScope::ModelProfile(_))
            })
            .or_else(|| detail.iter().find(|s| s.starts_with("Parent:")))
            .copied()
            .unwrap_or("Parent: not yet known");
        let local = detail
            .iter()
            .find(|s| {
                s.starts_with(
                    if matches!(editor.scope, crate::app::SettingsScope::ModelProfile(_)) {
                        "Profile layer:"
                    } else {
                        "Settings layer:"
                    },
                )
            })
            .copied()
            .unwrap_or("");
        let timing = if matches!(editor.scope, crate::app::SettingsScope::Server) {
            "Applies now"
        } else {
            "Applies on next load"
        };
        let guidance = match editor.definition.kind {
            norted_core::SettingKind::OneWayFlag => {
                "Inherit removes this flag; the parent might still enable it."
            }
            norted_core::SettingKind::Path => {
                "Path input: structured file paths resolve beneath the server data directory when relative; absolute paths retain existing file validation."
            }
            norted_core::SettingKind::StringList => {
                "PgUp/PgDn item; F2 Add / F3 Edit / F4 Remove / F5 Up / F6 Down; Enter finishes item"
            }
            norted_core::SettingKind::JsonObject => {
                "Multiline JSON object: Enter newline; F10 Save (or Ctrl+Enter)"
            }
            _ => "Up/Down or click selects draft; Enter Save; Esc Cancel",
        };
        let constraints = if editor.definition.kind == norted_core::SettingKind::OneWayFlag {
            "Inherit may still expose an enabled parent".to_owned()
        } else {
            editor.definition.kind.constraints()
        };
        let summary = format!(
            "{} [{}]\n{timing} · {scope}\n{parent}\n{local}\n{} {}",
            editor.definition.label,
            editor.definition.id,
            constraints,
            editor.definition.unit.as_deref().unwrap_or("")
        );
        let full = format!(
            "{}\n{}\n{scope}\n{}\n{}\n{guidance}",
            editor.definition.label,
            editor.definition.id,
            editor.definition.description,
            editor.metadata
        );
        let full = if let Some(error) = &app.settings_input_error {
            format!("Validation: {error}\n{full}")
        } else {
            full
        };
        let info = Paragraph::new(full)
            .style(theme.hint)
            .wrap(Wrap { trim: false });
        let pages = info.line_count(panel.width).div_ceil(5);
        let page = editor.info_page % (pages + 1);
        if page == 0 {
            frame.render_widget(
                Paragraph::new(summary).style(theme.hint),
                Rect::new(panel.x, panel.y, panel.width, 5),
            );
        } else {
            frame.render_widget(
                info.scroll((((page - 1) * 5) as u16, 0)),
                Rect::new(panel.x, panel.y, panel.width, 5),
            );
        }
        frame.render_widget(
            Paragraph::new(
                if editor.custom() && editor.definition.kind == norted_core::SettingKind::JsonObject
                {
                    "Tab mode · F10 save · Esc"
                } else {
                    "↑↓/Tab · Enter save · Esc"
                },
            )
            .style(theme.hint),
            Rect::new(panel.x, panel.y + 5, panel.width.saturating_sub(14), 1),
        );
        for (index, area) in &ui_layout.settings_editor_options {
            let option = &editor.options[*index];
            frame.render_widget(
                Paragraph::new(format!(
                    "({}) {}",
                    if *index == editor.selected {
                        "●"
                    } else {
                        " "
                    },
                    option.label
                ))
                .style(if *index == editor.selected {
                    theme.focused
                } else {
                    theme.hint
                }),
                *area,
            );
        }
    }
    let field_width = ui_layout.settings_input_field.width.saturating_sub(2) as usize;
    let field = if let Some(editor) = &input.editor {
        if editor.custom() {
            if editor.definition.kind == norted_core::SettingKind::StringList
                && !editor.editing_item
            {
                if editor.items.is_empty() {
                    "Empty item list — Add an item or select Inherit".to_owned()
                } else {
                    format!(
                        "Item {}/{}: {:?}",
                        editor.item + 1,
                        editor.items.len(),
                        editor.items[editor.item]
                    )
                }
            } else if editor.definition.kind == norted_core::SettingKind::JsonObject {
                let prefix: String = input.text.chars().take(input.cursor).collect();
                let line = prefix.chars().filter(|c| *c == '\n').count();
                let mut marked = input.text.clone();
                let byte = input
                    .text
                    .char_indices()
                    .nth(input.cursor)
                    .map_or(input.text.len(), |(i, _)| i);
                marked.insert(byte, '▏');
                let column = prefix.rsplit('\n').next().unwrap_or("").chars().count();
                let horizontal = column.saturating_sub(field_width.saturating_sub(1));
                marked
                    .lines()
                    .skip(line.saturating_sub(
                        ui_layout.settings_input_field.height.saturating_sub(1) as usize,
                    ))
                    .take(ui_layout.settings_input_field.height as usize)
                    .map(|line| {
                        line.chars()
                            .skip(horizontal)
                            .take(field_width)
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                format!(
                    "[{}]",
                    marked_input_window(&input.text, input.cursor, field_width, "_")
                )
            }
        } else if editor.options.len() > 8 {
            format!("Type to filter choices: {}_", editor.filter)
        } else {
            String::new()
        }
    } else {
        format!(
            "[{}]",
            marked_input_window(&input.text, input.cursor, field_width, "_")
        )
    };
    for (key, area) in &ui_layout.settings_editor_actions {
        let label = match key {
            1 => "[F1 More info]",
            2 => "[Add]",
            3 if input.editor.as_ref().is_some_and(|e| e.editing_item) => "[Done]",
            3 => "[Edit]",
            4 => "[Remove]",
            5 => "[Up]",
            6 => "[Down]",
            7 => "[Prev]",
            _ => "[Next]",
        };
        frame.render_widget(Paragraph::new(label).style(theme.focused), *area);
    }
    let field_style = if app.hover == Some(HoverTarget::SettingsInputField) {
        theme.focused.patch(theme.hovered)
    } else {
        theme.focused
    };
    frame.render_widget(
        Paragraph::new(field).style(field_style),
        ui_layout.settings_input_field,
    );
    let submit_label = if app.settings_busy() {
        "[ Saving… ]"
    } else {
        match input.kind {
            crate::app::SettingsInputKind::ProfileName => "[ Create ]",
            crate::app::SettingsInputKind::DuplicateProfile => "[ Duplicate ]",
            crate::app::SettingsInputKind::SettingValue => "[F10 Save]",
            crate::app::SettingsInputKind::Search => "[ Search ]",
            crate::app::SettingsInputKind::Reset => "[ Confirm ]",
        }
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
    if let Some(error) = &app.settings_input_error {
        let y = ui_layout.settings_input_submit.y + 1;
        let height = ui_layout.content.bottom().saturating_sub(y);
        frame.render_widget(
            Paragraph::new(error.as_str())
                .style(theme.error)
                .wrap(Wrap { trim: true }),
            Rect::new(
                ui_layout.settings_scopes.x,
                y,
                ui_layout.settings_scopes.width,
                height,
            ),
        );
    }
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

pub(super) fn selected_setting_detail(app: &App) -> Option<String> {
    let definitions = app.settings_definitions();
    let definition = definitions.get(app.settings_setting_index)?;
    Some(format!(
        "Identifier: {}\nDescription: {}\n{}",
        definition.id,
        definition.description,
        app.settings_default_detail(&definition.id)
    ))
}

fn render_setting_rows(frame: &mut Frame<'_>, app: &App, theme: &Theme, ui_layout: &UiLayout) {
    if app.settings_input.is_none()
        && let Some(detail) = selected_setting_detail(app)
    {
        frame.render_widget(Clear, ui_layout.settings_detail);
        frame.render_widget(
            Paragraph::new(detail)
                .scroll((app.settings_detail_scroll, 0))
                .style(theme.hint)
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .title(" Setting · i expand/close · [ ] scroll "),
                ),
            ui_layout.settings_detail,
        );
    }

    for (target, rect) in &ui_layout.settings_tools {
        let label = match target {
            HoverTarget::SettingsDetails => "[ i Details ]",
            HoverTarget::ProfileActions => "[a Actions]",
            HoverTarget::SettingsSearch => "[ / Search ]",
            HoverTarget::SettingsFilter if app.settings_overrides_only => "[ o Overrides ✓ ]",
            HoverTarget::SettingsFilter => "[ o Overrides ]",
            HoverTarget::SettingsReset => "[ R Reset scope ]",
            _ => "",
        };
        let label = if rect.width < 10 {
            match target {
                HoverTarget::SettingsSearch => "[ / ]",
                HoverTarget::SettingsFilter if app.settings_overrides_only => "[ o ✓ ]",
                HoverTarget::SettingsFilter => "[ o ]",
                HoverTarget::SettingsReset => "[ R ]",
                HoverTarget::SettingsDetails => "[ i ]",
                _ => label,
            }
        } else {
            label
        };
        let label = if *target == HoverTarget::SettingsSearch && !app.settings_query.is_empty() {
            truncate_middle(
                &format!("[ / {} ]", app.settings_query),
                rect.width as usize,
                "…",
            )
        } else {
            label.to_owned()
        };
        frame.render_widget(
            Paragraph::new(label).style(if app.hover == Some(*target) {
                theme.hovered
            } else {
                theme.hint
            }),
            *rect,
        );
    }

    if app.settings_show_detail || (app.screen == Screen::ModelProfiles && app.profile_actions_open)
    {
        return;
    }
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
    for (column, label) in ui_layout
        .settings_columns
        .iter()
        .zip(["Setting", "Value", "Source", "Action", "Status"])
    {
        frame.render_widget(Paragraph::new(label).style(theme.hint), *column);
    }
    for (category, rect) in &ui_layout.settings_categories {
        frame.render_widget(
            Paragraph::new(category.to_uppercase()).style(theme.accent),
            *rect,
        );
    }
    for (index, rect) in &ui_layout.settings_rows {
        let Some(definition) = definitions.get(*index) else {
            continue;
        };
        let display = app.settings_value_display(&definition.id);
        let selected = app.settings_setting_index == *index;
        let style = if selected { theme.selected } else { theme.text };
        frame.render_widget(Paragraph::new("").style(style), *rect);
        let invalid = app
            .settings_validation_error
            .as_ref()
            .is_some_and(|error| error.contains(definition.id.as_str()));
        let status = if invalid {
            "Invalid"
        } else if !definition.supported {
            "Unsup."
        } else {
            ""
        };
        let source = if display.can_clear {
            if app.screen == Screen::ModelProfiles {
                "Profile"
            } else {
                "Settings"
            }
        } else if !definition.supported {
            "—"
        } else if display.source.to_lowercase().contains("settings") {
            "Settings"
        } else if display.source.contains("server") {
            "Server default"
        } else if ui_layout.settings_list.width >= 70 {
            "Runtime default"
        } else {
            "Runtime"
        };
        let action = if display.can_clear { "Inherit" } else { "—" };
        for (i, text) in [
            definition.label.as_str(),
            display.value.as_str(),
            source,
            action,
            status,
        ]
        .iter()
        .enumerate()
        {
            let mut cell = ui_layout.settings_columns[i];
            cell.y = rect.y;
            let hovered = app.hover == Some(HoverTarget::Setting(*index))
                || (i == 1
                    && definition.supported
                    && app.hover == Some(HoverTarget::SettingValue(*index)))
                || (i == 3
                    && display.can_clear
                    && app.hover == Some(HoverTarget::SettingInherit(*index)));
            let cell_style = if hovered {
                theme.hovered
            } else if i == 4 && !status.is_empty() {
                theme.warning
            } else if i == 1 && (!definition.supported || app.settings_busy()) {
                theme.muted
            } else {
                style
            };
            frame.render_widget(
                Paragraph::new(truncate_middle(
                    text,
                    cell.width.saturating_sub(1) as usize,
                    "…",
                ))
                .style(cell_style),
                cell,
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
    let (left, right) = concise_help_columns(theme, glyphs);
    frame.render_widget(Paragraph::new(left), columns[0]);
    frame.render_widget(Paragraph::new(right), columns[1]);
}

fn concise_help_columns<'a>(theme: &Theme, glyphs: &Glyphs) -> (Vec<Line<'a>>, Vec<Line<'a>>) {
    let left = vec![
        Line::from(Span::styled("NAVIGATION", theme.hint)),
        key_value("Tab / Shift+Tab", "change focus", theme),
        key_value("Left / Right", "move navigation focus", theme),
        key_value("D / Esc", "details / back (scrollable)", theme),
        key_value("Mouse", "select rows / actions", theme),
        Line::default(),
        Line::from(Span::styled("MODELS AND PROFILES", theme.hint)),
        key_value("Models", "Installed / Discover", theme),
        key_value("Left / Right", "change library view", theme),
        key_value("e / f / d", "search, filter, download", theme),
        key_value("p / x", "pause/resume or cancel download", theme),
        key_value("f / Esc", "installed filter / clear", theme),
        key_value("J", "focused downloads", theme),
        key_value("l/u/e/Delete", "load, unload, bind, inherit", theme),
    ];
    let right = vec![
        Line::from(Span::styled("SETTINGS AND RUNTIMES", theme.hint)),
        key_value("Settings", "Server operations and engine overrides", theme),
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
        key_value("D / Esc", "full details / back", theme),
        key_value("Models", "f filter; Discover: e search, d get", theme),
        key_value("p / x", "pause/resume or cancel download", theme),
        key_value("Runtimes", "s search, g/Q/N default", theme),
        key_value(
            "Settings",
            "Enter edit, Delete inherit, / search, o filter, R reset, i details",
            theme,
        ),
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
        key_value(
            "D / Esc",
            "full details / back; arrows or wheel scroll",
            theme,
        ),
        key_value(
            "J / Esc",
            "downloads / inventory; Up/Down selects job",
            theme,
        ),
        key_value(
            "Installed: f",
            "local filter; Esc clears; Ctrl+U clears input",
            theme,
        ),
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
            "m / p / D / d / r",
            "profile model, role, duplicate, delete, refresh",
            theme,
        ),
        key_value(
            "i / [ / ]",
            "setting details; scroll details up/down",
            theme,
        ),
        key_value(
            "/ / o / R",
            "search settings, overrides filter, scoped reset",
            theme,
        ),
        key_value(
            "l/u/e/Delete",
            "load, unload, rebind engine, or inherit",
            theme,
        ),
        Line::default(),
        Line::from(Span::styled("SETTINGS AND RUNTIMES", theme.hint)),
        key_value("Settings", "Server operations and engine overrides", theme),
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

fn render_inventory_detail(frame: &mut Frame<'_>, app: &App, theme: &Theme, layout: &UiLayout) {
    if let Some(text) = inspection_text(app) {
        let summary = text.lines().take(2).collect::<Vec<_>>().join("\n");
        frame.render_widget(
            Paragraph::new(format!("Selected item  |  D: full details\n{summary}"))
                .style(theme.muted),
            layout.inventory_detail,
        );
    }
}

/// Capture the selected identity and its full facts before opening the reader.
/// Background reordering cannot change what the user is inspecting.
pub(crate) fn inspection_text(app: &App) -> Option<String> {
    let mut text = inspection_body(app).unwrap_or_else(|| match app.screen {
        Screen::Runtimes => app.runtime_list_error.clone().unwrap_or_else(|| {
            if app.runtime_list_loading {
                "Inspecting local runtimes".to_owned()
            } else {
                "Select an installed runtime to inspect it".to_owned()
            }
        }),
        Screen::Models => {
            if let norted_core::RegistryState::Failed { message } = &app.snapshot.registry_state {
                message.clone()
            } else {
                "Select an artifact or open J: Jobs to inspect a download".to_owned()
            }
        }
        Screen::Overview => "No resident backends observed".to_owned(),
        _ => "No selected item".to_owned(),
    });
    if let Some(notice) = &app.notice {
        text.push_str(&format!("\n\nOperation notice: {notice}"));
    }
    Some(text)
}

fn inspection_body(app: &App) -> Option<String> {
    if app.overlay == Some(crate::app::Overlay::Help) {
        return Some(
            help_lines(&Theme::current(app.no_color), &Glyphs::current(app.unicode))
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if app.overlay == Some(crate::app::Overlay::RuntimeSearch) {
        let result = app
            .runtime_search
            .as_ref()?
            .results
            .get(app.selected_runtime_search_result?)?;
        let available = &result.entry.available;
        return Some(format!(
            "Candidate: {}\nVersion: {}\nRuntime ID: {}\nCompatibility: {:?}\nInstalled: {}\nSource: {}\nIdentity: {:#?}\nAcquisition: {:#?}",
            available.display_name,
            available.identity.version,
            available.runtime_id,
            result.entry.compatibility,
            result.installed,
            available.source_url,
            available.identity,
            available.acquisition
        ));
    }
    if app.overlay == Some(crate::app::Overlay::ModelRuntime) {
        let status = app
            .runtime_list
            .as_ref()?
            .installed
            .get(app.runtime_picker_selection?)?;
        return Some(format!(
            "Engine: {}\nVersion: {}\nRuntime ID: {}\nModel fit: {:?}\nInstalled compatibility: {:?}\nLocation: {}\nIdentity: {:#?}",
            status.runtime.manifest.identity.engine_id,
            status.runtime.manifest.identity.version,
            status.runtime.manifest.runtime_id,
            app.runtime_picker_compatibility(&status.runtime.manifest.runtime_id),
            status.compatibility,
            status.runtime.installation_root.display(),
            status.runtime.manifest.identity
        ));
    }
    if app.screen == Screen::Models && app.downloads_focused {
        let job = app
            .selected_model_download_job
            .as_ref()
            .and_then(|id| app.model_download_jobs.iter().find(|job| &job.id == id))
            .or_else(|| app.model_download_jobs.first());
        return Some(job.map_or_else(|| "No download jobs".to_owned(), |job| format!("Job: {}\nReference: {}\nPhase: {:?}\nTransferred: {}\nTotal: {}\nProgress: {}\nRate: {}\nETA: {}\nMessage: {}\n", job.id, job.model_ref, job.phase, format_bytes(job.downloaded_bytes), job.total_bytes.map(format_bytes).unwrap_or_else(|| "Unknown".to_owned()), job.progress_percent.map(|v| format!("{v:.1}%")).unwrap_or_else(|| "Unknown".to_owned()), job.transfer_bytes_per_second.map(|v| format!("{}/s", format_bytes(v as u64))).unwrap_or_else(|| "Unknown".to_owned()), job.estimated_remaining.map(friendly_remaining).unwrap_or_else(|| "Unknown".to_owned()), job.message)));
    }
    let mut fields: Vec<(&str, String)> = Vec::new();
    match app.screen {
        Screen::Models if app.model_library_view == ModelLibraryView::Installed => {
            let model = app.snapshot.models.get(app.selected_model?)?;
            fields.extend([
                ("Model", model.display_name.clone()),
                ("Artifact ID", model.id.to_string()),
                ("Path", model.path.display().to_string()),
                ("Format", model.format.to_string()),
                ("Size", format_bytes(model.size_bytes)),
            ]);
            if let Some(value) = &model.hash {
                fields.push(("Hash", value.clone()));
            }
            if let Some(value) = &model.architecture {
                fields.push(("Architecture", value.clone()));
            }
            if let Some(value) = model.context_length {
                fields.push(("Context length", value.to_string()));
            }
            if let Some(p) = &model.provenance {
                fields.push(("Provider", p.provider.clone()));
                fields.push(("Acquisition", p.acquisition_id.clone()));
                for (label, value) in [
                    ("Repository", &p.repository),
                    ("Logical ID", &p.logical_id),
                    ("Source", &p.source),
                    ("Revision", &p.revision),
                    ("Filename", &p.remote_filename),
                    ("Digest", &p.digest),
                ] {
                    if let Some(value) = value {
                        fields.push((label, value.clone()));
                    }
                }
            } else {
                fields.push((
                    "Provenance",
                    "Local artifact; no acquisition receipt".to_owned(),
                ));
            }
            if let Some(value) = &model.norted_package {
                fields.push(("Package", format!("{value:#?}")));
            }
            if let Some(value) = &model.native_identity {
                fields.push(("Native identity", format!("{value:#?}")));
            }
            for value in &model.auxiliary_artifacts {
                fields.push(("Companion", format!("{value:#?}")));
            }
            let runtime = app
                .runtime_list
                .as_ref()
                .and_then(|s| s.selections.model_overrides.get(&model.id));
            fields.push((
                "Runtime binding",
                runtime.map_or_else(
                    || "Inherit format default".to_owned(),
                    |id| format!("Explicit: {id}"),
                ),
            ));
            fields.push(("Format default", selection_text(app, model.format, false)));
            fields.push(("Actions on model", "c: Create Profile | v: Runtime | u: Unload | d: Remove (when managed). Return to the inventory to act.".to_owned()));
        }
        Screen::Models => {
            let artifacts = app.model_search_artifacts();
            let (repository, artifact) =
                artifacts.get(app.selected_model_search_result?).copied()?;
            fields.extend([
                ("Artifact", artifact.filename.clone()),
                ("Repository", repository.repository.clone()),
                ("Revision", repository.revision.clone()),
                ("Provider", repository.provider.clone()),
                ("Exact reference", artifact.model_ref.clone()),
                (
                    "Digest",
                    artifact
                        .sha256
                        .clone()
                        .unwrap_or_else(|| "Unknown".to_owned()),
                ),
                ("Format candidate", artifact.format.to_string()),
                ("Local state", discovered_state(app, repository, artifact)),
                (
                    "Compatibility",
                    "Unverified; a discovered format is not verified runtime compatibility"
                        .to_owned(),
                ),
                (
                    "Size",
                    artifact
                        .size_bytes
                        .map(format_bytes)
                        .unwrap_or_else(|| "Unknown".to_owned()),
                ),
                ("Companions", artifact.required_companions.join("\n")),
            ]);
            if let Some(value) = &artifact.package_manifest {
                fields.push(("Package manifest", value.clone()));
            }
        }
        Screen::Runtimes => {
            let snapshot = app.runtime_list.as_ref()?;
            let status = snapshot.installed.get(app.selected_runtime?)?;
            let manifest = &status.runtime.manifest;
            fields.extend([
                ("Engine", manifest.identity.engine_id.clone()),
                ("Version", manifest.identity.version.clone()),
                ("Runtime ID", manifest.runtime_id.to_string()),
                (
                    "Variant",
                    format!(
                        "{} / {}",
                        manifest.identity.accelerator, manifest.identity.variant
                    ),
                ),
                (
                    "Installation",
                    status.runtime.installation_root.display().to_string(),
                ),
                (
                    "Entrypoint",
                    status.runtime.entrypoint_path().display().to_string(),
                ),
                ("Compatibility", format!("{:?}", status.compatibility)),
                ("Format defaults", status.selected_for.join(", ")),
                ("Update policy", runtime_policy_detail(app, status)),
                ("Source", format!("{:#?}", manifest.identity.package)),
            ]);
            for (label, format) in [
                ("GGUF default", ArtifactFormat::Gguf),
                ("Q27 default", ArtifactFormat::Q27),
                ("NInfer default", ArtifactFormat::Ninfer),
            ] {
                fields.push((label, selection_text(app, format, false)));
            }
            for (model, id) in &snapshot.selections.model_overrides {
                if id == &manifest.runtime_id {
                    fields.push(("Model override", model.to_string()));
                }
            }
            if let Some(control) = &app.control {
                for backend in &control.backends {
                    if backend.runtime_id.as_ref() == Some(&manifest.runtime_id) {
                        fields.push((
                            "Observed backend",
                            format!("{}: {:?}", backend.model_profile_id, backend.lifecycle),
                        ));
                    }
                }
            }
            if let Some(value) = &manifest.source_build {
                fields.push(("Source build", format!("{value:#?}")));
            }
            if let Some(value) = app.runtime_updates.get(&manifest.runtime_id) {
                fields.push(("Update result", format!("{value:#?}")));
            }
            for warning in &snapshot.warnings {
                fields.push(("Scan warning", warning.clone()));
            }
        }
        Screen::Overview => {
            return app
                .resident_backends()
                .get(app.overview_selected.unwrap_or_default())
                .map(|backend| backend_detail(backend));
        }
        Screen::Server => return Some(server_text(app)),
        Screen::Logs => {
            let end = app.logs.len().saturating_sub(app.log_scroll);
            return Some(
                app.logs[..end]
                    .iter()
                    .rev()
                    .map(|entry| format!("{:?}  {}", entry.level, entry.message))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            );
        }
        Screen::Help => {
            return Some(
                help_lines(&Theme::current(app.no_color), &Glyphs::current(app.unicode))
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        _ => return None,
    }
    Some(
        fields
            .into_iter()
            .map(|(label, value)| format!("{label}: {value}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn server_text(app: &App) -> String {
    let auth = &app.public_auth_status;
    let mut text = format!(
        "PUBLIC API\nObserved: {}\nPublic endpoint: {}\nConfigured bind: {}\n\nEXPOSURE / AUTHENTICATION\nExposure: {}\nConfigured auth: {}\nEffective auth: {}\nActive keys: {}\n",
        app.snapshot.server.label(),
        app.control
            .as_ref()
            .and_then(|c| c.public_endpoint.as_deref())
            .unwrap_or("Not observed"),
        auth.bind,
        if auth.loopback { "Loopback" } else { "Remote" },
        auth.configured_mode,
        auth.effective_mode,
        if app.public_auth_loading {
            "Loading".to_owned()
        } else if app.public_auth_error.is_some() {
            "Unavailable".to_owned()
        } else {
            auth.active_key_count.to_string()
        }
    );
    if let norted_core::ServerState::Unknown { message }
    | norted_core::ServerState::Failed { message } = &app.snapshot.server
    {
        text.push_str(&format!("Observation: {message}\n"));
    }
    if auth.insecure_remote {
        text.push_str("WARNING: remote authentication is disabled\n");
    }
    if let Some(error) = &app.public_auth_error {
        text.push_str(&format!("Auth state unavailable: {error}\n"));
    }
    text.push_str("\nAPI ROUTES\nGET /health\nGET /v1/models\nGET /v1/models/{model}\nPOST /v1/responses\nPOST /v1/chat/completions\nPOST /v1/completions\nPOST /v1/embeddings\n\nRESIDENT BACKENDS\n");
    if let Some(control) = &app.control {
        for backend in &control.backends {
            text.push('\n');
            text.push_str(&backend_detail(backend));
        }
    } else {
        text.push_str("Not observed\n");
    }
    text
}

fn backend_detail(backend: &BackendStatus) -> String {
    let mut text = String::new();
    text.push_str(&format!("Profile: {}\nModel: {}\nRole: {:?}\nResidency: {:?}\nLifecycle: {:?}\nEngine: {}\nRuntime: {}\nActive requests: {}\nPrimary leases: {}\nDevices: {:?}\nPrivate endpoint: {}\n", backend.model_profile_id, backend.model_id, backend.role, backend.residency, backend.lifecycle, backend.engine_id.as_deref().unwrap_or("Unknown"), backend.runtime_id.as_ref().map(ToString::to_string).unwrap_or_else(|| "Unknown".to_owned()), backend.active_request_count, backend.primary_lease_count, backend.accelerator_binding, backend.private_endpoint.as_deref().unwrap_or("Not observed")));
    if let Some(progress) = &backend.load_progress {
        text.push_str(&format!("Load progress: {progress:#?}\n"));
    }
    if let Some(failure) = &backend.failure {
        text.push_str(&format!("Failure: {failure:#?}\n"));
    }
    for activity in &backend.activities {
        text.push_str(&format!("Activity: {}\n", format_activity(activity)));
    }
    text
}
