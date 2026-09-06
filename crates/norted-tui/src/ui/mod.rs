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

pub fn render(frame: &mut Frame<'_>, app: &mut App) -> UiLayout {
    let area = frame.area();
    let layout = UiLayout::calculate(area, app);
    if matches!(
        app.screen,
        crate::app::Screen::Settings | crate::app::Screen::ModelProfiles
    ) && layout.settings_detail.height > 1
        && let Some(detail) = screens::selected_setting_detail(app)
    {
        let lines = ratatui::widgets::Paragraph::new(detail)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .line_count(layout.settings_detail.width);
        let max_scroll =
            lines.saturating_sub(layout.settings_detail.height.saturating_sub(1) as usize);
        app.settings_detail_scroll = app
            .settings_detail_scroll
            .min(max_scroll.min(u16::MAX as usize) as u16);
    }
    app.sync_marquee_target(layout.active_marquee_target(app));
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
    profile_engine::render(frame, app, &theme, &glyphs, &layout);
    if app.overlay.is_none() {
        shell::set_command_cursor(frame, layout.command_bar, app);
    }
    layout
}
