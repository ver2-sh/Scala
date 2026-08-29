use norted_core::{ArtifactFormat, RegistryState, RuntimeCompatibility, RuntimeUpdateState};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap};

use crate::app::{App, Screen};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{content_layout, format_bytes, key_value, render_empty, section_title};
use crate::ui::layout::{HoverTarget, UiLayout};
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
        Screen::Overview => render_overview(frame, area, app, theme, glyphs, ui_layout.compact),
        Screen::Models => render_models(frame, area, app, theme, glyphs, ui_layout),
        Screen::Runtimes => render_runtimes(frame, area, app, theme, glyphs, ui_layout),
        Screen::Server => render_server(frame, area, app, theme),
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
    compact: bool,
) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(if compact { 6 } else { 4 }),
            Constraint::Min(5),
        ])
        .split(area);
    frame.render_widget(
        section_title(
            "Overview",
            "Your local model runtime, from artifacts to API",
            theme,
        ),
        layout[0],
    );
    render_metrics(frame, layout[1], app, theme, glyphs, compact);
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
                    "Add one or more directories under [models].paths in your config.",
                    theme.muted,
                )),
                Line::from(Span::styled(
                    "Recognized formats: .gguf, .q27, and .ninfer",
                    theme.hint,
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
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), layout[2]);
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
            control
                .backend
                .model_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "None".to_owned())
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

fn render_models(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    ui_layout: &UiLayout,
) {
    let layout = content_layout(area);
    let subtitle = match &app.snapshot.registry_state {
        RegistryState::NotScanned => "Model discovery has not started".to_owned(),
        RegistryState::Scanning => "Scanning configured search paths in the background".to_owned(),
        RegistryState::Failed { .. } => "Model discovery could not complete; see Logs".to_owned(),
        RegistryState::Ready if app.snapshot.registry_warnings.is_empty() => {
            "Local artifacts discovered from configured search paths".to_owned()
        }
        RegistryState::Ready | RegistryState::ReadyWithWarnings { .. } => format!(
            "Local artifacts discovered with {} warning(s); see Logs",
            app.snapshot.registry_warnings.len()
        ),
    };
    frame.render_widget(section_title("Models", &subtitle, theme), layout[0]);
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
            &format!("{}  Registry is empty", glyphs.empty),
            "Configure model directories in config.toml. Unknown file types are ignored.",
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
                        matches!(
                            control.backend.lifecycle,
                            norted_engine::BackendLifecycle::Loading
                                | norted_engine::BackendLifecycle::Running
                        ) && control.backend.model_id.as_ref() == Some(&model.id)
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
            Line::from(vec![
                Span::styled(format_bytes(model.size_bytes), theme.muted),
                Span::styled(format!("  {}", model.path.display()), theme.hint),
            ]),
            Line::from(model.norted_package.as_ref().map_or_else(
                || Span::styled("Package: Raw", theme.muted),
                |package| {
                    let status = match &package.status {
                        norted_core::NortedPackageStatus::Valid => "valid",
                        norted_core::NortedPackageStatus::NeedsRuntimeCapability { .. } => {
                            "needs runtime capability"
                        }
                    };
                    Span::styled(
                        format!(
                            "Package: {}  {status}  schema {}  profile {}  runtime: evaluated separately",
                            package.kind,
                            package.manifest_version,
                            package
                                .runtime_policy_profile
                                .as_deref()
                                .or(package.runtime_policy_id.as_deref())
                                .unwrap_or("n/a")
                        ),
                        theme.hint,
                    )
                },
            )),
        ])
        .style(style)
    });
    frame.render_widget(List::new(items), layout[1]);
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
            let gguf = selection_text(app, ArtifactFormat::Gguf);
            let q27 = selection_text(app, ArtifactFormat::Q27);
            let ninfer = selection_text(app, ArtifactFormat::Ninfer);
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
            "Search upstream runtimes to install a compatible runtime pack."
        } else {
            "Models detected: press s to see compatible upstream runtimes, recommended first. Installation stays explicit."
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
                    format!(
                        "  source {} tree {} · {} · CMake {} · CUDA {}",
                        &build.source.commit_sha[..8],
                        &build.source.tree_sha[..8],
                        build.recipe_version,
                        build.toolchain.cmake_version,
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
        let search_style = if app.hover == Some(HoverTarget::RuntimeSearchAction) {
            theme.hovered
        } else {
            theme.accent
        };
        let update_style = if app.hover == Some(HoverTarget::RuntimeUpdateAction) {
            theme.hovered
        } else {
            theme.hint
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "[s] Search available",
                search_style,
            ))),
            ui_layout.runtime_search_action,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if app.runtime_update_loading {
                    "Checking…"
                } else {
                    "[u] Check updates"
                },
                update_style,
            ))),
            ui_layout.runtime_update_action,
        );
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

