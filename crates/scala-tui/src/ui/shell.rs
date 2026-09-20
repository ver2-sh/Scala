use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};

use crate::app::{App, FocusArea, ModelLibraryView, Screen};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{centered_message, hint, input_window, marquee_text, remaining_width};
use crate::ui::layout::{HoverTarget, UiLayout};

pub const MIN_WIDTH: u16 = 46;
pub const MIN_HEIGHT: u16 = 13;
pub const COMPACT_WIDTH: u16 = 84;

pub fn render_too_small(frame: &mut Frame<'_>, area: Rect, theme: &Theme, glyphs: &Glyphs) {
    centered_message(
        frame,
        area,
        vec![
            Line::from(Span::styled("SCALA", theme.accent)),
            Line::default(),
            Line::from(Span::styled("Terminal is too small", theme.warning)),
            Line::from(Span::styled(
                format!(
                    "Resize to at least {MIN_WIDTH} {} {MIN_HEIGHT}",
                    glyphs.dimensions
                ),
                theme.muted,
            )),
            Line::default(),
            Line::from(Span::styled("Ctrl+C  exit", theme.hint)),
        ],
    );
}

pub fn render_header(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    layout: &UiLayout,
) {
    let area = layout.header;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if layout.compact {
            [Constraint::Length(1), Constraint::Length(3)]
        } else {
            [Constraint::Length(2), Constraint::Length(3)]
        })
        .split(area);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(24)])
        .split(rows[0]);
    let mut brand = vec![
        Span::styled(glyphs.brand, theme.accent),
        Span::raw("  "),
        Span::styled("SCALA", theme.accent),
        Span::styled(" SERVER", theme.text),
    ];
    if !layout.compact {
        brand.push(Span::styled("  local runtime control", theme.muted));
    }
    frame.render_widget(Paragraph::new(Line::from(brand)), top[0]);
    let (marker, state_style) = match &app.snapshot.server {
        scala_core::ServerState::Unknown { .. } => ("?", theme.warning),
        scala_core::ServerState::Running { .. } => (glyphs.running, theme.success),
        scala_core::ServerState::Failed { .. } => ("!", theme.error),
        scala_core::ServerState::Starting | scala_core::ServerState::Stopping => {
            (glyphs.transitional, theme.warning)
        }
        scala_core::ServerState::Stopped => (glyphs.stopped, theme.muted),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(marker, state_style),
            Span::raw("  API "),
            Span::styled(app.snapshot.server.label(), state_style),
        ]))
        .alignment(ratatui::layout::Alignment::Right),
        top[1],
    );

    frame.render_widget(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_set(glyphs.border)
            .border_style(theme.border),
        rows[1],
    );
    for (screen, item_area) in &layout.nav_items {
        let mut style = if *screen == app.screen {
            theme.nav_active
        } else {
            theme.nav_inactive
        };
        if app.hover == Some(HoverTarget::Navigation(*screen)) {
            style = style.patch(theme.hovered);
        }
        if app.focus == FocusArea::Navigation && app.nav_focus == *screen {
            style = style.patch(theme.focused);
        }
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {} ", screen.label()),
                style,
            ))),
            *item_area,
        );
    }
}

