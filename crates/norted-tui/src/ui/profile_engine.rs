use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::app::{App, Overlay};
use crate::theme::{Glyphs, Theme};
use crate::ui::components::{
    ActionState, action_style, marquee_text, remaining_width, truncate_middle,
};
use crate::ui::layout::{HoverTarget, UiLayout};

const DETAILED_HEADING_SUFFIX: &str = " · select the engine this profile binds";
const COMPACT_HEADING_SUFFIX: &str = " · choose engine";
const MIN_DETAILED_NAME_WIDTH: usize = 12;

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
        area.x.saturating_add(if layout.compact { 2 } else { 3 }),
        area.y.saturating_add(if layout.compact { 1 } else { 2 }),
        area.width
            .saturating_sub(if layout.compact { 4 } else { 6 }),
        area.height
            .saturating_sub(if layout.compact { 2 } else { 4 }),
    );
    let suffix = heading_suffix(inner.width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                marquee_text(
                    &selection.model.display_name,
                    heading_name_width(inner.width),
                    app.marquee_animation_frame / 3,
                ),
                theme.text,
            ),
            Span::styled(suffix, theme.muted),
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
        let row_width = layout
            .profile_engine_rows
            .get(index)
            .map_or(0, |(_, area)| area.width as usize);
        let mut lines = vec![Line::from(truncate_middle(
            &format!("  {label}"),
            row_width,
            glyphs.ellipsis,
        ))];
        if layout
            .profile_engine_rows
            .get(index)
            .is_some_and(|(_, area)| area.height > 1)
        {
            lines.push(Line::default());
        }
        ListItem::new(lines).style(style)
    });
    let list_area = layout
        .profile_engine_rows
        .first()
        .zip(layout.profile_engine_rows.last())
        .map_or(Rect::default(), |((_, first), (_, last))| {
            Rect::new(first.x, first.y, first.width, last.bottom() - first.y)
        });
    frame.render_widget(List::new(items), list_area);
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

pub(super) fn popup_inner_width(popup_width: u16, compact: bool) -> u16 {
    popup_width.saturating_sub(if compact { 4 } else { 6 })
}

pub(super) fn heading_name_width(inner_width: u16) -> usize {
    remaining_width(inner_width, &[heading_suffix(inner_width)])
}

fn heading_suffix(inner_width: u16) -> &'static str {
    if remaining_width(inner_width, &[DETAILED_HEADING_SUFFIX]) >= MIN_DETAILED_NAME_WIDTH {
        DETAILED_HEADING_SUFFIX
    } else {
        COMPACT_HEADING_SUFFIX
    }
}