fn selection_text(app: &App, format: ArtifactFormat) -> String {
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
            format!(
                "{} {} {} / {}",
                identity.engine_id, identity.version, identity.accelerator, identity.variant
            )
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

fn render_server(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
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
        .map(|control| format!("{:?}", control.backend.lifecycle))
        .unwrap_or_else(|| {
            if pending {
                "Observing".to_owned()
            } else {
                "Unavailable".to_owned()
            }
        });
    let active_model = app
        .control
        .as_ref()
        .and_then(|control| control.backend.model_id.as_ref())
        .map(ToString::to_string)
        .unwrap_or_else(|| if pending { "Unknown" } else { "None" }.to_owned());
    let active_engine = app
        .control
        .as_ref()
        .and_then(|control| control.backend.engine_id.clone())
        .unwrap_or_else(|| if pending { "Unknown" } else { "None" }.to_owned());
    let active_runtime = app
        .control
        .as_ref()
        .and_then(|control| {
            control.backend.runtime_id.as_ref().map(|runtime_id| {
                control.backend.runtime_version.as_deref().map_or_else(
                    || runtime_id.to_string(),
                    |version| format!("{runtime_id} / {version}"),
                )
            })
        })
        .unwrap_or_else(|| if pending { "Unknown" } else { "None" }.to_owned());
    let private_backend = app
        .control
        .as_ref()
        .and_then(|control| control.backend.private_endpoint.clone())
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
            key_value("PUBLIC BIND", &auth.bind, theme),
            key_value("EXPOSURE", exposure, theme),
            key_value("AUTH CONFIGURED", &auth.configured_mode.to_string(), theme),
            key_value("AUTH EFFECTIVE", &auth.effective_mode.to_string(), theme),
            key_value("ACTIVE API KEYS", &active_key_count, theme),
            key_value("BACKEND", &lifecycle, theme),
            key_value("MODEL", &active_model, theme),
            key_value("ENGINE", &active_engine, theme),
            key_value("RUNTIME", &active_runtime, theme),
            key_value("PRIVATE", &private_backend, theme),
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
        layout[1],
    );
}

