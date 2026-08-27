use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Screen};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{centered_message, hint};

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
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    compact: bool,
) {
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

    let nav_line = if compact {
        Line::from(vec![
            Span::styled(format!("{}  ", glyphs.previous), theme.hint),
            Span::styled(app.screen.label(), theme.nav_active),
            Span::styled(format!("  {}", glyphs.next), theme.hint),
        ])
    } else {
        let mut spans = Vec::new();
        for (index, screen) in Screen::ALL.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw("   "));
            }
            spans.push(Span::styled(
                screen.label(),
                if *screen == app.screen {
                    theme.nav_active
                } else {
                    theme.nav_inactive
                },
            ));
        }
        Line::from(spans)
    };
    frame.render_widget(
        Paragraph::new(nav_line).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_set(glyphs.border)
                .border_style(theme.border),
        ),
        rows[1],
    );
}

pub fn render_command_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
) {
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
    } else {
        vec![
            hint("Tab", "navigate", theme),
            hint("/", "commands", theme),
            hint("?", "help", theme),
            hint("Ctrl+C", "exit", theme),
        ]
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