pub fn render_command_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
) {
    let focused = if app.command_active
        || app.focus == FocusArea::Command
        || app.hover == Some(HoverTarget::CommandBar)
    {
        theme.accent
    } else {
        theme.border
    };
    let input = if app.command_active {
        if app.command_input.is_empty() {
            Line::from(Span::styled("Type a /command", theme.hint))
        } else {
            let window = input_window(
                &app.command_input,
                app.command_cursor,
                area.width.saturating_sub(4) as usize,
            );
            Line::from(Span::styled(window.text, theme.command))
        }
    } else if let Some(notice) = &app.notice {
        Line::from(vec![
            Span::styled("!  ", theme.warning),
            Span::styled(
                marquee_text(
                    notice,
                    notice_width(area.width),
                    app.marquee_animation_frame / 3,
                ),
                theme.text,
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(glyphs.command, theme.accent),
            Span::styled("  Press / to run a command", theme.hint),
        ])
    };
    frame.render_widget(
        Paragraph::new(input).block(
            Block::default()
                .borders(if area.height < 3 {
                    Borders::NONE
                } else {
                    Borders::TOP | Borders::BOTTOM
                })
                .border_set(glyphs.border)
                .border_style(focused)
                .padding(Padding::horizontal(1)),
        ),
        area,
    );
}

pub fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, glyphs: &Glyphs) {
    let hints = if app.overlay == Some(crate::app::Overlay::ProfileEngine) {
        vec![
            hint("Enter", "create", theme),
            hint("Esc", "cancel", theme),
            hint(glyphs.up_down, "select engine", theme),
        ]
    } else if app.overlay == Some(crate::app::Overlay::ModelRuntime) {
        vec![
            hint("Enter", "apply / search", theme),
            hint("Esc", "cancel", theme),
            hint("D", "details", theme),
            hint(glyphs.up_down, "select", theme),
            hint("s", "search available", theme),
            hint("x/Del", "clear override", theme),
            hint("wheel", "scroll", theme),
        ]
    } else if app.overlay == Some(crate::app::Overlay::RuntimeSearch) {
        vec![
            hint("Enter/i", "install", theme),
            hint("Esc", "close", theme),
            hint("D", "details", theme),
            hint("Tab", "query/results", theme),
            hint(glyphs.up_down, "select", theme),
        ]
    } else if app.command_active {
        vec![
            hint(glyphs.up_down, "select", theme),
            hint("Enter", "run", theme),
            hint("Esc", "cancel", theme),
        ]
    } else if app.focus == FocusArea::Content && app.screen == Screen::Models {
        if app.downloads_focused {
            vec![
                hint("J/Esc", "back", theme),
                hint(glyphs.up_down, "job", theme),
                if app.selected_model_download_job_is_controllable() {
                    hint("p/x", "pause/cancel", theme)
                } else {
                    hint("D", "diagnostics", theme)
                },
            ]
        } else {
            models_footer(app, area.width, theme, glyphs)
        }
    } else {
        match (app.focus, app.screen) {
            (FocusArea::Navigation, _) => vec![
                hint("Left/Right", "focus", theme),
                hint("Enter", "open", theme),
                hint("Tab", "content", theme),
                hint("/", "commands", theme),
                hint("?", "help", theme),
            ],
            (FocusArea::Content, crate::app::Screen::ModelProfiles)
                if app.selected_remote_profile().is_some() =>
            {
                vec![
                    hint("Left/Right", "profile", theme),
                    hint(glyphs.up_down, "scroll", theme),
                    hint("l", "load on host", theme),
                    hint("u", "unload on host", theme),
                    hint("/", "commands", theme),
                ]
            }
            (FocusArea::Content, crate::app::Screen::ModelProfiles) => vec![
                hint("Left/Right", "profile", theme),
                hint(glyphs.up_down, "setting", theme),
                hint("Enter", "edit", theme),
                hint("e", "change engine", theme),
                hint("C", "capabilities", theme),
                hint("l", "load profile", theme),
                hint("u", "unload active", theme),
                hint("Delete", "inherit", theme),
                hint("?", "help", theme),
            ],
            (FocusArea::Content, crate::app::Screen::Logs) => vec![
                hint(glyphs.up_down, "scroll", theme),
                hint("End", "follow", theme),
                hint("Tab", "focus", theme),
                hint("/", "commands", theme),
            ],
            (FocusArea::Content, crate::app::Screen::Runtimes) => vec![
                hint(glyphs.up_down, "select", theme),
                hint("s", "search", theme),
                hint("U", "update selected", theme),
                hint("d d", "remove", theme),
                hint("r", "refresh", theme),
                hint("g", "GGUF default", theme),
                hint("Q/2", "Q27 default", theme),
                hint("N/3", "NInfer default", theme),
                hint("u", "updates", theme),
                hint("wheel", "scroll", theme),
                hint("?", "help", theme),
            ],
            (FocusArea::Content, crate::app::Screen::Settings) => vec![
                hint("Left/Right", "scope", theme),
                hint(glyphs.up_down, "setting", theme),
                hint("Enter", "edit", theme),
                hint("Delete", "inherit", theme),
                hint("?", "help", theme),
            ],
            _ => vec![
                hint("Tab", "change focus", theme),
                hint("/", "commands", theme),
                hint("?", "help", theme),
                hint("mouse", "click / hover", theme),
                hint("Ctrl+C", "exit", theme),
            ],
        }
    };
    let line = fit_footer_hints(hints, area.width);
    frame.render_widget(Paragraph::new(Line::from(line)), area);
}