fn render_logs(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, ui_layout: &UiLayout) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title("Logs", "Application and model-registry warnings", theme),
        layout[0],
    );
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
            "Load defaults, reusable profiles, and exact-runtime validation",
            theme,
        ),
        layout[0],
    );
    let settings_scopes = app.settings_scopes();
    for (index, rect) in &ui_layout.settings_scope_rows {
        let Some(scope) = settings_scopes.get(*index) else {
            continue;
        };
        let label = match scope {
            crate::app::SettingsScope::Global => "Global".to_owned(),
            crate::app::SettingsScope::Engine(engine) => engine.clone(),
            crate::app::SettingsScope::Profile(profile) => profile.to_string(),
            crate::app::SettingsScope::Model(_) => "Selected model".to_owned(),
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

    let info_area = Rect::new(
        ui_layout.settings_scopes.x,
        ui_layout.settings_scopes.y.saturating_add(1),
        ui_layout.settings_scopes.width,
        3,
    );
    let mut info = Vec::new();
    if let Some(input) = &app.settings_input {
        let prompt = match input.kind {
            crate::app::SettingsInputKind::ProfileName => "New profile",
            crate::app::SettingsInputKind::SettingValue => "Value",
        };
        info.push(Line::from(vec![
            Span::styled(format!("{prompt}: "), theme.hint),
            Span::styled(&input.text, theme.text),
            Span::styled("_", theme.accent),
        ]));
        info.push(Line::from(Span::styled(
            "Enter saves · Esc cancels",
            theme.muted,
        )));
    } else if let Some(model) = &app.settings_model {
        info.push(Line::from(vec![
            Span::styled(format!("Model {model}"), theme.text),
            Span::styled(
                format!(
                    "  profile: {}",
                    app.assigned_profile_for_settings_model()
                        .map(ToString::to_string)
                        .as_deref()
                        .unwrap_or("none")
                ),
                theme.accent,
            ),
            Span::styled(
                format!(
                    "  runtime: {}",
                    app.settings_runtime_id
                        .as_ref()
                        .map(ToString::to_string)
                        .as_deref()
                        .unwrap_or("not validated")
                ),
                theme.muted,
            ),
        ]));
        if let Some(error) = &app.settings_validation_error {
            info.push(Line::from(Span::styled(error, theme.warning)));
        } else {
            info.push(Line::from(Span::styled(
                "p cycles/clears the assigned profile · r revalidates the exact runtime",
                theme.hint,
            )));
        }
    } else {
        info.push(Line::from(Span::styled(
            "Left/Right scope · Enter edit/cycle · Delete inherit · n new profile · d delete",
            theme.hint,
        )));
    }
    let running = app.control.as_ref().is_some_and(|control| {
        control.backend.lifecycle == norted_engine::BackendLifecycle::Running
    });
    if running {
        info.push(Line::from(Span::styled(
            "A model is running; edits apply only on its next load.",
            theme.warning,
        )));
    }
    frame.render_widget(Paragraph::new(info).wrap(Wrap { trim: true }), info_area);

    if app.load_profiles_loading {
        frame.render_widget(
            Paragraph::new("Loading load-profile state…").style(theme.muted),
            ui_layout.settings_list,
        );
        return;
    }
    if let Some(error) = &app.load_profiles_error {
        frame.render_widget(
            Paragraph::new(error.as_str())
                .style(theme.error)
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
            norted_core::LoadSettingKind::Choice { choices } => {
                format!("{} [{}]", definition.label, choices.join("|"))
            }
            norted_core::LoadSettingKind::UnsignedIntegerOrChoice { choices, .. } => {
                format!("{} [number|{}]", definition.label, choices.join("|"))
            }
            _ => definition.label.clone(),
        };
        let lines = if ui_layout.compact {
            vec![
                Line::from(Span::styled(definition.id.to_string(), theme.text)),
                Line::from(vec![
                    Span::styled(value, theme.accent),
                    Span::styled(format!(" · {source} · {support}"), theme.muted),
                ]),
            ]
        } else {
            vec![
                Line::from(vec![
                    Span::styled(format!("{:<38}", definition.id), theme.text),
                    Span::styled(format!("{value:<18}"), theme.accent),
                    Span::styled(source, theme.muted),
                ]),
                Line::from(vec![
                    Span::styled(format!("  {label}"), theme.muted),
                    Span::styled(
                        format!("  {support}"),
                        if definition.supported {
                            theme.hint
                        } else {
                            theme.warning
                        },
                    ),
                ]),
            ]
        };
        frame.render_widget(Paragraph::new(lines).style(style), *rect);
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
        key_value("Enter", "open navigation or load selected model", theme),
        key_value("u", "unload the active model from Models", theme),
        key_value("v", "choose a model-specific runtime override", theme),
        key_value("p", "open model load settings/profile management", theme),
        key_value(
            "x / Delete",
            "clear an override from the model runtime picker",
            theme,
        ),
        key_value("Mouse", "click pages and interactive rows", theme),
        Line::default(),
        Line::from(Span::styled("CURRENT VIEW", theme.hint)),
        key_value("Up/Down or j/k", "select or scroll", theme),
        key_value("PageUp/PageDown", "scroll logs or model list", theme),
        key_value("Wheel", "scroll the current view", theme),
        key_value("End", "follow newest logs", theme),
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
            "Slash commands: /load /unload /status /models /runtimes /server /logs /settings /help /quit",
            theme.muted,
        )),
    ]
}
