use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, FocusArea, ModelLibraryView, Screen};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{centered_message, hint};
use crate::ui::layout::{HoverTarget, UiLayout};

pub const MIN_WIDTH: u16 = 46;
pub const MIN_HEIGHT: u16 = 13;
pub const COMPACT_WIDTH: u16 = 84;

pub fn render_too_small(frame: &mut Frame<'_>, area: Rect, theme: &Theme, glyphs: &Glyphs) {
    centered_message(
        frame,
        area,
        vec![
            Line::from(Span::styled("NORTED SERVER", theme.accent)),
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
            [Constraint::Length(2), Constraint::Length(2)]
        })
        .split(area);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(24)])
        .split(rows[0]);
    let mut brand = vec![
        Span::styled(glyphs.brand, theme.accent),
        Span::raw("  "),
        Span::styled("NORTED", theme.accent),
        Span::styled(" SERVER", theme.text),
    ];
    if !layout.compact {
        brand.push(Span::styled("  local runtime control", theme.muted));
    }
    frame.render_widget(Paragraph::new(Line::from(brand)), top[0]);
    let (marker, state_style) = match &app.snapshot.server {
        norted_core::ServerState::Unknown { .. } => ("?", theme.warning),
        norted_core::ServerState::Running { .. } => (glyphs.running, theme.success),
        norted_core::ServerState::Failed { .. } => ("!", theme.error),
        norted_core::ServerState::Starting | norted_core::ServerState::Stopping => {
            (glyphs.transitional, theme.warning)
        }
        norted_core::ServerState::Stopped => (glyphs.stopped, theme.muted),
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
            Line::from(Span::styled(&app.command_input, theme.command))
        }
    } else if let Some(notice) = &app.notice {
        Line::from(vec![
            Span::styled("!  ", theme.warning),
            Span::styled(notice, theme.text),
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
                .borders(Borders::TOP | Borders::BOTTOM)
                .border_set(glyphs.border)
                .border_style(focused)
                .padding(Padding::horizontal(1)),
        ),
        area,
    );
}

pub fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, glyphs: &Glyphs) {
    let line = if app.overlay == Some(crate::app::Overlay::ProfileEngine) {
        vec![
            hint(glyphs.up_down, "select engine", theme),
            hint("Enter", "create", theme),
            hint("Esc", "cancel", theme),
        ]
    } else if app.overlay == Some(crate::app::Overlay::ModelRuntime) {
        vec![
            hint(glyphs.up_down, "select", theme),
            hint("Enter", "apply / search", theme),
            hint("s", "search available", theme),
            hint("x/Del", "clear override", theme),
            hint("wheel", "scroll", theme),
            hint("Esc", "cancel", theme),
        ]
    } else if app.overlay == Some(crate::app::Overlay::RuntimeSearch) {
        vec![
            hint("Tab", "query/results", theme),
            hint(glyphs.up_down, "select", theme),
            hint("Enter/i", "install", theme),
            hint("Esc", "close", theme),
        ]
    } else if app.command_active {
        vec![
            hint(glyphs.up_down, "select", theme),
            hint("Enter", "run", theme),
            hint("Esc", "cancel", theme),
        ]
    } else if app.focus == FocusArea::Content && app.screen == Screen::Models {
        models_footer(app, area.width, theme, glyphs)
    } else if area.width < 60 {
        vec![
            hint("Tab", "focus", theme),
            hint("/", "commands", theme),
            hint("?", "help", theme),
        ]
    } else {
        match (app.focus, app.screen) {
            (FocusArea::Navigation, _) => vec![
                hint("Left/Right", "focus", theme),
                hint("Enter", "open", theme),
                hint("Tab", "content", theme),
                hint("/", "commands", theme),
            ],
            (FocusArea::Content, crate::app::Screen::ModelProfiles) => vec![
                hint("Left/Right", "profile", theme),
                hint(glyphs.up_down, "setting", theme),
                hint("Enter", "edit/cycle", theme),
                hint("Delete", "inherit", theme),
                hint("l", "load profile", theme),
                hint("u", "unload active", theme),
                hint("e", "change engine", theme),
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
                hint("g", "GGUF default", theme),
                hint("Q/2", "Q27 default", theme),
                hint("N/3", "NInfer default", theme),
                hint("u", "updates", theme),
                hint("U", "update selected", theme),
                hint("d d", "remove", theme),
                hint("r", "refresh", theme),
                hint("wheel", "scroll", theme),
            ],
            (FocusArea::Content, crate::app::Screen::Settings) => vec![
                hint("Left/Right", "scope", theme),
                hint(glyphs.up_down, "setting", theme),
                hint("Enter", "edit/cycle", theme),
                hint("Delete", "inherit", theme),
            ],
            _ => vec![
                hint("Tab", "change focus", theme),
                hint("mouse", "click / hover", theme),
                hint("/", "commands", theme),
                hint("?", "help", theme),
                hint("Ctrl+C", "exit", theme),
            ],
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(line.into_iter().flatten().collect::<Vec<_>>()))
            .wrap(Wrap { trim: true }),
        area,
    );
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
            let is_active = app
                .control
                .as_ref()
                .and_then(|control| control.backend.model_id.as_ref())
                == Some(&model.id);
            if width >= 90 && !app.model_library_busy() && model.provenance.is_some() && !is_active
            {
                line.push(hint("d", "remove", theme));
            }
        }
        return line;
    }

    if !compact {
        return vec![
            hint("Left/Right", "view", theme),
            hint("e", "search", theme),
            hint("f", "format", theme),
            hint("Up/Down", "select", theme),
            hint("d", "download", theme),
        ];
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
    if has_selection && !app.model_library_busy() {
        line.push(hint("d", "download", theme));
    }
    line
}

pub fn set_command_cursor(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if !app.command_active {
        return;
    }
    let byte_index = app
        .command_input
        .char_indices()
        .nth(app.command_cursor)
        .map(|(index, _)| index)
        .unwrap_or(app.command_input.len());
    let prefix_width = UnicodeWidthStr::width(&app.command_input[..byte_index]) as u16;
    let cursor_x = area
        .x
        .saturating_add(2)
        .saturating_add(prefix_width)
        .min(area.right().saturating_sub(2));
    frame.set_cursor_position(Position::new(cursor_x, area.y + 1));
}
