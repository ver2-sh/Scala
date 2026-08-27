use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap};

use crate::app::{App, Overlay, Screen};
use crate::theme::Theme;

const MIN_WIDTH: u16 = 46;
const MIN_HEIGHT: u16 = 13;
const COMPACT_WIDTH: u16 = 84;

pub fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let theme = Theme::current(app.no_color);
    frame.render_widget(Block::default().style(theme.text), area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_too_small(frame, area, &theme);
        return;
    }

    let compact = area.width < COMPACT_WIDTH || area.height < 23;
    let shell = area.inner(Margin {
        horizontal: if compact { 1 } else { 2 },
        vertical: 0,
    });
    let regions = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if compact { 3 } else { 4 }),
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .split(shell);

    render_header(frame, regions[0], app, &theme, compact);
    render_screen(frame, regions[1], app, &theme, compact);
    render_command_bar(frame, regions[2], app, &theme);
    render_footer(frame, regions[3], app, &theme);
    render_overlays(frame, app, &theme, regions[2]);

    if app.command_active {
        let prefix_width = app.command_input.chars().take(app.command_cursor).count() as u16;
        let cursor_x = regions[2]
            .x
            .saturating_add(3)
            .saturating_add(prefix_width)
            .min(regions[2].right().saturating_sub(2));
        frame.set_cursor_position(Position::new(cursor_x, regions[2].y + 1));
    }
}

fn render_too_small(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let message = Paragraph::new(vec![
        Line::from(Span::styled("NORTED SERVER", theme.accent)),
        Line::default(),
        Line::from(Span::styled("Terminal is too small", theme.warning)),
        Line::from(Span::styled(
            format!("Resize to at least {MIN_WIDTH} × {MIN_HEIGHT}"),
            theme.muted,
        )),
        Line::default(),
        Line::from(Span::styled("Ctrl+C  exit", theme.hint)),
    ])
    .alignment(Alignment::Center)
    .wrap(Wrap { trim: true });
    let height = 6.min(area.height);
    let y = area.y + area.height.saturating_sub(height) / 2;
    frame.render_widget(message, Rect::new(area.x, y, area.width, height));
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, compact: bool) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if compact {
            [Constraint::Length(1), Constraint::Length(2)]
        } else {
            [Constraint::Length(2), Constraint::Length(2)]
        })
        .split(area);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(24)])
        .split(rows[0]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("◆", theme.accent),
            Span::raw("  "),
            Span::styled("NORTED", theme.accent),
            Span::styled(" SERVER", theme.text),
            Span::styled("  local runtime control", theme.muted),
        ])),
        top[0],
    );
    let (marker, state_style) = match &app.snapshot.server {
        norted_core::ServerState::Running { .. } => ("●", theme.success),
        norted_core::ServerState::Failed { .. } => ("!", theme.error),
        norted_core::ServerState::Starting | norted_core::ServerState::Stopping => {
            ("◆", theme.warning)
        }
        norted_core::ServerState::Stopped => ("○", theme.muted),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(marker, state_style),
            Span::raw("  API "),
            Span::styled(app.snapshot.server.label(), state_style),
        ]))
        .alignment(Alignment::Right),
        top[1],
    );

    let nav_line = if compact {
        Line::from(vec![
            Span::styled("‹  ", theme.hint),
            Span::styled(app.screen.label(), theme.nav_active),
            Span::styled("  ›", theme.hint),
        ])
    } else {
        let mut spans = Vec::new();
        for (index, screen) in Screen::ALL.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw("   "));
            }
            let style = if *screen == app.screen {
                theme.nav_active
            } else {
                theme.nav_inactive
            };
            spans.push(Span::styled(screen.label(), style));
        }
        Line::from(spans)
    };
    frame.render_widget(
        Paragraph::new(nav_line).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(theme.border),
        ),
        rows[1],
    );
}

fn render_screen(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, compact: bool) {
    let content = area.inner(Margin {
        horizontal: if compact { 1 } else { 2 },
        vertical: 1,
    });
    match app.screen {
        Screen::Overview => render_overview(frame, content, app, theme, compact),
        Screen::Models => render_models(frame, content, app, theme),
        Screen::Engines => render_engines(frame, content, theme),
        Screen::Server => render_server(frame, content, app, theme),
        Screen::Logs => render_logs(frame, content, app, theme),
        Screen::Settings => render_settings(frame, content, app, theme),
        Screen::Help => render_help_content(frame, content, theme),
    }
}

fn section_title<'a>(title: &'a str, subtitle: &'a str, theme: &Theme) -> Paragraph<'a> {
    Paragraph::new(vec![
        Line::from(Span::styled(title, theme.accent)),
        Line::from(Span::styled(subtitle, theme.muted)),
    ])
}

fn render_overview(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, compact: bool) {
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
    render_metrics(frame, layout[1], app, theme, compact);

    if app.snapshot.models.is_empty() {
        let body = vec![
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
        ];
        frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), layout[2]);
    } else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled("Ready to explore", theme.text)),
                Line::from(Span::styled(
                    "Open Models to inspect discovered local artifacts.",
                    theme.muted,
                )),
            ]),
            layout[2],
        );
    }
}

fn render_metrics(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, compact: bool) {
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
                    .border_style(theme.accent)
                    .padding(Padding::horizontal(1)),
            ),
            columns[index],
        );
    }
}

