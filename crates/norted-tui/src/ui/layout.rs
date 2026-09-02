use norted_core::{ArtifactFormat, RegistryState, RuntimeAcquisitionMethod};
use ratatui::layout::{Constraint, Direction, Layout, Margin, Position, Rect};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, ModelLibraryView, Overlay, Screen};

use super::components::{content_layout, format_bytes, needs_marquee};
use super::shell::{COMPACT_WIDTH, MIN_HEIGHT, MIN_WIDTH};

const MODEL_ROW_HEIGHT_COMPACT: u16 = 3;
const MODEL_ROW_HEIGHT_COMFORTABLE: u16 = 4;
const TWO_LINE_ROW_HEIGHT_COMPACT: u16 = 2;
const TWO_LINE_ROW_HEIGHT_COMFORTABLE: u16 = 3;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InstalledModelAction {
    CreateProfile,
    Runtime,
    Unload,
    Remove,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ModelProfileAction {
    Load,
    Unload,
    Model,
    Engine,
    Role,
    Duplicate,
    Delete,
    Refresh,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SelectedRuntimeAction {
    Default(ArtifactFormat),
    Update,
    Remove,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HoverTarget {
    Navigation(Screen),
    ModelLibraryTab(ModelLibraryView),
    ModelSearchField,
    ModelSearchSubmit,
    ModelFormatFilter(Option<ArtifactFormat>),
    ModelDownloadAction(usize),
    Model(usize),
    InstalledModelAction(InstalledModelAction),
    Runtime(usize),
    RuntimeSearchAction,
    RuntimeUpdateAction,
    SelectedRuntimeAction(SelectedRuntimeAction),
    RuntimeSearchInput,
    RuntimeSearchIncompatibleToggle,
    RuntimeSearchResult(usize),
    RuntimeSearchSubmit,
    RuntimeInstall,
    RuntimeOverlayCancel,
    RuntimePickerResult(usize),
    RuntimePickerApply,
    RuntimePickerClear,
    SettingsScope(usize),
    Setting(usize),
    SettingValue(usize),
    SettingInherit(usize),
    SettingsInputField,
    SettingsInputSubmit,
    SettingsInputCancel,
    ModelProfileAction(ModelProfileAction),
    ProfileEngineResult(usize),
    ProfileEngineApply,
    ProfileEngineCancel,
    HelpClose,
    FollowLatest,
    CommandSuggestion(usize),
    CommandBar,
}

#[derive(Debug, Clone, Default)]
pub struct UiLayout {
    pub too_small: bool,
    pub compact: bool,
    pub model_row_height: u16,
    pub runtime_row_height: u16,
    pub settings_row_height: u16,
    pub overlay_row_height: u16,
    pub nav_items: Vec<(Screen, Rect)>,
    pub content: Rect,
    pub overview_metrics: Rect,
    pub overview_body: Rect,
    pub overview_progress: Rect,
    pub model_installed_tab: Rect,
    pub model_discover_tab: Rect,
    pub model_search_field: Rect,
    pub model_search_submit: Rect,
    pub model_format_row: Rect,
    pub model_format_filters: Vec<(Option<ArtifactFormat>, Rect)>,
    pub model_list: Rect,
    pub model_progress: Rect,
    pub model_downloads: Rect,
    pub model_rows: Vec<(usize, Rect)>,
    pub model_download_actions: Vec<(usize, Rect)>,
    pub installed_model_actions: Vec<(InstalledModelAction, Rect)>,
    pub server_details: Rect,
    pub server_progress: Rect,
    pub runtime_summary: Rect,
    pub runtime_list: Rect,
    pub runtime_rows: Vec<(usize, Rect)>,
    pub runtime_actions: Rect,
    pub runtime_search_action: Rect,
    pub runtime_update_action: Rect,
    pub selected_runtime_actions: Vec<(SelectedRuntimeAction, Rect)>,
    pub runtime_search_popup: Option<Rect>,
    pub runtime_search_input: Rect,
    pub runtime_search_incompatible_toggle: Rect,
    pub runtime_search_results: Rect,
    pub runtime_search_rows: Vec<(usize, Rect)>,
    pub runtime_search_details: Rect,
    pub runtime_search_submit: Rect,
    pub runtime_install_action: Rect,
    pub runtime_picker_clear_action: Rect,
    pub runtime_overlay_cancel: Rect,
    pub runtime_operation_status: Rect,
    pub runtime_picker_active: bool,
    pub logs: Rect,
    pub logs_follow_latest: Rect,
    pub settings_scopes: Rect,
    pub settings_list: Rect,
    pub settings_scope_rows: Vec<(usize, Rect)>,
    pub settings_rows: Vec<(usize, Rect)>,
    pub setting_values: Vec<(usize, Rect)>,
    pub setting_inherit_actions: Vec<(usize, Rect)>,
    pub settings_input_field: Rect,
    pub settings_input_submit: Rect,
    pub settings_input_cancel: Rect,
    pub model_profile_actions: Vec<(ModelProfileAction, Rect)>,
    pub profile_engine_popup: Option<Rect>,
    pub profile_engine_rows: Vec<(usize, Rect)>,
    pub profile_engine_apply: Rect,
    pub profile_engine_cancel: Rect,
    pub help_popup: Option<Rect>,
    pub help_close: Rect,
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

        let compact = area.width < COMPACT_WIDTH || area.height < 28;
        let model_row_height = if compact {
            MODEL_ROW_HEIGHT_COMPACT
        } else {
            MODEL_ROW_HEIGHT_COMFORTABLE
        };
        let two_line_row_height = if compact {
            TWO_LINE_ROW_HEIGHT_COMPACT
        } else {
            TWO_LINE_ROW_HEIGHT_COMFORTABLE
        };
        let shell_area = area.inner(Margin {
            horizontal: if compact { 1 } else { 3 },
            vertical: u16::from(!compact && area.height >= 34),
        });
        let regions = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(if compact { 4 } else { 5 }),
                Constraint::Min(5),
                Constraint::Length(3),
                Constraint::Length(if area.height == MIN_HEIGHT { 1 } else { 2 }),
            ])
            .split(shell_area);
        let content = regions[1].inner(Margin {
            horizontal: if compact { 1 } else { 2 },
            vertical: u16::from(area.height >= 20),
        });
        let screen_body = content_layout(content, compact)[1];
        let nav_items = nav_rects(regions[0], compact);

        let mut overview_metrics = Rect::default();
        let mut overview_body = Rect::default();
        let mut overview_progress = Rect::default();
        if app.screen == Screen::Overview {
            let progress_height = if app.load_progress().is_some() { 1 } else { 0 };
            let overview = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(if compact { 3 } else { 4 }),
                    Constraint::Length(if compact { 6 } else { 5 }),
                    Constraint::Length(progress_height),
                    Constraint::Min(5),
                ])
                .split(content);
            overview_metrics = overview[1];
            overview_progress = overview[2];
            overview_body = overview[3];
        }

        let mut model_installed_tab = Rect::default();
        let mut model_discover_tab = Rect::default();
        let mut model_search_field = Rect::default();
        let mut model_search_submit = Rect::default();
        let mut model_format_row = Rect::default();
        let mut model_format_filters = Vec::new();
        let mut installed_model_actions = Vec::new();
        let mut model_downloads = Rect::default();
        let (model_list, model_progress) = if app.screen == Screen::Models {
            let model_header = content_layout(content, compact)[0];
            model_installed_tab = Rect::new(model_header.x, model_header.y + 1, 13, 1);
            model_discover_tab = Rect::new(
                model_installed_tab.right().saturating_add(2),
                model_header.y + 1,
                if compact { 12 } else { 27 },
                1,
            );
            let mut model_body = screen_body;
            if app.model_library_view == ModelLibraryView::Discover {
                let search_row = Rect::new(
                    model_header.x,
                    model_header.y.saturating_add(2),
                    model_header.width,
                    u16::from(model_header.height > 2),
                );
                let submit_width = if compact { 6 } else { 10 }.min(search_row.width);
                let search_prefix_width = 8.min(
                    search_row
                        .width
                        .saturating_sub(submit_width.saturating_add(1)),
                );
                model_search_field = Rect::new(
                    search_row.x.saturating_add(search_prefix_width),
                    search_row.y,
                    search_row
                        .width
                        .saturating_sub(search_prefix_width)
                        .saturating_sub(submit_width.saturating_add(1)),
                    search_row.height,
                );
                model_search_submit = Rect::new(
                    model_search_field.right().saturating_add(1),
                    search_row.y,
                    submit_width,
                    search_row.height,
                );

                model_format_row = Rect::new(
                    model_body.x,
                    model_body.y,
                    model_body.width,
                    u16::from(model_body.height > 0),
                );
                let mut filter_x = model_format_row.x.saturating_add(7);
                for (format, width) in [
                    (None, 7),
                    (Some(ArtifactFormat::Gguf), 8),
                    (Some(ArtifactFormat::Q27), 7),
                    (Some(ArtifactFormat::Ninfer), 10),
                ] {
                    let width = width.min(model_format_row.right().saturating_sub(filter_x));
                    if width == 0 {
                        break;
                    }
                    model_format_filters.push((
                        format,
                        Rect::new(filter_x, model_format_row.y, width, model_format_row.height),
                    ));
                    filter_x = filter_x.saturating_add(width.saturating_add(1));
                }
                let format_gap = u16::from(!compact && model_body.height > 1);
                model_body = Rect::new(
                    model_body.x,
                    model_body
                        .y
                        .saturating_add(model_format_row.height)
                        .saturating_add(format_gap),
                    model_body.width,
                    model_body
                        .height
                        .saturating_sub(model_format_row.height.saturating_add(format_gap)),
                );
            }
            let pending_downloads = app
                .model_download_jobs
                .iter()
                .filter(|job| !job.is_terminal())
                .count();
            let recent_downloads = app
                .model_download_jobs
                .iter()
                .filter(|job| job.is_terminal())
                .count()
                .min(if pending_downloads == 0 { 3 } else { 1 });
            let download_height = (pending_downloads + recent_downloads + 1) as u16;
            let download_height =
                download_height.min(model_body.height.saturating_sub(model_row_height).max(3));
            let (model_body, downloads) = reserve_bottom(
                model_body,
                !app.model_download_jobs.is_empty(),
                download_height,
            );
            model_downloads = downloads;
            let (mut list, progress) =
                reserve_bottom(model_body, app.selected_model_load_progress().is_some(), 3);
            if app.model_library_view == ModelLibraryView::Installed {
                if let Some(model) = app
                    .selected_model
                    .and_then(|index| app.snapshot.models.get(index))
                {
                    let action_height = 2.min(list.height);
                    let action_area = Rect::new(
                        list.x,
                        list.bottom()
                            .saturating_sub(action_height)
                            .saturating_add(u16::from(!compact && action_height > 1)),
                        list.width,
                        if compact { action_height } else { 1 },
                    );
                    list.height = list.height.saturating_sub(action_height);
                    let mut actions = vec![
                        (
                            InstalledModelAction::CreateProfile,
                            if compact { 11 } else { 18 },
                        ),
                        (InstalledModelAction::Runtime, 11),
                    ];
                    let is_active = app.control.as_ref().is_some_and(|control| {
                        control.backends.iter().any(|backend| {
                            backend.model_id == model.id
                                && !matches!(
                                    backend.lifecycle,
                                    norted_engine::BackendLifecycle::Stopped
                                )
                        })
                    });
                    if is_active {
                        actions.push((InstalledModelAction::Unload, 10));
                    }
                    if model.provenance.is_some() {
                        actions.push((InstalledModelAction::Remove, 18));
                    }
                    installed_model_actions = flow_actions(action_area, &actions, compact);
                }
            }
            (list, progress)
        } else {
            (Rect::default(), Rect::default())
        };

        let mut model_rows = Vec::new();
        let mut model_download_actions = Vec::new();
        let model_count = if app.model_library_view == ModelLibraryView::Discover {
            app.model_search_artifacts().len()
        } else {
            app.snapshot.models.len()
        };
        if app.screen == Screen::Models
            && (app.model_library_view == ModelLibraryView::Discover
                || matches!(
                    app.snapshot.registry_state,
                    RegistryState::Ready | RegistryState::ReadyWithWarnings { .. }
                ))
            && model_count > 0
        {
            let capacity = (model_list.height / model_row_height) as usize;
            let start = if app.model_library_view == ModelLibraryView::Discover {
                app.selected_model_search_result
                    .unwrap_or_default()
                    .saturating_sub(capacity.saturating_sub(1))
            } else {
                app.model_scroll
            };
            let end = (start + capacity).min(model_count);
            for index in start..end {
                let row = Rect::new(
                    model_list.x,
                    model_list.y + ((index - start) as u16 * model_row_height),
                    model_list.width,
                    model_row_height,
                );
                model_rows.push((index, row));
                if app.model_library_view == ModelLibraryView::Discover {
                    let width = 12.min(row.width);
                    model_download_actions.push((
                        index,
                        Rect::new(
                            row.right().saturating_sub(width),
                            row.y.saturating_add(2),
                            width,
                            u16::from(row.height > 2),
                        ),
                    ));
                }
            }
        }

        let (server_details, server_progress) = if app.screen == Screen::Server {
            reserve_bottom(screen_body, app.load_progress().is_some(), 3)
        } else {
            (Rect::default(), Rect::default())
        };

        let mut runtime_summary = Rect::default();
        let mut runtime_list = Rect::default();
        let mut runtime_actions = Rect::default();
        let mut runtime_search_action = Rect::default();
        let mut runtime_update_action = Rect::default();
        let mut selected_runtime_actions = Vec::new();
        let mut runtime_rows = Vec::new();
        if app.screen == Screen::Runtimes {
            let summary_height = screen_body.height.min(if compact { 3 } else { 5 });
            let compact_runtime_actions = compact || screen_body.width < 83;
            let has_selection = app
                .selected_runtime
                .and_then(|index| app.runtime_list.as_ref()?.installed.get(index));
            let requested_action_height = if has_selection.is_some() { 3 } else { 1 };
            let action_height = screen_body
                .height
                .saturating_sub(summary_height)
                .min(requested_action_height);
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
                    runtime_search_action
                        .right()
                        .saturating_add(if compact { 1 } else { 2 }),
                    runtime_actions.y,
                    runtime_actions
                        .right()
                        .saturating_sub(runtime_search_action.right().saturating_add(if compact {
                            1
                        } else {
                            2
                        }))
                        .min(18),
                    1,
                );
                if let Some(status) = has_selection {
                    let selected_area = Rect::new(
                        runtime_actions.x,
                        runtime_actions
                            .y
                            .saturating_add(if compact { 1 } else { 2 }),
                        runtime_actions.width,
                        runtime_actions
                            .height
                            .saturating_sub(if compact { 1 } else { 2 }),
                    );
                    let mut actions = status
                        .runtime
                        .manifest
                        .supported_formats
                        .iter()
                        .copied()
                        .map(|format| {
                            (
                                SelectedRuntimeAction::Default(format),
                                if compact_runtime_actions {
                                    match format {
                                        ArtifactFormat::Gguf => 8,
                                        ArtifactFormat::Q27 => 7,
                                        ArtifactFormat::Ninfer => 10,
                                    }
                                } else {
                                    16
                                },
                            )
                        })
                        .collect::<Vec<_>>();
                    actions.push((
                        SelectedRuntimeAction::Update,
                        if compact_runtime_actions { 10 } else { 12 },
                    ));
                    if status.runtime.manifest.acquisition_method
                        != RuntimeAcquisitionMethod::ExternalBinary
                    {
                        actions.push((SelectedRuntimeAction::Remove, 18));
                    }
                    selected_runtime_actions = flow_actions(selected_area, &actions, compact);
                }
            }
            if let Some(snapshot) = &app.runtime_list {
                let capacity = (runtime_list.height / two_line_row_height) as usize;
                let end = (app.runtime_scroll + capacity).min(snapshot.installed.len());
                for index in app.runtime_scroll..end {
                    runtime_rows.push((
                        index,
                        Rect::new(
                            runtime_list.x,
                            runtime_list.y
                                + ((index - app.runtime_scroll) as u16 * two_line_row_height),
                            runtime_list.width,
                            two_line_row_height,
                        ),
                    ));
                }
            }
        }

        let mut runtime_search_popup = None;
        let mut runtime_search_input = Rect::default();
        let mut runtime_search_incompatible_toggle = Rect::default();
        let mut runtime_search_results = Rect::default();
        let mut runtime_search_details = Rect::default();
        let mut runtime_search_submit = Rect::default();
        let mut runtime_install_action = Rect::default();
        let mut runtime_picker_clear_action = Rect::default();
        let mut runtime_overlay_cancel = Rect::default();
        let mut runtime_operation_status = Rect::default();
        let mut runtime_search_rows = Vec::new();
        let runtime_picker_active = app.overlay == Some(Overlay::ModelRuntime);
        if matches!(
            app.overlay,
            Some(Overlay::RuntimeSearch | Overlay::ModelRuntime)
        ) {
            let horizontal_margin = if compact { 1 } else { (area.width / 10).max(4) };
            let vertical_margin = if compact { 1 } else { 3 };
            let popup = area.inner(Margin {
                horizontal: horizontal_margin,
                vertical: vertical_margin,
            });
            runtime_search_popup = Some(popup);
            let inner = popup.inner(Margin {
                horizontal: if compact { 2 } else { 3 },
                vertical: if compact { 1 } else { 2 },
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
                runtime_search_incompatible_toggle = Rect::new(
                    inner.x,
                    inner.y.saturating_add(1),
                    inner.width.min(24),
                    u16::from(inner.height > 1),
                );
            }
            let body_top = if compact { 2 } else { 3 };
            let body_bottom = if compact { 1 } else { 2 };
            let body = Rect::new(
                inner.x,
                inner.y.saturating_add(body_top),
                inner.width,
                inner
                    .height
                    .saturating_sub(body_top.saturating_add(body_bottom)),
            );
            if !compact && body.width >= 68 {
                let result_width = body.width.saturating_mul(3) / 5;
                runtime_search_results = Rect::new(body.x, body.y, result_width, body.height);
                runtime_search_details = Rect::new(
                    body.x.saturating_add(result_width).saturating_add(3),
                    body.y,
                    body.width.saturating_sub(result_width.saturating_add(3)),
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
            if runtime_picker_active && app.selected_model_has_runtime_override() {
                let clear_width = inner.width.min(if compact { 9 } else { 18 });
                runtime_picker_clear_action = Rect::new(
                    inner
                        .right()
                        .saturating_sub(10)
                        .saturating_sub(clear_width.saturating_add(1)),
                    runtime_install_action.y,
                    clear_width,
                    runtime_install_action.height,
                );
                runtime_install_action.width = runtime_install_action.width.min(
                    runtime_picker_clear_action
                        .x
                        .saturating_sub(runtime_install_action.x.saturating_add(1)),
                );
            }
            let cancel_width = inner
                .width
                .saturating_sub(runtime_install_action.width)
                .min(10);
            runtime_overlay_cancel = Rect::new(
                inner.right().saturating_sub(cancel_width),
                runtime_install_action.y,
                cancel_width,
                runtime_install_action.height,
            );
            runtime_operation_status = if runtime_picker_active {
                Rect::new(
                    inner.x,
                    inner.y.saturating_add(1),
                    inner.width,
                    u16::from(inner.height > 1),
                )
            } else {
                Rect::new(
                    runtime_install_action.right().saturating_add(1),
                    runtime_install_action.y,
                    runtime_overlay_cancel
                        .x
                        .saturating_sub(runtime_install_action.right().saturating_add(1)),
                    runtime_install_action.height,
                )
            };
            let (indices, scroll) = if runtime_picker_active {
                (app.runtime_picker_indices(), app.runtime_picker_scroll)
            } else {
                (app.runtime_search_indices(), app.runtime_search_scroll)
            };
            let capacity = (runtime_search_results.height / two_line_row_height) as usize;
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
                        runtime_search_results.y
                            + ((visible_position - scroll) as u16 * two_line_row_height),
                        runtime_search_results.width,
                        two_line_row_height,
                    ),
                ));
            }
        }

        let suggestions = app.suggestions();
        let (suggestion_popup, suggestion_rows) = if app.command_active && !suggestions.is_empty() {
            let visible_count = suggestions.len().min(8);
            let popup_inset_x = if compact { 2 } else { 3 };
            let popup_inset_y = if compact { 1 } else { 2 };
            let height = visible_count as u16 + popup_inset_y * 2;
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
                            popup.x.saturating_add(popup_inset_x),
                            popup.y + popup_inset_y + (index - app.suggestion_scroll) as u16,
                            popup.width.saturating_sub(popup_inset_x * 2),
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
        let mut setting_values = Vec::new();
        let mut setting_inherit_actions = Vec::new();
        let mut settings_input_field = Rect::default();
        let mut settings_input_submit = Rect::default();
        let mut settings_input_cancel = Rect::default();
        let mut model_profile_actions = Vec::new();
        if matches!(app.screen, Screen::Settings | Screen::ModelProfiles) {
            settings_scopes = Rect::new(
                screen_body.x,
                screen_body.y,
                screen_body.width,
                screen_body.height.min(2),
            );
            let settings_intro_height = match (app.screen, compact) {
                (Screen::ModelProfiles, true) => 7,
                (Screen::ModelProfiles, false) => 8,
                (_, true) => 6,
                (_, false) => 7,
            };
            settings_list = Rect::new(
                screen_body.x,
                screen_body.y.saturating_add(settings_intro_height),
                screen_body.width,
                screen_body.height.saturating_sub(settings_intro_height),
            );
            let labels = if app.screen == Screen::ModelProfiles {
                app.model_profile_values()
                    .into_iter()
                    .map(|profile| profile.id.to_string())
                    .collect::<Vec<_>>()
            } else {
                app.settings_scopes()
                    .into_iter()
                    .map(|scope| match scope {
                        crate::app::SettingsScope::Global => "Global".to_owned(),
                        crate::app::SettingsScope::Engine(engine) => engine,
                        crate::app::SettingsScope::ModelProfile(profile) => profile.to_string(),
                    })
                    .collect::<Vec<_>>()
            };
            let mut x = settings_scopes.x;
            for (index, label) in labels.iter().enumerate() {
                let width = (UnicodeWidthStr::width(label.as_str()) as u16 + 2)
                    .min(settings_scopes.right().saturating_sub(x));
                if width == 0 {
                    break;
                }
                settings_scope_rows.push((
                    index,
                    Rect::new(
                        x,
                        settings_scopes.y,
                        width,
                        u16::from(settings_scopes.height > 0),
                    ),
                ));
                x = x.saturating_add(width);
            }
            let definitions = app.settings_definitions();
            let capacity = (settings_list.height / two_line_row_height) as usize;
            let end = (app.settings_scroll + capacity).min(definitions.len());
            for index in app.settings_scroll..end {
                let row = Rect::new(
                    settings_list.x,
                    settings_list.y + ((index - app.settings_scroll) as u16 * two_line_row_height),
                    settings_list.width,
                    two_line_row_height,
                );
                settings_rows.push((index, row));
                let inherited = app
                    .settings_definitions()
                    .get(index)
                    .is_none_or(|definition| app.settings_value_display(&definition.id).2);
                let inherit_width = if inherited { 11.min(row.width) } else { 0 };
                if inherit_width > 0 {
                    setting_inherit_actions.push((
                        index,
                        Rect::new(
                            row.right().saturating_sub(inherit_width),
                            row.y.saturating_add(1),
                            inherit_width,
                            u16::from(row.height > 1),
                        ),
                    ));
                }
                let value_width = row
                    .width
                    .saturating_sub(inherit_width.saturating_add(u16::from(inherit_width > 0)))
                    .min(if compact { 16 } else { 20 });
                let value_right = row
                    .right()
                    .saturating_sub(inherit_width.saturating_add(u16::from(inherit_width > 0)));
                setting_values.push((
                    index,
                    Rect::new(
                        value_right.saturating_sub(value_width),
                        row.y.saturating_add(1),
                        value_width,
                        u16::from(row.height > 1),
                    ),
                ));
            }

            if app.settings_input.is_some() {
                settings_input_field = Rect::new(
                    settings_scopes.x,
                    settings_scopes.y.saturating_add(1),
                    settings_scopes.width,
                    u16::from(screen_body.height > 1),
                );
                let labels = [((), 15_u16), ((), 10_u16)];
                let actions = flow_actions(
                    Rect::new(
                        settings_scopes.x,
                        settings_scopes.y.saturating_add(2),
                        settings_scopes.width,
                        u16::from(screen_body.height > 2),
                    ),
                    &labels,
                    compact,
                );
                settings_input_submit = actions.first().map_or(Rect::default(), |(_, rect)| *rect);
                settings_input_cancel = actions.get(1).map_or(Rect::default(), |(_, rect)| *rect);
            } else if app.screen == Screen::ModelProfiles
                && app.selected_model_profile_value().is_some()
            {
                let action_area = Rect::new(
                    settings_scopes.x,
                    settings_scopes
                        .y
                        .saturating_add(if compact { 5 } else { 6 }),
                    settings_scopes.width,
                    screen_body
                        .height
                        .saturating_sub(if compact { 5 } else { 6 })
                        .min(2),
                );
                let mut actions = vec![(ModelProfileAction::Load, 8)];
                if app.selected_profile_is_active() {
                    actions.push((ModelProfileAction::Unload, 10));
                }
                actions.extend([
                    (ModelProfileAction::Model, 9),
                    (ModelProfileAction::Engine, 10),
                    (ModelProfileAction::Role, 8),
                    (ModelProfileAction::Duplicate, if compact { 9 } else { 13 }),
                    (ModelProfileAction::Delete, 18),
                    (ModelProfileAction::Refresh, if compact { 9 } else { 11 }),
                ]);
                model_profile_actions = flow_actions(action_area, &actions, compact);
            }
        }

        let mut profile_engine_popup = None;
        let mut profile_engine_rows = Vec::new();
        let mut profile_engine_apply = Rect::default();
        let mut profile_engine_cancel = Rect::default();
        if app.overlay == Some(Overlay::ProfileEngine) {
            if let Some(selection) = &app.profile_engine_selection {
                let popup = centered_fixed(
                    area,
                    if compact { 68 } else { 76 },
                    (selection.engines.len() as u16)
                        .saturating_mul(if compact { 1 } else { 2 })
                        .saturating_add(if compact { 7 } else { 9 }),
                );
                profile_engine_popup = Some(popup);
                let inner = popup.inner(Margin {
                    horizontal: if compact { 2 } else { 3 },
                    vertical: if compact { 1 } else { 2 },
                });
                let engine_row_height = if compact { 1 } else { 2 };
                for index in 0..selection.engines.len() {
                    profile_engine_rows.push((
                        index,
                        Rect::new(
                            inner.x,
                            inner.y.saturating_add(2 + index as u16 * engine_row_height),
                            inner.width,
                            engine_row_height,
                        ),
                    ));
                }
                let actions = flow_actions(
                    Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
                    &[((), 11), ((), 10)],
                    compact,
                );
                profile_engine_apply = actions.first().map_or(Rect::default(), |(_, rect)| *rect);
                profile_engine_cancel = actions.get(1).map_or(Rect::default(), |(_, rect)| *rect);
            }
        }

        let mut help_popup = None;
        let mut help_close = Rect::default();
        if app.overlay == Some(Overlay::Help) {
            let popup = if compact {
                area.inner(Margin {
                    horizontal: 1,
                    vertical: 1,
                })
            } else {
                centered_fixed(area, area.width.saturating_mul(82) / 100, 22)
            };
            help_popup = Some(popup);
            help_close = Rect::new(
                popup.right().saturating_sub(11),
                popup.bottom().saturating_sub(if compact { 2 } else { 3 }),
                9.min(popup.width.saturating_sub(2)),
                1,
            );
        }

        let logs = if app.screen == Screen::Logs {
            screen_body
        } else {
            Rect::default()
        };
        let logs_follow_latest = if app.screen == Screen::Logs && app.log_scroll > 0 {
            Rect::new(
                content.right().saturating_sub(18),
                content.y.saturating_add(1),
                18.min(content.width),
                1,
            )
        } else {
            Rect::default()
        };

        Self {
            too_small: false,
            compact,
            model_row_height,
            runtime_row_height: two_line_row_height,
            settings_row_height: two_line_row_height,
            overlay_row_height: two_line_row_height,
            nav_items,
            content,
            overview_metrics,
            overview_body,
            overview_progress,
            model_installed_tab,
            model_discover_tab,
            model_search_field,
            model_search_submit,
            model_format_row,
            model_format_filters,
            model_list,
            model_progress,
            model_downloads,
            model_rows,
            model_download_actions,
            installed_model_actions,
            server_details,
            server_progress,
            runtime_summary,
            runtime_list,
            runtime_rows,
            runtime_actions,
            runtime_search_action,
            runtime_update_action,
            selected_runtime_actions,
            runtime_search_popup,
            runtime_search_input,
            runtime_search_incompatible_toggle,
            runtime_search_results,
            runtime_search_rows,
            runtime_search_details,
            runtime_search_submit,
            runtime_install_action,
            runtime_picker_clear_action,
            runtime_overlay_cancel,
            runtime_operation_status,
            runtime_picker_active,
            logs,
            logs_follow_latest,
            settings_scopes,
            settings_list,
            settings_scope_rows,
            settings_rows,
            setting_values,
            setting_inherit_actions,
            settings_input_field,
            settings_input_submit,
            settings_input_cancel,
            model_profile_actions,
            profile_engine_popup,
            profile_engine_rows,
            profile_engine_apply,
            profile_engine_cancel,
            help_popup,
            help_close,
            command_bar: regions[2],
            suggestion_popup,
            suggestion_rows,
            footer: regions[3],
            header: regions[0],
        }
    }

    pub fn hit_test(&self, position: Position) -> Option<HoverTarget> {
        if self.help_popup.is_some() {
            return contains(self.help_close, position).then_some(HoverTarget::HelpClose);
        }
        if self.profile_engine_popup.is_some() {
            if contains(self.profile_engine_apply, position) {
                return Some(HoverTarget::ProfileEngineApply);
            }
            if contains(self.profile_engine_cancel, position) {
                return Some(HoverTarget::ProfileEngineCancel);
            }
            return self
                .profile_engine_rows
                .iter()
                .find(|(_, area)| contains(*area, position))
                .map(|(index, _)| HoverTarget::ProfileEngineResult(*index));
        }
        if self.runtime_search_popup.is_some() {
            if contains(self.runtime_overlay_cancel, position) {
                return Some(HoverTarget::RuntimeOverlayCancel);
            }
            if self.runtime_picker_active && contains(self.runtime_picker_clear_action, position) {
                return Some(HoverTarget::RuntimePickerClear);
            }
            if !self.runtime_picker_active && contains(self.runtime_search_input, position) {
                return Some(HoverTarget::RuntimeSearchInput);
            }
            if !self.runtime_picker_active && contains(self.runtime_search_submit, position) {
                return Some(HoverTarget::RuntimeSearchSubmit);
            }
            if !self.runtime_picker_active
                && contains(self.runtime_search_incompatible_toggle, position)
            {
                return Some(HoverTarget::RuntimeSearchIncompatibleToggle);
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
        if contains(self.model_installed_tab, position) {
            return Some(HoverTarget::ModelLibraryTab(ModelLibraryView::Installed));
        }
        if contains(self.model_discover_tab, position) {
            return Some(HoverTarget::ModelLibraryTab(ModelLibraryView::Discover));
        }
        if contains(self.model_search_field, position) {
            return Some(HoverTarget::ModelSearchField);
        }
        if contains(self.model_search_submit, position) {
            return Some(HoverTarget::ModelSearchSubmit);
        }
        if let Some((format, _)) = self
            .model_format_filters
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::ModelFormatFilter(*format));
        }
        if let Some((index, _)) = self
            .model_download_actions
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::ModelDownloadAction(*index));
        }
        if let Some((action, _)) = self
            .installed_model_actions
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::InstalledModelAction(*action));
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
        if let Some((action, _)) = self
            .selected_runtime_actions
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::SelectedRuntimeAction(*action));
        }
        if contains(self.settings_input_submit, position) {
            return Some(HoverTarget::SettingsInputSubmit);
        }
        if contains(self.settings_input_cancel, position) {
            return Some(HoverTarget::SettingsInputCancel);
        }
        if contains(self.settings_input_field, position) {
            return Some(HoverTarget::SettingsInputField);
        }
        if let Some((index, _)) = self
            .settings_scope_rows
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::SettingsScope(*index));
        }
        if let Some((index, _)) = self
            .setting_inherit_actions
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::SettingInherit(*index));
        }
        if let Some((index, _)) = self
            .setting_values
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::SettingValue(*index));
        }
        if let Some((index, _)) = self
            .settings_rows
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::Setting(*index));
        }
        if let Some((action, _)) = self
            .model_profile_actions
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::ModelProfileAction(*action));
        }
        if contains(self.runtime_search_action, position) {
            return Some(HoverTarget::RuntimeSearchAction);
        }
        if contains(self.runtime_update_action, position) {
            return Some(HoverTarget::RuntimeUpdateAction);
        }
        if contains(self.logs_follow_latest, position) {
            return Some(HoverTarget::FollowLatest);
        }
        contains(self.command_bar, position).then_some(HoverTarget::CommandBar)
    }

    pub fn model_capacity(&self) -> usize {
        (self.model_list.height / self.model_row_height.max(1)) as usize
    }

    pub fn log_capacity(&self) -> usize {
        self.logs.height as usize
    }

    pub fn runtime_capacity(&self) -> usize {
        (self.runtime_list.height / self.runtime_row_height.max(1)) as usize
    }

    pub fn runtime_search_capacity(&self) -> usize {
        (self.runtime_search_results.height / self.overlay_row_height.max(1)) as usize
    }

    pub fn settings_capacity(&self) -> usize {
        (self.settings_list.height / self.settings_row_height.max(1)) as usize
    }

    pub fn has_active_marquee(&self, app: &App) -> bool {
        if self.too_small {
            return false;
        }
        if !app.command_active
            && app.notice.as_deref().is_some_and(|notice| {
                needs_marquee(notice, self.command_bar.width.saturating_sub(7))
            })
        {
            return true;
        }
        if app.command_active {
            let suggestions = app.suggestions();
            if let Some(command) = suggestions.get(app.suggestion_index)
                && let Some((_, row)) = self
                    .suggestion_rows
                    .iter()
                    .find(|(index, _)| *index == app.suggestion_index)
            {
                let name_width = if self.compact { 12 } else { 18 };
                let prefix_width = command.name.len().max(name_width) + 2;
                if needs_marquee(
                    command.description,
                    row.width.saturating_sub(prefix_width as u16),
                ) {
                    return true;
                }
            }
        }

        match app.overlay {
            Some(Overlay::RuntimeSearch) => {
                if let Some(progress) = &app.runtime_operation
                    && needs_marquee(
                        &super::runtime_search::progress_text(progress),
                        self.runtime_operation_status.width,
                    )
                {
                    return true;
                }
                let Some(result) = app
                    .selected_runtime_search_result
                    .and_then(|index| app.runtime_search.as_ref()?.results.get(index))
                else {
                    return false;
                };
                let available = &result.entry.available;
                let row_width = self
                    .runtime_search_rows
                    .iter()
                    .find(|(index, _)| Some(*index) == app.selected_runtime_search_result)
                    .map_or(0, |(_, row)| row.width);
                let compatibility = match result.entry.compatibility {
                    norted_core::RuntimeCompatibility::Recommended => "recommended",
                    norted_core::RuntimeCompatibility::Compatible => "compatible",
                    norted_core::RuntimeCompatibility::NeedsAttention(_) => "needs attention",
                    norted_core::RuntimeCompatibility::Incompatible(_) => "incompatible",
                };
                let name_width = row_width.saturating_sub(5 + compatibility.len() as u16);
                let detail_width = self
                    .runtime_search_details
                    .width
                    .saturating_sub(super::components::KEY_COLUMN as u16 + 1);
                let source = available
                    .identity
                    .package
                    .repository
                    .as_deref()
                    .unwrap_or(available.source_url.as_str());
                needs_marquee(&available.display_name, name_width)
                    || needs_marquee(&available.display_name, self.runtime_search_details.width)
                    || needs_marquee(source, detail_width)
            }
            Some(Overlay::ModelRuntime) => {
                let model_overflow = app
                    .selected_model
                    .and_then(|index| app.snapshot.models.get(index))
                    .is_some_and(|model| {
                        needs_marquee(
                            &model.display_name,
                            self.runtime_search_input.width.saturating_sub(32),
                        )
                    });
                let runtime_overflow = app
                    .runtime_picker_selection
                    .and_then(|selected| {
                        let status = app.runtime_list.as_ref()?.installed.get(selected)?;
                        let row = self
                            .runtime_search_rows
                            .iter()
                            .find(|(index, _)| *index == selected)?
                            .1;
                        let identity = &status.runtime.manifest.identity;
                        Some(needs_marquee(
                            &format!("{}  {}", identity.engine_id, identity.version),
                            row.width.saturating_sub(18),
                        ))
                    })
                    .unwrap_or(false);
                model_overflow || runtime_overflow
            }
            Some(Overlay::ProfileEngine) => {
                app.profile_engine_selection
                    .as_ref()
                    .is_some_and(|selection| {
                        needs_marquee(
                            &selection.model.display_name,
                            self.profile_engine_popup
                                .map_or(0, |popup| popup.width.saturating_sub(41)),
                        )
                    })
            }
            Some(Overlay::Help) => false,
            None => match app.screen {
                Screen::Models => self.model_rows.iter().any(|(index, row)| {
                    let active = app.selected_model == Some(*index)
                        || app.hover == Some(HoverTarget::Model(*index));
                    if !active {
                        return false;
                    }
                    if app.model_library_view == ModelLibraryView::Discover {
                        return app
                            .model_search_artifact(*index)
                            .is_some_and(|(_, artifact)| {
                                needs_marquee(&artifact.filename, row.width.saturating_sub(9))
                            });
                    }
                    app.snapshot.models.get(*index).is_some_and(|model| {
                        let size_width =
                            UnicodeWidthStr::width(format_bytes(model.size_bytes).as_str()) as u16;
                        needs_marquee(
                            &model.display_name,
                            row.width.saturating_sub(3).saturating_mul(3) / 5,
                        ) || needs_marquee(
                            &model.path.display().to_string(),
                            row.width.saturating_sub(size_width.saturating_add(2)),
                        )
                    })
                }),
                Screen::Runtimes => self.runtime_rows.iter().any(|(index, row)| {
                    let active = app.selected_runtime == Some(*index)
                        || app.hover == Some(HoverTarget::Runtime(*index));
                    active
                        && app
                            .runtime_list
                            .as_ref()
                            .and_then(|snapshot| snapshot.installed.get(*index))
                            .is_some_and(|status| {
                                let identity = &status.runtime.manifest.identity;
                                needs_marquee(
                                    &format!("{}  {}", identity.engine_id, identity.version),
                                    row.width.saturating_sub(20),
                                )
                            })
                }),
                Screen::Settings | Screen::ModelProfiles => {
                    let setting_overflow = app
                        .settings_definitions()
                        .get(app.settings_setting_index)
                        .is_some_and(|definition| {
                            let id_width = self
                                .setting_values
                                .iter()
                                .find(|(index, _)| *index == app.settings_setting_index)
                                .and_then(|(_, value)| {
                                    self.settings_rows
                                        .iter()
                                        .find(|(index, _)| *index == app.settings_setting_index)
                                        .map(|(_, row)| {
                                            value.x.saturating_sub(row.x).saturating_sub(2)
                                        })
                                })
                                .unwrap_or(0);
                            let value_width = self
                                .setting_values
                                .iter()
                                .find(|(index, _)| *index == app.settings_setting_index)
                                .map_or(0, |(_, area)| area.width);
                            let (value, _, _) = app.settings_value_display(&definition.id);
                            needs_marquee(&definition.id.to_string(), id_width)
                                || needs_marquee(&format!("[ {value} ]"), value_width)
                        });
                    let profile_path_overflow = app.screen == Screen::ModelProfiles
                        && app.selected_profile_model().is_some_and(|model| {
                            needs_marquee(
                                &model.path.display().to_string(),
                                self.settings_scopes.width,
                            )
                        });
                    setting_overflow || profile_path_overflow
                }
                Screen::Server => {
                    let width = self
                        .server_details
                        .width
                        .saturating_sub(super::components::KEY_COLUMN as u16 + 1);
                    let endpoint = app
                        .control
                        .as_ref()
                        .and_then(|control| control.public_endpoint.as_deref())
                        .or_else(|| app.snapshot.server.endpoint())
                        .unwrap_or("Not serving");
                    let control_values = app.control.as_ref().map(|control| {
                        [
                            control
                                .backends
                                .iter()
                                .map(|backend| {
                                    format!(
                                        "{} [{:?}/{:?}/{:?}; req {}; leases {}]",
                                        backend.model_profile_id,
                                        backend.role,
                                        backend.residency,
                                        backend.lifecycle,
                                        backend.active_request_count,
                                        backend.primary_lease_count,
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(", "),
                            control
                                .backends
                                .iter()
                                .map(|backend| backend.model_id.to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                            control
                                .backends
                                .iter()
                                .filter_map(|backend| backend.engine_id.clone())
                                .collect::<Vec<_>>()
                                .join(", "),
                            control
                                .backends
                                .iter()
                                .filter_map(|backend| {
                                    backend.runtime_id.as_ref().map(ToString::to_string)
                                })
                                .collect::<Vec<_>>()
                                .join(", "),
                            control
                                .backends
                                .iter()
                                .filter_map(|backend| backend.private_endpoint.clone())
                                .collect::<Vec<_>>()
                                .join(", "),
                        ]
                    });
                    needs_marquee(endpoint, width)
                        || needs_marquee(&app.public_auth_status.bind, width)
                        || control_values.is_some_and(|values| {
                            values.iter().any(|value| needs_marquee(value, width))
                        })
                }
                Screen::Overview | Screen::Logs | Screen::Help => false,
            },
        }
    }

    pub fn contains_content(&self, position: Position) -> bool {
        contains(self.content, position)
    }

    pub fn contains_suggestions(&self, position: Position) -> bool {
        self.suggestion_popup
            .is_some_and(|area| contains(area, position))
    }
}

fn flow_actions<T: Copy>(area: Rect, actions: &[(T, u16)], compact: bool) -> Vec<(T, Rect)> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let mut x = area.x;
    let mut y = area.y;
    let mut result = Vec::with_capacity(actions.len());
    for (action, requested_width) in actions {
        let requested_width = (*requested_width).min(area.width);
        if x > area.x && x.saturating_add(requested_width) > area.right() {
            y = y.saturating_add(1);
            x = area.x;
        }
        if y >= area.bottom() {
            break;
        }
        let width = requested_width.min(area.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        result.push((*action, Rect::new(x, y, width, 1)));
        x = x.saturating_add(width.saturating_add(if compact { 1 } else { 2 }));
    }
    result
}

fn centered_fixed(area: Rect, maximum_width: u16, requested_height: u16) -> Rect {
    let width = maximum_width.min(area.width.saturating_sub(4)).max(1);
    let height = requested_height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn reserve_bottom(area: Rect, enabled: bool, requested_height: u16) -> (Rect, Rect) {
    if !enabled {
        return (area, Rect::default());
    }
    let progress_height = requested_height.min(area.height);
    let body_height = area.height.saturating_sub(progress_height);
    (
        Rect::new(area.x, area.y, area.width, body_height),
        Rect::new(area.x, area.y + body_height, area.width, progress_height),
    )
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use norted_core::{
        AppSnapshot, ArtifactFormat, EffectivePublicAuthMode, ModelArtifact, ModelId,
        PublicAuthMode, PublicAuthStatus, RegistryState, ServerState,
    };
    use norted_engine::{
        BackendLifecycle, BackendLoadPhase, BackendLoadProgress, BackendStatus, ControlStatus,
    };

    use super::*;

    fn test_app(model_count: usize) -> App {
        let models = (0..model_count)
            .map(|index| ModelArtifact {
                id: ModelId(format!("model-{index}")),
                display_name: format!("Model {index}"),
                path: PathBuf::from(format!("/models/model-{index}.gguf")),
                format: ArtifactFormat::Gguf,
                size_bytes: 1,
                created: 0,
                hash: None,
                architecture: None,
                context_length: None,
                provenance: None,
                native_identity: None,
                auxiliary_artifacts: Vec::new(),
                norted_package: None,
            })
            .collect();
        App::new(
            AppSnapshot {
                server: ServerState::Running {
                    endpoint: "http://127.0.0.1:8080".to_owned(),
                },
                registry_state: RegistryState::Ready,
                models,
                registry_warnings: Vec::new(),
            },
            PublicAuthStatus {
                bind: "127.0.0.1:8080".to_owned(),
                loopback: true,
                configured_mode: PublicAuthMode::Auto,
                effective_mode: EffectivePublicAuthMode::Disabled,
                active_key_count: 0,
                bind_allowed: true,
                insecure_remote: false,
            },
            true,
            true,
            Vec::new(),
        )
    }

    fn set_loading(app: &mut App, model_index: usize) {
        app.selected_model = Some(model_index);
        app.control = Some(ControlStatus {
            public_endpoint: Some("http://127.0.0.1:8080".to_owned()),
            available_engine_count: 1,
            installed_engine_count: 1,
            running_backend_count: 0,
            engines: Vec::new(),
            backends: vec![BackendStatus {
                model_profile_id: norted_core::ModelProfileId::new("fixture").expect("profile ID"),
                generation: 1,
                lifecycle: BackendLifecycle::Loading,
                model_id: app.snapshot.models[model_index].id.clone(),
                role: norted_core::ModelRole::Primary,
                residency: norted_engine::BackendResidency::Pinned,
                engine_id: Some("llama.cpp".to_owned()),
                runtime_id: None,
                runtime_version: None,
                runtime_variant: None,
                runtime_executable_sha256: None,
                process_id: None,
                private_endpoint: None,
                load_progress: Some(BackendLoadProgress::indeterminate(
                    BackendLoadPhase::LoadingModel,
                )),
                failure: None,
                provenance: None,
                active_request_count: 0,
                primary_lease_count: 0,
                last_used_unix: 0,
                retiring: false,
            }],
            recent_events: Vec::new(),
        });
    }

    fn overlaps(left: Rect, right: Rect) -> bool {
        left.x < right.right()
            && left.right() > right.x
            && left.y < right.bottom()
            && left.bottom() > right.y
    }

    #[test]
    fn overview_progress_reserves_a_non_overlapping_row() {
        let mut app = test_app(1);
        app.screen = Screen::Overview;
        set_loading(&mut app, 0);
        let layout = UiLayout::calculate(Rect::new(0, 0, 100, 30), &app);

        assert_eq!(layout.overview_progress.height, 1);
        assert!(!overlaps(layout.overview_progress, layout.overview_body));
    }

    #[test]
    fn model_progress_reduces_capacity_and_cannot_hit_hidden_rows() {
        let area = Rect::new(0, 0, 100, 30);
        let mut app = test_app(20);
        app.screen = Screen::Models;
        app.selected_model = Some(5);
        app.model_scroll = 2;
        let idle = UiLayout::calculate(area, &app);
        let idle_capacity = idle.model_capacity();
        assert_eq!(idle_capacity, idle.model_rows.len());

        set_loading(&mut app, 5);
        let loading = UiLayout::calculate(area, &app);
        assert_eq!(loading.model_progress.height, 3);
        assert_eq!(loading.model_capacity(), idle_capacity.saturating_sub(1));
        assert_eq!(loading.model_rows.len(), loading.model_capacity());
        assert!(
            loading
                .model_rows
                .iter()
                .all(|(_, row)| !overlaps(*row, loading.model_progress))
        );

        let position = Position::new(loading.model_progress.x, loading.model_progress.y);
        assert!(!matches!(
            loading.hit_test(position),
            Some(HoverTarget::Model(_))
        ));
        assert_eq!(
            loading.model_rows.last().map(|(index, _)| *index),
            Some(app.model_scroll + loading.model_capacity() - 1),
            "visible rows and selection/scroll capacity must use the same reduced area"
        );
    }

    #[test]
    fn model_layout_without_progress_keeps_the_full_body_capacity() {
        let mut app = test_app(20);
        app.screen = Screen::Models;
        let layout = UiLayout::calculate(Rect::new(0, 0, 100, 30), &app);
        let screen_body = content_layout(layout.content, layout.compact)[1];

        assert_eq!(layout.model_progress, Rect::default());
        assert_eq!(layout.model_list, screen_body);
        assert_eq!(
            layout.model_capacity(),
            (screen_body.height / layout.model_row_height) as usize
        );
    }

    #[test]
    fn server_progress_reserves_space_below_details() {
        let mut app = test_app(1);
        app.screen = Screen::Server;
        set_loading(&mut app, 0);
        let layout = UiLayout::calculate(Rect::new(0, 0, 100, 30), &app);

        assert_eq!(layout.server_progress.height, 3);
        assert!(!overlaps(layout.server_progress, layout.server_details));
    }

    #[test]
    fn comfortable_and_compact_density_use_shared_responsive_metrics() {
        let app = test_app(20);
        let comfortable = UiLayout::calculate(Rect::new(0, 0, 100, 30), &app);
        let narrow = UiLayout::calculate(Rect::new(0, 0, 83, 30), &app);
        let short = UiLayout::calculate(Rect::new(0, 0, 100, 27), &app);

        assert!(!comfortable.compact);
        assert_eq!(comfortable.model_row_height, 4);
        assert_eq!(comfortable.runtime_row_height, 3);
        assert!(comfortable.content.x > narrow.content.x);

        for compact in [&narrow, &short] {
            assert!(compact.compact);
            assert_eq!(compact.model_row_height, 3);
            assert_eq!(compact.runtime_row_height, 2);
            assert_eq!(compact.settings_row_height, 2);
        }
    }

    #[test]
    fn model_row_hitboxes_follow_the_responsive_row_rhythm() {
        let mut app = test_app(20);
        app.screen = Screen::Models;
        for area in [Rect::new(0, 0, 120, 40), Rect::new(0, 0, 80, 24)] {
            let layout = UiLayout::calculate(area, &app);
            for (position, (index, row)) in layout.model_rows.iter().enumerate() {
                assert_eq!(row.height, layout.model_row_height);
                assert!(row.right() <= area.right() && row.bottom() <= area.bottom());
                assert_eq!(
                    layout.hit_test(Position::new(row.x, row.y + row.height - 1)),
                    Some(HoverTarget::Model(*index)),
                );
                if let Some((_, next)) = layout.model_rows.get(position + 1) {
                    assert_eq!(next.y, row.bottom());
                }
            }
        }
    }

    #[test]
    fn every_screen_and_overlay_stays_bounded_near_the_minimum() {
        let area = Rect::new(0, 0, MIN_WIDTH, MIN_HEIGHT);
        let mut app = test_app(4);
        for screen in Screen::ALL {
            app.screen = screen;
            let layout = UiLayout::calculate(area, &app);
            assert!(!layout.too_small);
            for rect in [
                layout.header,
                layout.content,
                layout.command_bar,
                layout.footer,
                layout.model_list,
                layout.runtime_list,
                layout.settings_list,
                layout.logs,
            ] {
                assert!(rect.right() <= area.right());
                assert!(rect.bottom() <= area.bottom());
            }
        }

        for overlay in [Overlay::Help, Overlay::RuntimeSearch, Overlay::ModelRuntime] {
            app.overlay = Some(overlay);
            let layout = UiLayout::calculate(area, &app);
            let popup = if overlay == Overlay::Help {
                layout.help_popup
            } else {
                layout.runtime_search_popup
            }
            .expect("active overlay has geometry");
            assert!(popup.right() <= area.right());
            assert!(popup.bottom() <= area.bottom());
        }
    }
}
