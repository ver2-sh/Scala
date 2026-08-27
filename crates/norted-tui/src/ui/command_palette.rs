use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Paragraph, Wrap};

use crate::app::{App, Overlay};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{centered_rect, popup_block};
use crate::ui::screens::help_lines;

pub fn render_overlays(
    frame: &mut Frame<'_>,
    app: &App,
    theme: &Theme,
    glyphs: &Glyphs,
    command_area: Rect,
) {
    if app.overlay == Some(Overlay::Help) {
        let area = centered_rect(74, 72, frame.area());
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(help_lines(theme, glyphs))
                .block(popup_block(" Help ", theme, glyphs))
                .wrap(Wrap { trim: true }),
            area,
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
    let visible_count = suggestions.len().min(8);
    let height = visible_count as u16 + 2;
    let area = Rect::new(
        command_area.x,
        command_area.y.saturating_sub(height),
        command_area.width,
        height,
    );
    frame.render_widget(Clear, area);
    let end = (app.suggestion_scroll + visible_count).min(suggestions.len());
    let items = suggestions[app.suggestion_scroll..end]
        .iter()
        .enumerate()
        .map(|(visible_index, command)| {
            let index = app.suggestion_scroll + visible_index;
            let style = if index == app.suggestion_index {
                theme.selected
            } else {
                Style::default()
            };
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
