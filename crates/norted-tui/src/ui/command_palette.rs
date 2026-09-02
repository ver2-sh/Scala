use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Paragraph, Wrap};

use crate::app::{App, Overlay};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{ActionState, action_style, popup_block};
use crate::ui::layout::{HoverTarget, UiLayout};
use crate::ui::screens::help_lines;

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
        frame.render_widget(
            Paragraph::new(help_lines(theme, glyphs))
                .block(popup_block(" Help ", theme, glyphs))
                .wrap(Wrap { trim: true }),
            area,
        );
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
            let mut style = if index == app.suggestion_index {
                theme.selected
            } else {
                Style::default()
            };
            if app.hover == Some(HoverTarget::CommandSuggestion(index)) {
                style = style.patch(theme.hovered);
            }
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<12}", command.name), theme.accent),
                Span::styled(command.description, theme.muted),
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
        List::new(items).block(popup_block(title, theme, glyphs)),
        area,
    );
}