pub(super) fn notice_width(area_width: u16) -> usize {
    let content_width = area_width.saturating_sub(4);
    remaining_width(content_width, &["!  "])
}

fn fit_footer_hints<'a>(hints: Vec<Vec<Span<'a>>>, width: u16) -> Vec<Span<'a>> {
    let mut used = 0usize;
    let mut line = Vec::new();
    for hint in hints {
        let hint_width = hint
            .iter()
            .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
            .sum::<usize>();
        if used.saturating_add(hint_width) > width as usize {
            continue;
        }
        used = used.saturating_add(hint_width);
        line.extend(hint);
    }
    line
}

fn models_footer<'a>(
    app: &'a App,
    width: u16,
    theme: &'a Theme,
    glyphs: &'a Glyphs,
) -> Vec<Vec<Span<'a>>> {
    if app.model_library_view == ModelLibraryView::Discover && app.model_search_editing {
        let finish_label = if width < 60 { "done" } else { "finish editing" };
        return vec![
            hint("Type", "query", theme),
            hint("Enter", "search", theme),
            hint("Esc", finish_label, theme),
        ];
    }

    let compact = width < 84;
    if app.model_library_view == ModelLibraryView::Installed {
        let mut line = if compact {
            vec![hint(
                if glyphs.unicode { "→/s" } else { ">/s" },
                "Discover",
                theme,
            )]
        } else {
            vec![
                hint("Left/Right", "view", theme),
                hint("s", "Discover", theme),
            ]
        };
        if !app.snapshot.models.is_empty() {
            line.push(hint(glyphs.up_down, "select", theme));
        }
        if let Some(model) = app
            .selected_model
            .and_then(|index| app.snapshot.models.get(index))
        {
            line.push(hint("Enter/c", "profile", theme));
            if !compact {
                line.push(hint("v", "runtime", theme));
            }
            let is_active = app.control.as_ref().is_some_and(|control| {
                control
                    .backends
                    .iter()
                    .any(|backend| backend.model_id == model.id)
            });
            if width >= 90 && !app.model_removal_busy() && model.provenance.is_some() && !is_active
            {
                line.push(hint("d", "remove", theme));
            }
        }
        if app.selected_model_download_job_is_controllable() {
            line.push(hint("p", "pause/resume", theme));
            line.push(hint("x", "cancel download", theme));
            if !compact {
                line.push(hint("Shift+Up/Down", "select download", theme));
            }
        }
        line.push(hint("?", "help", theme));
        return line;
    }

    if !compact {
        let mut line = vec![
            hint("Left/Right", "view", theme),
            hint("e", "search", theme),
            hint("f", "format", theme),
            hint("Up/Down", "select", theme),
            hint("d", "download", theme),
        ];
        if app.selected_model_download_job_is_controllable() {
            line.push(hint("p", "pause/resume", theme));
            line.push(hint("x", "cancel", theme));
            line.push(hint("Shift+Up/Down", "select download", theme));
        }
        line.push(hint("?", "help", theme));
        return line;
    }

    let has_results = !app.model_search_artifacts().is_empty();
    let has_selection = app.selected_model_search_result.is_some() && has_results;
    let mut line = vec![
        hint(
            if glyphs.unicode { "←/i" } else { "</i" },
            "Installed",
            theme,
        ),
        hint("e", "search", theme),
        hint("f", "format", theme),
    ];
    if has_selection {
        line.push(hint("d", "download", theme));
    }
    if app.selected_model_download_job_is_controllable() {
        line.push(hint("p", "pause/resume", theme));
        line.push(hint("x", "cancel", theme));
    }
    line.push(hint("?", "help", theme));
    line
}

pub fn set_command_cursor(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if !app.command_active {
        return;
    }
    let window = input_window(
        &app.command_input,
        app.command_cursor,
        area.width.saturating_sub(4) as usize,
    );
    let cursor_x = area
        .x
        .saturating_add(2)
        .saturating_add(window.cursor_column)
        .min(area.right().saturating_sub(2));
    frame.set_cursor_position(Position::new(
        cursor_x,
        area.y + u16::from(area.height >= 3),
    ));
}
