mod command_palette;
mod components;
pub(crate) mod layout;
mod model_runtime;
mod profile_engine;
mod runtime_search;
pub(crate) mod screens;
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

    if let Some(text) = &app.detail_text {
        let body = ratatui::layout::Rect::new(
            area.x + 1,
            area.y + 2,
            area.width.saturating_sub(2),
            area.height.saturating_sub(3),
        );
        let paragraph = ratatui::widgets::Paragraph::new(text.as_str())
            .wrap(ratatui::widgets::Wrap { trim: false });
        let max = paragraph
            .line_count(body.width)
            .saturating_sub(body.height as usize)
            .min(u16::MAX as usize) as u16;
        app.detail_scroll = app.detail_scroll.min(max);
        frame.render_widget(
            ratatui::widgets::Paragraph::new("[ Esc / D: Back ]  Details  |  Up/Down: scroll")
                .style(theme.accent),
            ratatui::layout::Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1),
        );
        frame.render_widget(paragraph.scroll((app.detail_scroll, 0)), body);
        return UiLayout {
            content: area,
            ..UiLayout::default()
        };
    }

    if app
        .settings_input
        .as_ref()
        .is_some_and(|i| i.editor.is_some())
    {
        screens::render_settings_input(frame, app, &theme, &layout);
        return layout;
    }
    shell::render_header(frame, app, &theme, &glyphs, &layout);
    screens::render_screen(frame, app, &theme, &glyphs, &layout);
    if layout.model_jobs_action.height > 0 {
        frame.render_widget(
            ratatui::widgets::Paragraph::new(if app.downloads_focused {
                "[ Esc Back ]"
            } else {
                "[ J Jobs ]"
            })
            .style(
                if app.hover == Some(layout::HoverTarget::ModelDownloadsView) {
                    theme.accent.patch(theme.hovered)
                } else {
                    theme.accent
                },
            ),
            layout.model_jobs_action,
        );
    }
    if layout.inspection_action.height > 0 {
        frame.render_widget(
            ratatui::widgets::Paragraph::new("[ D Details ]").style(
                if app.hover == Some(layout::HoverTarget::InspectionDetails) {
                    theme.accent.patch(theme.hovered)
                } else {
                    theme.accent
                },
            ),
            layout.inspection_action,
        );
    }
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
