mod command_palette;
mod components;
pub(crate) mod layout;
mod model_runtime;
mod profile_engine;
mod runtime_search;
mod screens;
mod shell;

use ratatui::Frame;
use ratatui::widgets::Block;

use crate::app::App;
use crate::theme::{Glyphs, Theme};
use layout::UiLayout;

pub fn render(frame: &mut Frame<'_>, app: &App) -> UiLayout {
    let area = frame.area();
    let layout = UiLayout::calculate(area, app);
    let theme = Theme::current(app.no_color);
    let glyphs = Glyphs::current(app.unicode);
    frame.render_widget(Block::default().style(theme.text), area);
    if layout.too_small {
        shell::render_too_small(frame, area, &theme, &glyphs);
        return layout;
    }

    shell::render_header(frame, app, &theme, &glyphs, &layout);
    screens::render_screen(frame, app, &theme, &glyphs, &layout);
    shell::render_command_bar(frame, layout.command_bar, app, &theme, &glyphs);
    shell::render_footer(frame, layout.footer, app, &theme, &glyphs);
    command_palette::render_overlays(frame, app, &theme, &glyphs, &layout);
    runtime_search::render(frame, app, &theme, &glyphs, &layout);
    model_runtime::render(frame, app, &theme, &glyphs, &layout);
    profile_engine::render(frame, app, &theme, &glyphs);
    if app.overlay.is_none() {
        shell::set_command_cursor(frame, layout.command_bar, app);
    }
    layout
}
