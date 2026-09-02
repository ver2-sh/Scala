use ratatui::Frame;
use ratatui::layout::Margin;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Paragraph};

use crate::app::{App, Overlay};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{
    ActionState, action_style, marquee_text, popup_block, truncate_middle,
};
use crate::ui::layout::{HoverTarget, UiLayout};
use crate::ui::screens::render_help_body;

pub fn render_overlays(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    layout: &UiLayout,
) {
    if app.overlay == Some(Overlay::Help) {
        let Some(area) = layout.help_popup else {
            return;
        };
        frame.render_widget(Clear, area);
        frame.render_widget(popup_block(" Help ", theme, glyphs, layout.compact), area);
        let mut body = area.inner(Margin {
            horizontal: if layout.compact { 2 } else { 3 },
            vertical: if layout.compact { 1 } else { 2 },
        });
        body.height = body.height.saturating_sub(1);
        render_help_body(frame, body, theme, glyphs, layout.compact);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "[ Close ]",
                action_style(
                    theme,
                    ActionState::Primary,
                    app.hover == Some(HoverTarget::HelpClose),
                ),
            ))),
            layout.help_close,
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
    let Some(area) = layout.suggestion_popup else {
        return;
    };
    let visible_count = layout.suggestion_rows.len();
    frame.render_widget(Clear, area);
    let end = (app.suggestion_scroll + visible_count).min(suggestions.len());
    let items = suggestions[app.suggestion_scroll..end]
        .iter()
        .enumerate()
        .map(|(visible_index, command)| {
            let index = app.suggestion_scroll + visible_index;
            let row_width = layout
                .suggestion_rows
                .get(visible_index)
                .map_or(0, |(_, row)| row.width as usize);
            let name_width = if layout.compact { 12 } else { 18 };
            let name = format!("{:<name_width$}  ", command.name);
            let description_width = row_width.saturating_sub(name.len());
            let mut style = if index == app.suggestion_index {
                theme.selected
            } else {
                Style::default()
            };
            if app.hover == Some(HoverTarget::CommandSuggestion(index)) {
                style = style.patch(theme.hovered);
            }
            let description = if index == app.suggestion_index
                || app.hover == Some(HoverTarget::CommandSuggestion(index))
            {
                marquee_text(
                    command.description,
                    description_width,
                    app.ui_animation_frame / 3,
                )
            } else {
                truncate_middle(command.description, description_width, glyphs.ellipsis)
            };
            ListItem::new(Line::from(vec![
                Span::styled(name, theme.accent),
                Span::styled(description, theme.muted),
            ]))
            .style(style)
        });
    let title = match (app.suggestion_scroll > 0, end < suggestions.len()) {
        (true, true) => " Commands - more above/below ",
        (true, false) => " Commands - more above ",
        (false, true) => " Commands - more below ",
        (false, false) => " Commands ",
    };
    frame.render_widget(
        List::new(items).block(popup_block(title, theme, glyphs, layout.compact)),
        area,
    );
}
