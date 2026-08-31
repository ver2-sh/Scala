use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::app::{App, Overlay};
use crate::theme::{Glyphs, Theme};

pub fn render(frame: &mut Frame<'_>, app: &App, theme: &Theme, glyphs: &Glyphs) {
    if app.overlay != Some(Overlay::ProfileEngine) {
        return;
    }
    let Some(selection) = &app.profile_engine_selection else {
        return;
    };
    let area = centered(
        frame.area(),
        68,
        (selection.engines.len() as u16).saturating_add(6),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .title(" Choose Model Profile engine ")
            .borders(Borders::ALL)
            .border_set(glyphs.border)
            .border_style(theme.accent),
        area,
    );
    let inner = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(&selection.model.display_name, theme.text),
            Span::styled(" · select the engine this profile binds", theme.muted),
        ])),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let items = selection.engines.iter().enumerate().map(|(index, engine)| {
        let label = if engine.as_str() == "ninfer" {
            "NInfer"
        } else {
            engine.as_str()
        };
        ListItem::new(format!("  {label}")).style(if selection.selected == index {
            theme.selected
        } else {
            theme.text
        })
    });
    frame.render_widget(
        List::new(items),
        Rect::new(
            inner.x,
            inner.y.saturating_add(2),
            inner.width,
            selection.engines.len() as u16,
        ),
    );
    frame.render_widget(
        Paragraph::new("↑/↓ or j/k selects · Enter creates · Esc cancels").style(theme.hint),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}

fn centered(area: Rect, maximum_width: u16, requested_height: u16) -> Rect {
    let width = maximum_width.min(area.width.saturating_sub(4)).max(1);
    let height = requested_height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}
