mod command_palette;
mod components;
mod screens;
mod shell;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Margin};
use ratatui::widgets::Block;

use crate::app::App;
use crate::theme::{Glyphs, Theme};

pub fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let theme = Theme::current(app.no_color);
    let glyphs = Glyphs::current(app.unicode);
    frame.render_widget(Block::default().style(theme.text), area);
    if area.width < shell::MIN_WIDTH || area.height < shell::MIN_HEIGHT {
        shell::render_too_small(frame, area, &theme, &glyphs);
        return;
    }

    let compact = area.width < shell::COMPACT_WIDTH || area.height < 23;
    let shell_area = area.inner(Margin {
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
        .split(shell_area);

    shell::render_header(frame, regions[0], app, &theme, &glyphs, compact);
    let content = regions[1].inner(Margin {
        horizontal: if compact { 1 } else { 2 },
        vertical: 1,
    });
    screens::render_screen(frame, content, app, &theme, &glyphs, compact);
    shell::render_command_bar(frame, regions[2], app, &theme, &glyphs);
    shell::render_footer(frame, regions[3], app, &theme, &glyphs);
    command_palette::render_overlays(frame, app, &theme, &glyphs, regions[2]);
    shell::set_command_cursor(frame, regions[2], app);
}