fn render_models(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title(
            "Models",
            "Local artifacts discovered from configured search paths",
            theme,
        ),
        layout[0],
    );
    if app.snapshot.models.is_empty() {
        render_empty(
            frame,
            layout[1],
            "◇  Registry is empty",
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

fn render_engines(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
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
        "○  No engine adapters installed",
        "llama.cpp and Q27 adapters are planned. This bootstrap does not download or run either engine.",
        theme,
    );
}

fn render_server(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title(
            "API Server",
            "Stable public gateway, isolated from engine-native protocols",
            theme,
        ),
        layout[0],
    );
    let endpoint = app.snapshot.server.endpoint().unwrap_or("Not serving");
    let body = vec![
        key_value("STATE", app.snapshot.server.label(), theme),
        key_value("ENDPOINT", endpoint, theme),
        Line::default(),
        Line::from(Span::styled("Available now", theme.text)),
        Line::from(Span::styled("GET  /health", theme.accent)),
        Line::from(Span::styled("GET  /v1/models", theme.accent)),
        Line::default(),
        Line::from(Span::styled(
            "The Responses API is the next gateway phase; no inference endpoint is advertised yet.",
            theme.muted,
        )),
    ];
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), layout[1]);
}

fn render_logs(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title(
            "Logs",
            "Application events are kept separate from terminal rendering",
            theme,
        ),
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
    let body = vec![
        key_value("SERVER", &app.server_address, theme),
        key_value("CONFIG", &app.config_path, theme),
        key_value("MODELS", &model_paths, theme),
        Line::default(),
        Line::from(Span::styled(
            "Use `norted-server config show` to inspect the complete resolved TOML.",
            theme.muted,
        )),
        Line::from(Span::styled(
            "NO_COLOR is honored for monochrome output.",
            theme.hint,
        )),
    ];
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), layout[1]);
}

fn render_help_content(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let layout = content_layout(area);
    frame.render_widget(
        section_title(
            "Help",
            "Navigate directly or open the command bar from anywhere",
            theme,
        ),
        layout[0],
    );
    frame.render_widget(Paragraph::new(help_lines(theme)), layout[1]);
}

fn render_command_bar(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let focused = if app.command_active {
        theme.accent
    } else {
        theme.border
    };
    let input = if app.command_active {
        if app.command_input.is_empty() {
            Line::from(Span::styled("Type a /command", theme.hint))
        } else {
            Line::from(Span::styled(&app.command_input, theme.command))
        }
    } else if let Some(notice) = &app.notice {
        Line::from(vec![
            Span::styled("!  ", theme.warning),
            Span::styled(notice, theme.text),
        ])
    } else {
        Line::from(vec![
            Span::styled("›", theme.accent),
            Span::styled("  Press / to run a command", theme.hint),
        ])
    };
    frame.render_widget(
        Paragraph::new(input).block(
            Block::default()
                .borders(Borders::TOP | Borders::BOTTOM)
                .border_style(focused)
                .padding(Padding::horizontal(1)),
        ),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let line = if app.command_active {
        vec![
            hint("↑↓", "select", theme),
            hint("Enter", "run", theme),
            hint("Esc", "cancel", theme),
        ]
    } else {
        vec![
            hint("Tab", "navigate", theme),
            hint("/", "commands", theme),
            hint("?", "help", theme),
            hint("Ctrl+C", "exit", theme),
        ]
    };
    let spans = line.into_iter().flatten().collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_overlays(frame: &mut Frame<'_>, app: &App, theme: &Theme, command_area: Rect) {
    if app.overlay == Some(Overlay::Help) {
        let area = centered_rect(74, 72, frame.area());
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(help_lines(theme))
                .block(popup_block(" Help ", theme))
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    if !app.command_active {
        return;
    }
    let suggestions = app.suggestions();
    if suggestions.is_empty() {
        return;
    }
    let height = (suggestions.len() as u16 + 2).min(10);
    let y = command_area.y.saturating_sub(height);
    let area = Rect::new(command_area.x, y, command_area.width, height);
    frame.render_widget(Clear, area);
    let items = suggestions.iter().enumerate().map(|(index, command)| {
        let style = if index == app.suggestion_index {
            theme.selected
        } else {
            Style::default()
        };
        ListItem::new(Line::from(vec![
            Span::styled(format!("{:<12}", command.name), theme.accent),
            Span::styled(command.description, theme.muted),
        ]))
        .style(style)
    });
    frame.render_widget(
        List::new(items).block(popup_block(" Commands ", theme)),
        area,
    );
}

fn content_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3)])
        .split(area)
}

fn render_empty(frame: &mut Frame<'_>, area: Rect, title: &str, detail: &str, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(title, theme.text)),
            Line::default(),
            Line::from(Span::styled(detail, theme.muted)),
        ])
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn key_value<'a>(key: &'a str, value: &'a str, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{key:<12}"), theme.hint),
        Span::styled(value, theme.text),
    ])
}

fn hint<'a>(key: &'a str, label: &'a str, theme: &Theme) -> Vec<Span<'a>> {
    vec![
        Span::styled(key, theme.accent),
        Span::styled(format!(" {label}   "), theme.hint),
    ]
}

fn help_lines(theme: &Theme) -> Vec<Line<'_>> {
    vec![
        Line::from(Span::styled("NAVIGATION", theme.hint)),
        key_value("Tab / →", "next screen", theme),
        key_value("Shift+Tab", "previous screen", theme),
        key_value("j / k", "move between screens", theme),
        Line::default(),
        Line::from(Span::styled("COMMANDS", theme.hint)),
        key_value("/", "open slash-command suggestions", theme),
        key_value("↑ / ↓", "select a suggestion", theme),
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

fn popup_block<'a>(title: &'a str, theme: &Theme) -> Block<'a> {
    Block::default()
        .title(Span::styled(title, theme.accent))
        .borders(Borders::ALL)
        .border_style(theme.border)
        .style(theme.panel)
        .padding(Padding::horizontal(1))
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}
