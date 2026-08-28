use norted_core::RegistryState;
use norted_engine::InstallationState;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap};

use crate::app::{App, Screen};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{content_layout, format_bytes, key_value, render_empty, section_title};
use crate::ui::layout::{HoverTarget, UiLayout};

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
        Screen::Engines => render_engines(frame, area, app, theme, glyphs),
        Screen::Server => render_server(frame, area, app, theme),
        Screen::Logs => render_logs(frame, area, app, theme, ui_layout),
        Screen::Settings => render_settings(frame, area, app, theme),
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
        RegistryState::NotScanned | RegistryState::Scanning => vec![
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
                    "Recognized formats: .gguf and .q27",
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
    let engine_value = app.control.as_ref().map_or_else(
        || "Unavailable".to_owned(),
        |control| control.installed_engine_count.to_string(),
    );
    let active_model = app.control.as_ref().map_or_else(
        || "Unavailable".to_owned(),
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
        ("ENGINES", engine_value),
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
    if matches!(
        app.snapshot.registry_state,
        RegistryState::NotScanned | RegistryState::Scanning
    ) {
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
            ]),
            Line::from(vec![
                Span::styled(format_bytes(model.size_bytes), theme.muted),
                Span::styled(format!("  {}", model.path.display()), theme.hint),
            ]),
        ])
        .style(style)
    });
    frame.render_widget(List::new(items), layout[1]);
}

fn render_engines(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, glyphs: &Glyphs) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title(
            "Engines",
            "Managed upstream runtimes and their capabilities",
            theme,
        ),
        layout[0],
    );
    let Some(control) = &app.control else {
        render_empty(
            frame,
            layout[1],
            &format!("{}  Server control unavailable", glyphs.stopped),
            app.control_observation_error
                .as_deref()
                .unwrap_or("Start `norted-server serve` to inspect registered engines."),
            theme,
        );
        return;
    };
    if control.engines.is_empty() {
        render_empty(
            frame,
            layout[1],
            &format!("{}  No engine adapters registered", glyphs.stopped),
            "The running server reported no available adapters.",
            theme,
        );
        return;
    }
    let mut lines = Vec::new();
    for engine in &control.engines {
        let (state, style) = match &engine.probe.installation {
            InstallationState::Installed { .. } if engine.probe.healthy => {
                ("available", theme.success)
            }
            InstallationState::Installed { .. } => ("unhealthy", theme.warning),
            InstallationState::NotInstalled => ("not installed", theme.muted),
            InstallationState::Invalid { .. } => ("invalid", theme.error),
        };
        lines.push(Line::from(vec![
            Span::styled(&engine.identity.display_name, theme.text),
            Span::styled(format!("  {state}"), style),
        ]));
        lines.push(Line::from(Span::styled(&engine.probe.detail, theme.muted)));
        if let InstallationState::Installed { installation } = &engine.probe.installation {
            lines.push(key_value(
                "VERSION",
                installation.engine.version.as_deref().unwrap_or("unknown"),
                theme,
            ));
            lines.push(key_value(
                "REVISION",
                installation.engine.revision.as_deref().unwrap_or("unknown"),
                theme,
            ));
            lines.push(Line::from(vec![
                Span::styled(format!("{:<12}", "BINARY"), theme.hint),
                Span::styled(installation.binary_path.display().to_string(), theme.text),
            ]));
        }
        lines.push(Line::default());
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), layout[1]);
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
    let endpoint = app
        .control
        .as_ref()
        .and_then(|control| control.public_endpoint.as_deref())
        .or_else(|| app.snapshot.server.endpoint())
        .unwrap_or("Not serving");
    let lifecycle = app
        .control
        .as_ref()
        .map(|control| format!("{:?}", control.backend.lifecycle))
        .unwrap_or_else(|| "Unavailable".to_owned());
    let active_model = app
        .control
        .as_ref()
        .and_then(|control| control.backend.model_id.as_ref())
        .map(ToString::to_string)
        .unwrap_or_else(|| "None".to_owned());
    let active_engine = app
        .control
        .as_ref()
        .and_then(|control| control.backend.engine_id.clone())
        .unwrap_or_else(|| "None".to_owned());
    let private_backend = app
        .control
        .as_ref()
        .and_then(|control| control.backend.private_endpoint.clone())
        .unwrap_or_else(|| "None".to_owned());
    frame.render_widget(
        Paragraph::new(vec![
            key_value("STATE", app.snapshot.server.label(), theme),
            key_value("ENDPOINT", endpoint, theme),
            key_value("BACKEND", &lifecycle, theme),
            key_value("MODEL", &active_model, theme),
            key_value("ENGINE", &active_engine, theme),
            key_value("PRIVATE", &private_backend, theme),
            Line::default(),
            Line::from(Span::styled("Available now", theme.text)),
            Line::from(Span::styled("GET  /health", theme.accent)),
            Line::from(Span::styled("GET  /v1/models", theme.accent)),
            Line::from(Span::styled("POST /v1/responses", theme.accent)),
            Line::from(Span::styled(
                "PRIVATE is the internal loopback llama.cpp endpoint.",
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

fn render_settings(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title(
            "Settings",
            "Resolved configuration and platform paths",
            theme,
        ),
        layout[0],
    );
    let model_paths = if app.model_paths.is_empty() {
        "None configured".to_owned()
    } else {
        app.model_paths.join(", ")
    };
    frame.render_widget(
        Paragraph::new(vec![
            key_value("SERVER", &app.server_address, theme),
            key_value("CONFIG", &app.config_path, theme),
            key_value("MODELS", &model_paths, theme),
            Line::default(),
            Line::from(Span::styled(
                "Relative model paths resolve against the configuration directory.",
                theme.muted,
            )),
            Line::from(Span::styled(
                "NO_COLOR is honored for monochrome output.",
                theme.hint,
            )),
        ])
        .wrap(Wrap { trim: true }),
        layout[1],
    );
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
        key_value("Mouse", "click pages and interactive rows", theme),
        Line::default(),
        Line::from(Span::styled("CURRENT VIEW", theme.hint)),
        key_value("Up/Down or j/k", "select or scroll", theme),
        key_value("PageUp/PageDown", "scroll logs or model list", theme),
        key_value("Wheel", "scroll the current view", theme),
        key_value("End", "follow newest logs", theme),
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
            "Slash commands: /load /unload /status /models /engines /server /logs /settings /help /quit",
            theme.muted,
        )),
    ]
}
