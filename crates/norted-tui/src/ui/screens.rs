use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap};

use crate::app::{App, Screen};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{content_layout, format_bytes, key_value, render_empty, section_title};

pub fn render_screen(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    compact: bool,
) {
    match app.screen {
        Screen::Overview => render_overview(frame, area, app, theme, glyphs, compact),
        Screen::Models => render_models(frame, area, app, theme, glyphs),
        Screen::Engines => render_engines(frame, area, theme, glyphs),
        Screen::Server => render_server(frame, area, app, theme),
        Screen::Logs => render_logs(frame, area, app, theme),
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
    let body = if app.snapshot.models.is_empty() {
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
    } else {
        vec![
            Line::from(Span::styled("Ready to explore", theme.text)),
            Line::from(Span::styled(
                "Open Models to inspect discovered local artifacts.",
                theme.muted,
            )),
        ]
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
    let values = [
        ("SERVER", app.snapshot.server.label().to_owned()),
        ("MODELS", app.snapshot.models.len().to_string()),
        ("ENGINES", app.snapshot.installed_engine_count.to_string()),
        (
            "ACTIVE MODEL",
            app.snapshot
                .active_model
                .as_deref()
                .unwrap_or("None")
                .to_owned(),
        ),
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

fn render_models(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, glyphs: &Glyphs) {
    let layout = content_layout(area);
    let subtitle = if app.snapshot.registry_warnings.is_empty() {
        "Local artifacts discovered from configured search paths".to_owned()
    } else {
        format!(
            "Local artifacts discovered with {} warning(s); see Logs",
            app.snapshot.registry_warnings.len()
        )
    };
    frame.render_widget(section_title("Models", &subtitle, theme), layout[0]);
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
    let items = app.snapshot.models.iter().map(|model| {
        ListItem::new(vec![
            Line::from(vec![
                Span::styled(&model.display_name, theme.text),
                Span::styled(format!("  {}", model.format.as_str()), theme.accent),
            ]),
            Line::from(vec![
                Span::styled(format_bytes(model.size_bytes), theme.muted),
                Span::styled(format!("  {}", model.path.display()), theme.hint),
            ]),
        ])
    });
    frame.render_widget(List::new(items).highlight_style(theme.selected), layout[1]);
}

fn render_engines(frame: &mut Frame<'_>, area: Rect, theme: &Theme, glyphs: &Glyphs) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title(
            "Engines",
            "Managed upstream runtimes and their capabilities",
            theme,
        ),
        layout[0],
    );
    render_empty(
        frame,
        layout[1],
        &format!("{}  No engine adapters registered", glyphs.stopped),
        "Engine support is intentionally absent while the adapter and supervisor foundations settle.",
        theme,
    );
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
    let endpoint = app.snapshot.server.endpoint().unwrap_or("Not serving");
    frame.render_widget(
        Paragraph::new(vec![
            key_value("STATE", app.snapshot.server.label(), theme),
            key_value("ENDPOINT", endpoint, theme),
            Line::default(),
            Line::from(Span::styled("Available now", theme.text)),
            Line::from(Span::styled("GET  /health", theme.accent)),
            Line::from(Span::styled("GET  /v1/models", theme.accent)),
            Line::default(),
            Line::from(Span::styled(
                "The Responses API remains future work; no inference endpoint is advertised.",
                theme.muted,
            )),
        ])
        .wrap(Wrap { trim: true }),
        layout[1],
    );
}

fn render_logs(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title("Logs", "Application and model-registry warnings", theme),
        layout[0],
    );
    let visible = layout[1].height as usize;
    let start = app.logs.len().saturating_sub(visible);
    let lines = app.logs[start..].iter().map(|entry| {
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
        key_value("Tab", "next screen", theme),
        key_value("Shift+Tab", "previous screen", theme),
        key_value("j / k", "move between screens", theme),
        Line::from(Span::styled(
            format!("Right key ({}) also advances", glyphs.right),
            theme.hint,
        )),
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
            "Slash commands: /status /models /engines /server /logs /settings /help /quit",
            theme.muted,
        )),
    ]
}
