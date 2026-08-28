use norted_core::RegistryState;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Position, Rect};

use crate::app::{App, Overlay, Screen};

use super::components::content_layout;
use super::shell::{COMPACT_WIDTH, MIN_HEIGHT, MIN_WIDTH};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HoverTarget {
    Navigation(Screen),
    Model(usize),
    Runtime(usize),
    RuntimeSearchAction,
    RuntimeUpdateAction,
    RuntimeSearchInput,
    RuntimeSearchResult(usize),
    RuntimeSearchSubmit,
    RuntimeInstall,
    RuntimePickerResult(usize),
    RuntimePickerApply,
    SettingsScope(usize),
    Setting(usize),
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
    pub runtime_summary: Rect,
    pub runtime_list: Rect,
    pub runtime_rows: Vec<(usize, Rect)>,
    pub runtime_actions: Rect,
    pub runtime_search_action: Rect,
    pub runtime_update_action: Rect,
    pub runtime_search_popup: Option<Rect>,
    pub runtime_search_input: Rect,
    pub runtime_search_results: Rect,
    pub runtime_search_rows: Vec<(usize, Rect)>,
    pub runtime_search_details: Rect,
    pub runtime_search_submit: Rect,
    pub runtime_install_action: Rect,
    pub runtime_operation_status: Rect,
    pub runtime_picker_active: bool,
    pub logs: Rect,
    pub settings_scopes: Rect,
    pub settings_list: Rect,
    pub settings_scope_rows: Vec<(usize, Rect)>,
    pub settings_rows: Vec<(usize, Rect)>,
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

        let mut runtime_summary = Rect::default();
        let mut runtime_list = Rect::default();
        let mut runtime_actions = Rect::default();
        let mut runtime_search_action = Rect::default();
        let mut runtime_update_action = Rect::default();
        let mut runtime_rows = Vec::new();
        if app.screen == Screen::Runtimes {
            let summary_height = screen_body.height.min(if compact { 3 } else { 4 });
            let action_height = u16::from(screen_body.height > summary_height);
            runtime_summary = Rect::new(
                screen_body.x,
                screen_body.y,
                screen_body.width,
                summary_height,
            );
            runtime_actions = Rect::new(
                screen_body.x,
                screen_body.bottom().saturating_sub(action_height),
                screen_body.width,
                action_height,
            );
            runtime_list = Rect::new(
                screen_body.x,
                runtime_summary.bottom(),
                screen_body.width,
                runtime_actions.y.saturating_sub(runtime_summary.bottom()),
            );
            if action_height > 0 {
                runtime_search_action = Rect::new(
                    runtime_actions.x,
                    runtime_actions.y,
                    runtime_actions.width.min(20),
                    1,
                );
                runtime_update_action = Rect::new(
                    runtime_search_action.right().saturating_add(1),
                    runtime_actions.y,
                    runtime_actions
                        .right()
                        .saturating_sub(runtime_search_action.right().saturating_add(1))
                        .min(18),
                    1,
                );
            }
            if let Some(snapshot) = &app.runtime_list {
                let capacity = (runtime_list.height / 2) as usize;
                let end = (app.runtime_scroll + capacity).min(snapshot.installed.len());
                for index in app.runtime_scroll..end {
                    runtime_rows.push((
                        index,
                        Rect::new(
                            runtime_list.x,
                            runtime_list.y + ((index - app.runtime_scroll) as u16 * 2),
                            runtime_list.width,
                            2,
                        ),
                    ));
                }
            }
        }

        let mut runtime_search_popup = None;
        let mut runtime_search_input = Rect::default();
        let mut runtime_search_results = Rect::default();
        let mut runtime_search_details = Rect::default();
        let mut runtime_search_submit = Rect::default();
        let mut runtime_install_action = Rect::default();
        let mut runtime_operation_status = Rect::default();
        let mut runtime_search_rows = Vec::new();
        let runtime_picker_active = app.overlay == Some(Overlay::ModelRuntime);
        if matches!(
            app.overlay,
            Some(Overlay::RuntimeSearch | Overlay::ModelRuntime)
        ) {
            let horizontal_margin = if area.width < COMPACT_WIDTH {
                1
            } else {
                (area.width / 16).max(2)
            };
            let vertical_margin = if area.height < 20 { 1 } else { 2 };
            let popup = area.inner(Margin {
                horizontal: horizontal_margin,
                vertical: vertical_margin,
            });
            runtime_search_popup = Some(popup);
            let inner = popup.inner(Margin {
                horizontal: 2,
                vertical: 1,
            });
            if runtime_picker_active {
                runtime_search_input =
                    Rect::new(inner.x, inner.y, inner.width, u16::from(inner.height > 0));
            } else {
                let submit_width = inner.width.min(12);
                runtime_search_input = Rect::new(
                    inner.x,
                    inner.y,
                    inner.width.saturating_sub(submit_width.saturating_add(1)),
                    u16::from(inner.height > 0),
                );
                runtime_search_submit = Rect::new(
                    runtime_search_input.right().saturating_add(1),
                    inner.y,
                    submit_width,
                    u16::from(inner.height > 0),
                );
            }
            let body = Rect::new(
                inner.x,
                inner.y.saturating_add(2),
                inner.width,
                inner.height.saturating_sub(3),
            );
            if inner.width >= COMPACT_WIDTH {
                let result_width = body.width.saturating_mul(3) / 5;
                runtime_search_results = Rect::new(body.x, body.y, result_width, body.height);
                runtime_search_details = Rect::new(
                    body.x.saturating_add(result_width).saturating_add(2),
                    body.y,
                    body.width.saturating_sub(result_width.saturating_add(2)),
                    body.height,
                );
            } else {
                let result_height = body.height.saturating_mul(3) / 5;
                runtime_search_results = Rect::new(body.x, body.y, body.width, result_height);
                runtime_search_details = Rect::new(
                    body.x,
                    body.y.saturating_add(result_height),
                    body.width,
                    body.height.saturating_sub(result_height),
                );
            }
            runtime_install_action = Rect::new(
                inner.x,
                inner.bottom().saturating_sub(1),
                inner.width.min(22),
                u16::from(inner.height > 0),
            );
            runtime_operation_status = Rect::new(
                runtime_install_action.right().saturating_add(1),
                runtime_install_action.y,
                inner
                    .right()
                    .saturating_sub(runtime_install_action.right().saturating_add(1)),
                runtime_install_action.height,
            );
            let (indices, scroll) = if runtime_picker_active {
                (app.runtime_picker_indices(), app.runtime_picker_scroll)
            } else {
                (app.runtime_search_indices(), app.runtime_search_scroll)
            };
            let capacity = (runtime_search_results.height / 2) as usize;
            let end = (scroll + capacity).min(indices.len());
            for (visible_position, index) in indices
                .iter()
                .enumerate()
                .skip(scroll)
                .take(end.saturating_sub(scroll))
            {
                runtime_search_rows.push((
                    *index,
                    Rect::new(
                        runtime_search_results.x,
                        runtime_search_results.y + ((visible_position - scroll) as u16 * 2),
                        runtime_search_results.width,
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

        let mut settings_scopes = Rect::default();
        let mut settings_list = Rect::default();
        let mut settings_scope_rows = Vec::new();
        let mut settings_rows = Vec::new();
        if app.screen == Screen::Settings {
            settings_scopes = Rect::new(screen_body.x, screen_body.y, screen_body.width, 2);
            settings_list = Rect::new(
                screen_body.x,
                screen_body.y.saturating_add(4),
                screen_body.width,
                screen_body.height.saturating_sub(4),
            );
            let scopes = app.settings_scopes();
            let mut x = settings_scopes.x;
            for (index, scope) in scopes.iter().enumerate() {
                let label = match scope {
                    crate::app::SettingsScope::Global => "Global".to_owned(),
                    crate::app::SettingsScope::Engine(engine) => engine.clone(),
                    crate::app::SettingsScope::Profile(profile) => profile.to_string(),
                    crate::app::SettingsScope::Model(_) => "Selected model".to_owned(),
                };
                let width = (label.chars().count() as u16 + 2)
                    .min(settings_scopes.right().saturating_sub(x));
                if width == 0 {
                    break;
                }
                settings_scope_rows.push((index, Rect::new(x, settings_scopes.y, width, 1)));
                x = x.saturating_add(width);
            }
            let definitions = app.settings_definitions();
            let capacity = (settings_list.height / 2) as usize;
            let end = (app.settings_scroll + capacity).min(definitions.len());
            for index in app.settings_scroll..end {
                settings_rows.push((
                    index,
                    Rect::new(
                        settings_list.x,
                        settings_list.y + ((index - app.settings_scroll) as u16 * 2),
                        settings_list.width,
                        2,
                    ),
                ));
            }
        }

        Self {
            too_small: false,
            compact,
            nav_items,
            content,
            model_rows,
            runtime_summary,
            runtime_list,
            runtime_rows,
            runtime_actions,
            runtime_search_action,
            runtime_update_action,
            runtime_search_popup,
            runtime_search_input,
            runtime_search_results,
            runtime_search_rows,
            runtime_search_details,
            runtime_search_submit,
            runtime_install_action,
            runtime_operation_status,
            runtime_picker_active,
            logs: if app.screen == Screen::Logs {
                screen_body
            } else {
                Rect::default()
            },
            settings_scopes,
            settings_list,
            settings_scope_rows,
            settings_rows,
            command_bar: regions[2],
            suggestion_popup,
            suggestion_rows,
            footer: regions[3],
            header: regions[0],
        }
    }

    pub fn hit_test(&self, position: Position) -> Option<HoverTarget> {
        if self.runtime_search_popup.is_some() {
            if !self.runtime_picker_active && contains(self.runtime_search_input, position) {
                return Some(HoverTarget::RuntimeSearchInput);
            }
            if !self.runtime_picker_active && contains(self.runtime_search_submit, position) {
                return Some(HoverTarget::RuntimeSearchSubmit);
            }
            if contains(self.runtime_install_action, position) {
                return Some(if self.runtime_picker_active {
                    HoverTarget::RuntimePickerApply
                } else {
                    HoverTarget::RuntimeInstall
                });
            }
            if let Some((index, _)) = self
                .runtime_search_rows
                .iter()
                .find(|(_, area)| contains(*area, position))
            {
                return Some(if self.runtime_picker_active {
                    HoverTarget::RuntimePickerResult(*index)
                } else {
                    HoverTarget::RuntimeSearchResult(*index)
                });
            }
            return None;
        }
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
        if let Some((index, _)) = self
            .runtime_rows
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::Runtime(*index));
        }
        if let Some((index, _)) = self
            .settings_scope_rows
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::SettingsScope(*index));
        }
        if let Some((index, _)) = self
            .settings_rows
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::Setting(*index));
        }
        if contains(self.runtime_search_action, position) {
            return Some(HoverTarget::RuntimeSearchAction);
        }
        if contains(self.runtime_update_action, position) {
            return Some(HoverTarget::RuntimeUpdateAction);
        }
        contains(self.command_bar, position).then_some(HoverTarget::CommandBar)
    }

    pub fn model_capacity(&self) -> usize {
        (self.content.height.saturating_sub(3) / 2) as usize
    }

    pub fn log_capacity(&self) -> usize {
        self.logs.height as usize
    }

    pub fn runtime_capacity(&self) -> usize {
        (self.runtime_list.height / 2) as usize
    }

    pub fn runtime_search_capacity(&self) -> usize {
        (self.runtime_search_results.height / 2) as usize
    }

    pub fn settings_capacity(&self) -> usize {
        (self.settings_list.height / 2) as usize
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
