use norted_core::RegistryState;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Position, Rect};

use crate::app::{App, Screen};

use super::components::content_layout;
use super::shell::{COMPACT_WIDTH, MIN_HEIGHT, MIN_WIDTH};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HoverTarget {
    Navigation(Screen),
    Model(usize),
    CommandSuggestion(usize),
    CommandBar,
}

#[derive(Debug, Clone, Default)]
pub struct UiLayout {
    pub too_small: bool,
    pub compact: bool,
    pub nav_items: Vec<(Screen, Rect)>,
    pub content: Rect,
    pub model_rows: Vec<(usize, Rect)>,
    pub logs: Rect,
    pub command_bar: Rect,
    pub suggestion_popup: Option<Rect>,
    pub suggestion_rows: Vec<(usize, Rect)>,
    pub footer: Rect,
    pub header: Rect,
}

impl UiLayout {
    pub fn calculate(area: Rect, app: &App) -> Self {
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            return Self {
                too_small: true,
                ..Self::default()
            };
        }

        let compact = area.width < COMPACT_WIDTH || area.height < 23;
        let shell_area = area.inner(Margin {
            horizontal: if compact { 1 } else { 2 },
            vertical: 0,
        });
        let regions = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(5),
                Constraint::Length(3),
                Constraint::Length(if area.height == MIN_HEIGHT { 1 } else { 2 }),
            ])
            .split(shell_area);
        let content = regions[1].inner(Margin {
            horizontal: if compact { 1 } else { 2 },
            vertical: 1,
        });
        let screen_body = content_layout(content)[1];
        let nav_items = nav_rects(regions[0], compact);

        let mut model_rows = Vec::new();
        if app.screen == Screen::Models
            && matches!(
                app.snapshot.registry_state,
                RegistryState::Ready | RegistryState::ReadyWithWarnings { .. }
            )
            && !app.snapshot.models.is_empty()
        {
            let capacity = (screen_body.height / 2) as usize;
            let end = (app.model_scroll + capacity).min(app.snapshot.models.len());
            for index in app.model_scroll..end {
                model_rows.push((
                    index,
                    Rect::new(
                        screen_body.x,
                        screen_body.y + ((index - app.model_scroll) as u16 * 2),
                        screen_body.width,
                        2,
                    ),
                ));
            }
        }

        let suggestions = app.suggestions();
        let (suggestion_popup, suggestion_rows) = if app.command_active && !suggestions.is_empty() {
            let visible_count = suggestions.len().min(8);
            let height = visible_count as u16 + 2;
            let popup = Rect::new(
                regions[2].x,
                regions[2].y.saturating_sub(height),
                regions[2].width,
                height,
            );
            let end = (app.suggestion_scroll + visible_count).min(suggestions.len());
            let rows = (app.suggestion_scroll..end)
                .map(|index| {
                    (
                        index,
                        Rect::new(
                            popup.x.saturating_add(1),
                            popup.y + 1 + (index - app.suggestion_scroll) as u16,
                            popup.width.saturating_sub(2),
                            1,
                        ),
                    )
                })
                .collect();
            (Some(popup), rows)
        } else {
            (None, Vec::new())
        };

        Self {
            too_small: false,
            compact,
            nav_items,
            content,
            model_rows,
            logs: if app.screen == Screen::Logs {
                screen_body
            } else {
                Rect::default()
            },
            command_bar: regions[2],
            suggestion_popup,
            suggestion_rows,
            footer: regions[3],
            header: regions[0],
        }
    }

    pub fn hit_test(&self, position: Position) -> Option<HoverTarget> {
        if let Some((index, _)) = self
            .suggestion_rows
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::CommandSuggestion(*index));
        }
        if let Some((screen, _)) = self
            .nav_items
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::Navigation(*screen));
        }
        if let Some((index, _)) = self
            .model_rows
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::Model(*index));
        }
        contains(self.command_bar, position).then_some(HoverTarget::CommandBar)
    }

    pub fn model_capacity(&self) -> usize {
        (self.content.height.saturating_sub(3) / 2) as usize
    }

    pub fn log_capacity(&self) -> usize {
        self.logs.height as usize
    }

    pub fn contains_content(&self, position: Position) -> bool {
        contains(self.content, position)
    }

    pub fn contains_suggestions(&self, position: Position) -> bool {
        self.suggestion_popup
            .is_some_and(|area| contains(area, position))
    }
}

fn nav_rects(header: Rect, compact: bool) -> Vec<(Screen, Rect)> {
    let nav = if compact {
        Rect::new(header.x, header.y + 1, header.width, 3)
    } else {
        Rect::new(header.x, header.y + 2, header.width, 2)
    };
    let rows: &[&[Screen]] = if compact {
        &[&Screen::ALL[..4], &Screen::ALL[4..]]
    } else {
        &[&Screen::ALL]
    };
    let mut result = Vec::with_capacity(Screen::ALL.len());
    for (row_index, screens) in rows.iter().enumerate() {
        let mut x = nav.x;
        for screen in *screens {
            let width = screen.label().len() as u16 + 2;
            result.push((*screen, Rect::new(x, nav.y + row_index as u16, width, 1)));
            x = x.saturating_add(width);
        }
    }
    result
}

fn contains(area: Rect, position: Position) -> bool {
    position.x >= area.x
        && position.x < area.right()
        && position.y >= area.y
        && position.y < area.bottom()
}
