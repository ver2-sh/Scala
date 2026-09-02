use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::app::{App, Overlay};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{ActionState, action_style};
use crate::ui::layout::{HoverTarget, UiLayout};

pub fn render(frame: &mut Frame<'_>, app: &App, theme: &Theme, glyphs: &Glyphs, layout: &UiLayout) {
    if app.overlay != Some(Overlay::ProfileEngine) {
        return;
    }
    let Some(selection) = &app.profile_engine_selection else {
        return;
    };
    let Some(area) = layout.profile_engine_popup else {
        return;
    };
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
        let mut style = if selection.selected == index {
            theme.selected
        } else {
            theme.text
        };
        if app.hover == Some(HoverTarget::ProfileEngineResult(index)) {
            style = style.patch(theme.hovered);
        }
        ListItem::new(format!("  {label}")).style(style)
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
        Paragraph::new(Line::from(Span::styled(
            "[ Create ]",
            action_style(
                theme,
                ActionState::Primary,
                app.hover == Some(HoverTarget::ProfileEngineApply),
            ),
        ))),
        layout.profile_engine_apply,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "[ Cancel ]",
            action_style(
                theme,
                ActionState::Normal,
                app.hover == Some(HoverTarget::ProfileEngineCancel),
            ),
        ))),
        layout.profile_engine_cancel,
    );
}
