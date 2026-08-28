use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, FocusArea};
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
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(glyphs.brand, theme.accent),
            Span::raw("  "),
            Span::styled("NORTED", theme.accent),
            Span::styled(" SERVER", theme.text),
            Span::styled("  local runtime control", theme.muted),
        ])),
        top[0],
    );
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
    let line = if app.command_active {
        vec![
            hint(glyphs.up_down, "select", theme),
            hint("Enter", "run", theme),
            hint("Esc", "cancel", theme),
        ]
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
            (FocusArea::Content, crate::app::Screen::Models) => vec![
                hint(glyphs.up_down, "select", theme),
                hint("Enter", "load", theme),
                hint("u", "unload", theme),
                hint("wheel", "scroll", theme),
                hint("Tab", "focus", theme),
                hint("/", "commands", theme),
            ],
            (FocusArea::Content, crate::app::Screen::Logs) => vec![
                hint(glyphs.up_down, "scroll", theme),
                hint("End", "follow", theme),
                hint("Tab", "focus", theme),
                hint("/", "commands", theme),
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
        Paragraph::new(Line::from(line.into_iter().flatten().collect::<Vec<_>>())),
        area,
    );
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
