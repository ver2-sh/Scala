use ratatui::layout::{Constraint, Direction, Layout, Margin, Position, Rect};
use ratatui::widgets::{Block, Borders, Padding};
use scala_core::{ArtifactFormat, RegistryState, RuntimeAcquisitionMethod};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, ModelLibraryView, Overlay, Screen};

use super::components::{content_layout, model_content_layout, needs_marquee};
use super::shell::{COMPACT_WIDTH, MIN_HEIGHT, MIN_WIDTH};

const MODEL_ROW_HEIGHT_COMPACT: u16 = 1;
const MODEL_ROW_HEIGHT_COMFORTABLE: u16 = 1;
const TWO_LINE_ROW_HEIGHT_COMPACT: u16 = 1;
const TWO_LINE_ROW_HEIGHT_COMFORTABLE: u16 = 1;
const OVERVIEW_CARD_HEIGHT_COMPACT: u16 = 3;
const OVERVIEW_CARD_HEIGHT_COMFORTABLE: u16 = 6;

pub(super) fn overview_backend_card_inner(area: Rect) -> Rect {
    Block::default()
        .borders(Borders::LEFT)
        .padding(Padding::horizontal(1))
        .inner(area)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InstalledModelAction {
    CreateProfile,
    Runtime,
    Unload,
    Remove,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ModelProfileAction {
    Benchmark,
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
pub enum DownloadJobAction {
    Pause,
    Resume,
    Cancel,
}

impl DownloadJobAction {
    pub fn completed_label(self) -> &'static str {
        match self {
            Self::Pause => "Paused",
            Self::Resume => "Resumed",
            Self::Cancel => "Cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HoverTarget {
    BenchmarkRow(usize),
    BenchmarkAction(crate::benchmarks::Action),
    Navigation(Screen),
    ModelLibraryTab(ModelLibraryView),
    ModelSearchField,
    ModelDownloadsView,
    InspectionDetails,
    ModelSearchSubmit,
    ModelFormatFilter(Option<ArtifactFormat>),
    ModelDownloadAction(usize),
    DownloadJob(usize),
    DownloadJobAction(usize, DownloadJobAction),
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
    SettingsDetails,
    ProfileActions,
    SettingsSearch,
    SettingsFilter,
    SettingsReset,
    SettingsScope(usize),
    Setting(usize),
    SettingValue(usize),
    SettingInherit(usize),
    SettingsInputField,
    SettingsEditorOption(usize),
    SettingsEditorAction(u8),
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
    OverviewBackend(usize),
    OverviewBackendAction(usize),
}

#[derive(Debug, Clone, Default)]
pub struct UiLayout {
    pub benchmarks: crate::benchmarks::BenchmarkLayout,
    pub too_small: bool,
    pub compact: bool,
    pub model_row_height: u16,
    pub runtime_row_height: u16,
    pub overlay_row_height: u16,
    pub nav_items: Vec<(Screen, Rect)>,
    pub content: Rect,
    pub overview_metrics: Rect,
    pub overview_resident: Rect,
    pub overview_backend_rows: Vec<(usize, Rect)>,
    pub overview_backend_actions: Vec<(usize, Rect)>,
    pub model_installed_tab: Rect,
    pub model_discover_tab: Rect,
    pub model_jobs_action: Rect,
    pub model_search_field: Rect,
    pub model_search_submit: Rect,
    pub model_format_row: Rect,
    pub model_format_filters: Vec<(Option<ArtifactFormat>, Rect)>,
    pub model_list: Rect,
    pub inventory_header: Rect,
    pub inspection_action: Rect,
    pub inventory_detail: Rect,
    pub model_progress: Rect,
    pub model_downloads: Rect,
    pub model_rows: Vec<(usize, Rect)>,
    pub model_download_actions: Vec<(usize, Rect)>,
    pub download_job_rows: Vec<(usize, Rect)>,
    pub download_job_actions: Vec<(usize, DownloadJobAction, Rect)>,
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
    pub runtime_search_header: Rect,
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
    pub settings_detail: Rect,
    pub settings_tools: Vec<(HoverTarget, Rect)>,
    pub settings_scope_rows: Vec<(usize, Rect)>,
    pub settings_rows: Vec<(usize, Rect)>,
    pub settings_categories: Vec<(String, Rect)>,
    pub settings_columns: Vec<Rect>,
    pub setting_values: Vec<(usize, Rect)>,
    pub setting_inherit_actions: Vec<(usize, Rect)>,
    pub settings_editor_panel: Rect,
    pub settings_input_field: Rect,
    pub settings_editor_options: Vec<(usize, Rect)>,
    pub settings_editor_actions: Vec<(u8, Rect)>,
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

        if let Some(editor) = app.settings_input.as_ref().and_then(|i| i.editor.as_ref()) {
            let panel = area.inner(Margin {
                horizontal: 1,
                vertical: 0,
            });
            let bottom = panel.bottom().saturating_sub(3);
            let field_height = if editor.custom() && area.height >= 18 {
                if editor.definition.kind == scala_core::SettingKind::JsonObject {
                    (area.height - 12).min(12)
                } else {
                    3
                }
            } else if editor.custom() || editor.options.len() > 8 {
                1
            } else {
                0
            };
            let top = panel.y + 6;
            let count = bottom.saturating_sub(top + field_height + 1).max(1) as usize;
            let visible = editor.visible();
            let selected = visible
                .iter()
                .position(|i| *i == editor.selected)
                .unwrap_or(0);
            let start = selected.saturating_sub(count.saturating_sub(1));
            let options = visible
                .into_iter()
                .skip(start)
                .take(count)
                .enumerate()
                .map(|(row, index)| (index, Rect::new(panel.x, top + row as u16, panel.width, 1)))
                .collect();
            let mut actions = vec![(1, Rect::new(panel.right() - 14, panel.y + 5, 14, 1))];
            if editor.custom() && editor.definition.kind == scala_core::SettingKind::StringList {
                for (index, label_width) in [6, 6, 8, 5, 7, 6, 6].into_iter().enumerate() {
                    let x = panel.x + [0, 6, 12, 20, 25, 32, 38][index];
                    actions.push((index as u8 + 2, Rect::new(x, bottom - 1, label_width, 1)));
                }
            }
            return Self {
                content: panel,
                settings_scopes: panel,
                settings_editor_panel: panel,
                settings_editor_options: options,
                settings_editor_actions: actions,
                settings_input_field: Rect::new(
                    panel.x,
                    bottom - field_height - 1,
                    panel.width,
                    field_height,
                ),
                settings_input_submit: Rect::new(panel.x, bottom, 12, 1),
                settings_input_cancel: Rect::new(panel.x + 14, bottom, 12, 1),
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
                Constraint::Length(
                    if area.height < 20
                        && !matches!(app.screen, Screen::Settings | Screen::ModelProfiles)
                    {
                        1
                    } else {
                        3
                    },
                ),
                Constraint::Length(if area.height == MIN_HEIGHT { 1 } else { 2 }),
            ])
            .split(shell_area);
        let content = regions[1].inner(Margin {
            horizontal: if compact { 1 } else { 2 },
            vertical: u16::from(area.height >= 20),
        });
        let screen_body = if matches!(app.screen, Screen::Settings | Screen::ModelProfiles) {
            content
        } else if app.screen == Screen::Models {
            model_content_layout(content, compact)[1]
        } else {
            content_layout(content, compact)[1]
        };
        let nav_items = nav_rects(regions[0], compact);

        let mut overview_metrics = Rect::default();
        let mut overview_resident = Rect::default();
        let mut overview_backend_rows = Vec::new();
        let mut overview_backend_actions = Vec::new();
        if app.screen == Screen::Overview {
            let overview = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(if content.height < 10 { 2 } else { 3 }),
                    Constraint::Length(if content.height < 10 {
                        1
                    } else if compact {
                        4
                    } else {
                        3
                    }),
                    Constraint::Min(2),
                ])
                .split(content);
            overview_metrics = overview[1];
            overview_resident = overview[2];
            let mut card_height = if compact {
                OVERVIEW_CARD_HEIGHT_COMPACT
            } else if content.height < 24 {
                5
            } else {
                OVERVIEW_CARD_HEIGHT_COMFORTABLE
            };
            let cards = Rect::new(
                overview_resident.x,
                overview_resident.y.saturating_add(1),
                overview_resident.width,
                overview_resident.height.saturating_sub(1),
            );
            if cards.height < card_height {
                card_height = cards.height.max(1);
            }
            let capacity = (cards.height / card_height.max(1)) as usize;
            let visible_count = capacity.min(
                app.resident_backends()
                    .len()
                    .saturating_sub(app.overview_scroll),
            );
            for (visible, index) in (app.overview_scroll..).take(visible_count).enumerate() {
                let area = Rect::new(
                    cards.x,
                    cards.y + visible as u16 * card_height,
                    cards.width,
                    card_height,
                );
                overview_backend_rows.push((index, area));
                let inner = overview_backend_card_inner(area);
                let label_width = 10.min(inner.width);
                overview_backend_actions.push((
                    index,
                    Rect::new(
                        inner.right().saturating_sub(label_width),
                        inner.y
                            + if compact {
                                inner.height.saturating_sub(1)
                            } else {
                                3.min(inner.height.saturating_sub(1))
                            },
                        label_width,
                        1,
                    ),
                ));
            }
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
            let model_header = model_content_layout(content, compact)[0];
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
            let download_count = pending_downloads + recent_downloads;
            let card_stride = if compact { 3 } else { 4 };
            let desired_download_height = if download_count == 0 {
                0
            } else {
                1 + (download_count as u16).saturating_mul(card_stride) - u16::from(!compact)
            };
            let preserved_rows = if model_body.height >= model_row_height.saturating_mul(4) {
                model_row_height.saturating_mul(2)
            } else {
                model_row_height
            };
            let available_after_models = model_body.height.saturating_sub(preserved_rows);
            let proportional_cap = model_body.height / 3;
            let mut download_height =
                desired_download_height.min(available_after_models.min(proportional_cap.max(4)));
            if download_count > 0 && model_body.height >= 8 && download_height < 4 {
                download_height = 4;
            }
            if model_body.height < 8 {
                download_height = 0;
            }
            let (model_body, downloads) =
                reserve_bottom(model_body, download_height > 0, download_height);
            model_downloads = downloads;
            let (mut list, progress) = reserve_bottom(
                model_body,
                app.selected_model_load_progress().is_some() && model_body.height >= 9,
                3,
            );
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
                                    scala_engine::BackendLifecycle::Stopped
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

        let mut model_list = model_list;
        let mut model_progress = model_progress;
        if app.screen == Screen::Models && app.downloads_focused {
            model_downloads = Rect::new(
                content.x,
                content.y + 1,
                content.width,
                content.height.saturating_sub(1),
            );
            model_list = Rect::default();
            model_progress = Rect::default();
            installed_model_actions.clear();
            model_installed_tab = Rect::default();
            model_discover_tab = Rect::default();
            model_search_field = Rect::default();
            model_search_submit = Rect::default();
            model_format_filters.clear();
        }
        let mut inventory_header = Rect::default();
        let mut inventory_detail = Rect::default();
        if app.screen == Screen::Models && model_list.height > 1 {
            inventory_header = Rect::new(model_list.x, model_list.y, model_list.width, 1);
            model_list.y += 1;
            model_list.height -= 1;
            if model_list.height >= 7 {
                let height = 3.min(model_list.height / 3);
                inventory_detail = Rect::new(
                    model_list.x,
                    model_list.bottom() - height,
                    model_list.width,
                    height,
                );
                model_list.height -= height;
            }
        }
        let mut model_rows = Vec::new();
        let mut model_download_actions = Vec::new();
        let mut download_job_rows = Vec::new();
        let mut download_job_actions = Vec::new();
        let model_count = if app.model_library_view == ModelLibraryView::Discover {
            app.model_search_artifacts().len()
        } else {
            app.installed_model_indices().len()
        };
        if app.screen == Screen::Models
            && (app.model_library_view == ModelLibraryView::Discover
                || !app.snapshot.models.is_empty()
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
                let indices = app.installed_model_indices();
                let selected = app
                    .selected_model
                    .and_then(|i| indices.iter().position(|v| *v == i))
                    .unwrap_or(app.model_scroll);
                if selected < app.model_scroll {
                    selected
                } else {
                    app.model_scroll
                        .max(selected.saturating_sub(capacity.saturating_sub(1)))
                }
            };
            let end = (start + capacity).min(model_count);
            for index in start..end {
                let row = Rect::new(
                    model_list.x,
                    model_list.y + ((index - start) as u16 * model_row_height),
                    model_list.width,
                    model_row_height,
                );
                let model_index = if app.model_library_view == ModelLibraryView::Installed {
                    app.installed_model_indices()[index]
                } else {
                    index
                };
                model_rows.push((model_index, row));
                if app.model_library_view == ModelLibraryView::Discover {
                    let width = 12.min(row.width);
                    model_download_actions.push((
                        index,
                        Rect::new(row.right().saturating_sub(width), row.y, width, 1),
                    ));
                }
            }
        }

        if app.screen == Screen::Models && model_downloads.height > 1 {
            let glyphs = crate::theme::Glyphs::current(app.unicode);
            let stride = if compact { 3 } else { 4 };
            let mut y = model_downloads.y.saturating_add(1);
            let indices = app.model_download_display_indices();
            let capacity = (model_downloads.height.saturating_sub(1) / stride).max(1) as usize;
            let selected = app
                .selected_model_download_job
                .as_ref()
                .and_then(|id| {
                    indices
                        .iter()
                        .position(|i| &app.model_download_jobs[*i].id == id)
                })
                .unwrap_or(0);
            let start = selected.saturating_sub(capacity.saturating_sub(1));
            for index in indices.into_iter().skip(start) {
                if y.saturating_add(3) > model_downloads.bottom() {
                    break;
                }
                let row = Rect::new(model_downloads.x, y, model_downloads.width, 3);
                download_job_rows.push((index, row));
                let Some(job) = app.model_download_jobs.get(index) else {
                    continue;
                };
                let secondary = match job.phase {
                    scala_model_library::ModelOperationPhase::Queued
                    | scala_model_library::ModelOperationPhase::Resolving
                    | scala_model_library::ModelOperationPhase::Downloading => {
                        Some(DownloadJobAction::Pause)
                    }
                    scala_model_library::ModelOperationPhase::Paused => {
                        Some(DownloadJobAction::Resume)
                    }
                    _ => None,
                };
                if let Some(secondary) = secondary {
                    let secondary_glyph = match secondary {
                        DownloadJobAction::Pause => glyphs.pause,
                        DownloadJobAction::Resume => glyphs.resume,
                        DownloadJobAction::Cancel => glyphs.cancel,
                    };
                    let secondary_width = UnicodeWidthStr::width(secondary_glyph) as u16
                        + if compact { 2 } else { 4 };
                    let cancel_width =
                        UnicodeWidthStr::width(glyphs.cancel) as u16 + if compact { 2 } else { 4 };
                    let secondary_area = Rect::new(
                        row.right().saturating_sub(secondary_width),
                        row.y,
                        secondary_width.min(row.width),
                        1,
                    );
                    let cancel_area = Rect::new(
                        secondary_area
                            .x
                            .saturating_sub(cancel_width.saturating_add(1)),
                        row.y,
                        cancel_width.min(row.width),
                        1,
                    );
                    download_job_actions.push((index, DownloadJobAction::Cancel, cancel_area));
                    download_job_actions.push((index, secondary, secondary_area));
                }
                y = y.saturating_add(stride);
            }
        }

        let (server_details, server_progress) = if app.screen == Screen::Server {
            reserve_bottom(
                screen_body,
                app.load_progress().is_some() && screen_body.height >= 9,
                3,
            )
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
            let summary_height = if screen_body.height >= 12 { 3 } else { 0 };
            let compact_runtime_actions = compact || screen_body.width < 83;
            let has_selection = app
                .selected_runtime
                .and_then(|index| app.runtime_list.as_ref()?.installed.get(index));
            let requested_action_height = if has_selection.is_some() {
                if screen_body.height < 7 { 2 } else { 3 }
            } else {
                1
            };
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
                            .saturating_add(if action_height > 2 { 2 } else { 1 }),
                        runtime_actions.width,
                        runtime_actions.height.saturating_sub(if action_height > 2 {
                            2
                        } else {
                            1
                        }),
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
            if runtime_list.height > 1 {
                inventory_header = Rect::new(runtime_list.x, runtime_list.y, runtime_list.width, 1);
                runtime_list.y += 1;
                runtime_list.height -= 1;
                if runtime_list.height >= 7 {
                    inventory_detail = Rect::new(
                        runtime_list.x,
                        runtime_list.bottom() - 3,
                        runtime_list.width,
                        3,
                    );
                    runtime_list.height -= 3;
                }
            }
            if let Some(snapshot) = &app.runtime_list {
                let capacity = runtime_list.height as usize;
                let selected = app.selected_runtime.unwrap_or(app.runtime_scroll);
                let start = if selected < app.runtime_scroll {
                    selected
                } else {
                    app.runtime_scroll
                        .max(selected.saturating_sub(capacity.saturating_sub(1)))
                };
                let end = (start + capacity).min(snapshot.installed.len());
                for index in start..end {
                    runtime_rows.push((
                        index,
                        Rect::new(
                            runtime_list.x,
                            runtime_list.y + (index - start) as u16,
                            runtime_list.width,
                            1,
                        ),
                    ));
                }
            }
        }

        let mut runtime_search_popup = None;
        let mut runtime_search_input = Rect::default();
        let mut runtime_search_incompatible_toggle = Rect::default();
        let mut runtime_search_results = Rect::default();
        let mut runtime_search_header = Rect::default();
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
            let mut popup = area.inner(Margin {
                horizontal: horizontal_margin,
                vertical: vertical_margin,
            });
            if !compact && popup.height > 26 {
                popup.y += (popup.height - 26) / 2;
                popup.height = 26;
            }
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
            if !compact && body.width >= 110 {
                let result_width = body.width.saturating_mul(3) / 5;
                runtime_search_results = Rect::new(body.x, body.y, result_width, body.height);
                runtime_search_details = Rect::new(
                    body.x.saturating_add(result_width).saturating_add(3),
                    body.y,
                    body.width.saturating_sub(result_width.saturating_add(3)),
                    body.height,
                );
            } else {
                let count = if runtime_picker_active {
                    app.runtime_picker_indices().len()
                } else {
                    app.runtime_search_indices().len()
                };
                let result_height = (body.height.saturating_mul(3) / 5)
                    .min(count.max(1).saturating_add(1).min(u16::MAX as usize) as u16);
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
            if runtime_search_results.height > 1 {
                runtime_search_header = Rect::new(
                    runtime_search_results.x,
                    runtime_search_results.y,
                    runtime_search_results.width,
                    1,
                );
                runtime_search_results.y += 1;
                runtime_search_results.height -= 1;
            }
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
        let mut settings_detail = Rect::default();
        let mut settings_tools = Vec::new();
        let mut settings_scope_rows = Vec::new();
        let mut settings_rows = Vec::new();
        let mut settings_categories = Vec::new();
        let mut settings_columns = Vec::new();
        let mut setting_values = Vec::new();
        let mut setting_inherit_actions = Vec::new();
        let mut settings_input_field = Rect::default();
        let settings_editor_options = Vec::new();
        let settings_editor_actions = Vec::new();
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
            let settings_intro_height = if screen_body.height < 12 {
                2
            } else if app.screen == Screen::ModelProfiles {
                8
            } else {
                4
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
                    .enumerate()
                    .map(|(index, _)| app.profile_label(index))
                    .collect::<Vec<_>>()
            } else {
                app.settings_scopes()
                    .into_iter()
                    .map(|scope| match scope {
                        crate::app::SettingsScope::Server => "Server".to_owned(),
                        crate::app::SettingsScope::Runtime(engine) => engine,
                        crate::app::SettingsScope::ModelProfile(profile) => profile.to_string(),
                    })
                    .collect::<Vec<_>>()
            };
            let mut x = settings_scopes.x;
            let selector_start = if app.screen == Screen::ModelProfiles && screen_body.width < 110 {
                app.selected_model_profile.unwrap_or(0)
            } else {
                0
            };
            for (index, label) in labels.iter().enumerate().skip(selector_start) {
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
            if app.screen == Screen::ModelProfiles && screen_body.width < 110 {
                settings_scope_rows.truncate(1);
                if let Some((_, rect)) = settings_scope_rows.first_mut() {
                    rect.width = settings_scopes.width;
                    if labels.len() > 1 {
                        rect.x += 3;
                        rect.width = rect.width.saturating_sub(6);
                    }
                }
                if labels.len() > 1 {
                    settings_scope_rows.push((
                        (selector_start + labels.len() - 1) % labels.len(),
                        Rect::new(settings_scopes.x, settings_scopes.y, 3, 1),
                    ));
                    settings_scope_rows.push((
                        (selector_start + 1) % labels.len(),
                        Rect::new(settings_scopes.right() - 3, settings_scopes.y, 3, 1),
                    ));
                }
            }
            if screen_body.width >= 110 {
                let sidebar_width = 24;
                let selected = if app.screen == Screen::ModelProfiles {
                    app.selected_model_profile.unwrap_or(0)
                } else {
                    app.settings_scope_index
                };
                let capacity = screen_body.height as usize / 2;
                let start = selected.saturating_sub(capacity.saturating_sub(1));
                settings_scope_rows = labels
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(capacity)
                    .map(|(index, _)| {
                        (
                            index,
                            Rect::new(
                                screen_body.x,
                                screen_body.y + ((index - start) * 2) as u16,
                                sidebar_width - 1,
                                2,
                            ),
                        )
                    })
                    .collect();
                settings_scopes.x += sidebar_width;
                settings_scopes.width = settings_scopes.width.saturating_sub(sidebar_width);
                settings_list.x = settings_scopes.x;
                settings_list.width = settings_scopes.width;
            }
            settings_tools = flow_actions(
                Rect::new(
                    settings_scopes.x,
                    settings_list.y.saturating_sub(1),
                    settings_scopes.width,
                    1,
                ),
                &[
                    (
                        HoverTarget::SettingsSearch,
                        if settings_scopes.width < 60
                            || screen_body.height < 12
                            || app.profile_actions_open
                        {
                            6
                        } else {
                            12
                        },
                    ),
                    (
                        HoverTarget::SettingsFilter,
                        if settings_scopes.width < 60
                            || screen_body.height < 12
                            || app.profile_actions_open
                        {
                            8
                        } else {
                            18
                        },
                    ),
                    (
                        HoverTarget::SettingsReset,
                        if settings_scopes.width < 60
                            || screen_body.height < 12
                            || app.profile_actions_open
                        {
                            6
                        } else {
                            17
                        },
                    ),
                    (
                        HoverTarget::SettingsDetails,
                        if settings_scopes.width < 60
                            || screen_body.height < 12
                            || app.profile_actions_open
                        {
                            6
                        } else {
                            13
                        },
                    ),
                ],
                compact,
            );
            if app.screen == Screen::ModelProfiles
                && (screen_body.height < 12 || app.profile_actions_open)
            {
                settings_tools.push((
                    HoverTarget::ProfileActions,
                    Rect::new(
                        settings_scopes.x + 31,
                        settings_list.y - 1,
                        settings_scopes.width.saturating_sub(31),
                        1,
                    ),
                ));
            }
            if screen_body.width >= 145 {
                let detail_width = 40;
                settings_detail = Rect::new(
                    settings_list.right().saturating_sub(detail_width),
                    settings_list.y,
                    detail_width,
                    settings_list.height,
                );
                settings_list.width = settings_list.width.saturating_sub(detail_width + 1);
            } else {
                let detail_height = if settings_list.height >= 12 { 5 } else { 0 };
                settings_detail = Rect::new(
                    settings_list.x,
                    settings_list.bottom().saturating_sub(detail_height),
                    settings_list.width,
                    detail_height,
                );
                settings_list.height = settings_list.height.saturating_sub(detail_height);
            }
            if app.settings_show_detail {
                settings_detail = Rect::new(
                    settings_scopes.x,
                    settings_scopes.y.saturating_add(1),
                    settings_scopes.width,
                    screen_body.bottom().saturating_sub(settings_scopes.y + 2),
                );
            }
            let definitions = app.settings_definitions();
            let widths = if settings_list.width >= 70 {
                vec![
                    Constraint::Percentage(32),
                    Constraint::Percentage(25),
                    Constraint::Min(12),
                    Constraint::Length(11),
                    Constraint::Length(9),
                ]
            } else {
                vec![
                    Constraint::Percentage(32),
                    Constraint::Min(8),
                    Constraint::Length(9),
                    Constraint::Length(8),
                    Constraint::Length(0),
                ]
            };
            settings_columns = Layout::horizontal(widths)
                .split(Rect::new(
                    settings_list.x,
                    settings_list.y,
                    settings_list.width,
                    1,
                ))
                .to_vec();
            let mut y = settings_list.y.saturating_add(1);
            for index in app.settings_scroll..definitions.len() {
                let definition = definitions[index];
                if settings_list.height >= 3
                    && (index == app.settings_scroll
                        || definitions[index - 1].category != definition.category)
                {
                    if y.saturating_add(1) >= settings_list.bottom() {
                        break;
                    }
                    settings_categories.push((
                        definition.category.to_string(),
                        Rect::new(settings_list.x, y, settings_list.width, 1),
                    ));
                    y += 1;
                }
                if y >= settings_list.bottom() {
                    break;
                }
                let row = Rect::new(settings_list.x, y, settings_list.width, 1);
                settings_rows.push((index, row));
                let mut value = settings_columns[1];
                value.y = y;
                if definition.supported {
                    setting_values.push((index, value));
                }
                if app.settings_value_display(&definition.id).can_clear {
                    let mut action = settings_columns[3];
                    action.y = y;
                    setting_inherit_actions.push((index, action));
                }
                y += 1;
            }

            if app.settings_input.is_some() {
                settings_input_field = Rect::new(
                    settings_scopes.x,
                    settings_scopes.y.saturating_add(2),
                    settings_scopes.width,
                    u16::from(screen_body.height > 1),
                );
                let labels = [((), 15_u16), ((), 10_u16)];
                let actions = flow_actions(
                    Rect::new(
                        settings_scopes.x,
                        settings_scopes.y.saturating_add(3),
                        settings_scopes.width,
                        u16::from(screen_body.height > 2),
                    ),
                    &labels,
                    compact,
                );
                settings_input_submit = actions.first().map_or(Rect::default(), |(_, rect)| *rect);
                settings_input_cancel = actions.get(1).map_or(Rect::default(), |(_, rect)| *rect);
            } else if app.screen == Screen::ModelProfiles
                && (screen_body.height >= 12 || app.profile_actions_open)
                && app.selected_model_profile_value().is_some()
            {
                let action_area = Rect::new(
                    settings_scopes.x,
                    settings_scopes
                        .y
                        .saturating_add(if app.profile_actions_open { 1 } else { 5 }),
                    settings_scopes.width,
                    if app.profile_actions_open {
                        screen_body.height.saturating_sub(2)
                    } else {
                        2
                    },
                );
                let mut actions = vec![
                    (ModelProfileAction::Load, 8),
                    (ModelProfileAction::Benchmark, 21),
                ];
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

        if app.screen == Screen::ModelProfiles && app.profile_actions_open {
            settings_rows.clear();
            setting_values.clear();
            setting_inherit_actions.clear();
            settings_detail = Rect::default();
            for (_, rect) in &mut settings_tools {
                rect.y = screen_body.bottom().saturating_sub(1);
            }
        }

        if app.settings_show_detail
            && matches!(app.screen, Screen::Settings | Screen::ModelProfiles)
        {
            settings_rows.clear();
            setting_values.clear();
            setting_inherit_actions.clear();
            model_profile_actions.clear();
            for (_, rect) in &mut settings_tools {
                rect.y = screen_body.bottom().saturating_sub(1);
            }
        }

        if app.settings_input.is_some()
            && matches!(app.screen, Screen::Settings | Screen::ModelProfiles)
        {
            settings_tools.clear();
            settings_rows.clear();
            setting_values.clear();
            setting_inherit_actions.clear();
            model_profile_actions.clear();
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
            benchmarks: if app.screen == Screen::Benchmarks {
                crate::benchmarks::BenchmarkLayout::calculate(content, &app.benchmarks)
            } else {
                Default::default()
            },
            too_small: false,
            compact,
            model_row_height,
            runtime_row_height: 1,
            overlay_row_height: two_line_row_height,
            nav_items,
            content,
            overview_metrics,
            overview_resident,
            overview_backend_rows,
            overview_backend_actions,
            model_installed_tab,
            model_discover_tab,
            model_jobs_action: if app.screen == Screen::Models {
                if app.downloads_focused {
                    Rect::new(content.x, content.y, 12, 1)
                } else {
                    let x = model_discover_tab.right().saturating_add(1);
                    Rect::new(
                        x,
                        model_discover_tab.y,
                        content.right().saturating_sub(x).min(10),
                        1,
                    )
                }
            } else {
                Rect::default()
            },
            model_search_field,
            model_search_submit,
            model_format_row,
            model_format_filters,
            model_list,
            inventory_header,
            inspection_action: if matches!(
                app.screen,
                Screen::Models
                    | Screen::Runtimes
                    | Screen::Overview
                    | Screen::Server
                    | Screen::Logs
                    | Screen::Help
            ) {
                Rect::new(
                    content.right().saturating_sub(13),
                    content.y,
                    13.min(content.width),
                    1,
                )
            } else {
                Rect::default()
            },
            inventory_detail,
            model_progress,
            model_downloads,
            model_rows,
            model_download_actions,
            download_job_rows,
            download_job_actions,
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
            runtime_search_header,
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
            settings_detail,
            settings_tools,
            settings_scope_rows,
            settings_rows,
            settings_categories,
            settings_columns,
            setting_values,
            setting_inherit_actions,
            settings_editor_panel: Rect::default(),
            settings_input_field,
            settings_editor_options,
            settings_editor_actions,
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
            .overview_backend_actions
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::OverviewBackendAction(*index));
        }
        if let Some((index, _)) = self
            .overview_backend_rows
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::OverviewBackend(*index));
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
        for (action, rect) in &self.benchmarks.actions {
            if contains(*rect, position) {
                return Some(HoverTarget::BenchmarkAction(*action));
            }
        }
        for (index, rect) in &self.benchmarks.rows {
            if contains(*rect, position) {
                return Some(HoverTarget::BenchmarkRow(*index));
            }
        }
        if contains(self.model_jobs_action, position) {
            return Some(HoverTarget::ModelDownloadsView);
        }
        if contains(self.inspection_action, position) {
            return Some(HoverTarget::InspectionDetails);
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
        if let Some((index, action, _)) = self
            .download_job_actions
            .iter()
            .find(|(_, _, area)| contains(*area, position))
        {
            return Some(HoverTarget::DownloadJobAction(*index, *action));
        }
        if let Some((index, _)) = self
            .download_job_rows
            .iter()
            .find(|(_, area)| contains(*area, position))
        {
            return Some(HoverTarget::DownloadJob(*index));
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
        if let Some((target, _)) = self
            .settings_tools
            .iter()
            .find(|(_, rect)| contains(*rect, position))
        {
            return Some(*target);
        }
        for (index, area) in &self.settings_editor_options {
            if contains(*area, position) {
                return Some(HoverTarget::SettingsEditorOption(*index));
            }
        }
        for (key, area) in &self.settings_editor_actions {
            if contains(*area, position) {
                return Some(HoverTarget::SettingsEditorAction(*key));
            }
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

    pub fn overview_capacity(&self) -> usize {
        self.overview_backend_rows.len()
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
        self.settings_rows.len().max(1)
    }

    pub fn active_marquee_target(&self, app: &App) -> Option<Vec<String>> {
        if self.too_small {
            return None;
        }
        let glyphs = crate::theme::Glyphs::current(app.unicode);
        let mut active = Vec::new();
        let mut track = |label: &str, text: &str, width: usize| {
            if needs_marquee(text, width) {
                active.push(format!("{label}:{width}:{text}"));
            }
        };

        if !app.command_active
            && let Some(notice) = app.notice.as_deref()
        {
            track(
                "notice",
                notice,
                super::shell::notice_width(self.command_bar.width),
            );
        }
        if app.command_active {
            let suggestions = app.suggestions();
            for (index, row) in &self.suggestion_rows {
                if (*index == app.suggestion_index
                    || app.hover == Some(HoverTarget::CommandSuggestion(*index)))
                    && let Some(command) = suggestions.get(*index)
                {
                    track(
                        &format!("command:{index}"),
                        command.description,
                        super::command_palette::suggestion_description_width(
                            row.width,
                            command.name,
                            self.compact,
                        ),
                    );
                }
            }
        }

        match app.overlay {
            Some(Overlay::RuntimeSearch) => {
                if let Some(progress) = &app.runtime_operation {
                    let progress = super::runtime_search::progress_text(progress);
                    track(
                        "runtime-search-progress",
                        &progress,
                        self.runtime_operation_status.width as usize,
                    );
                }
                if let Some(search) = &app.runtime_search {
                    if let Some(index) = app.selected_runtime_search_result
                        && let Some(result) = search.results.get(index)
                    {
                        let available = &result.entry.available;
                        let source = available
                            .identity
                            .package
                            .repository
                            .as_deref()
                            .unwrap_or(available.source_url.as_str());
                        track(
                            &format!("runtime-search-source:{index}"),
                            source,
                            super::components::key_value_width(self.runtime_search_details.width),
                        );
                    }
                }
            }
            Some(Overlay::ModelRuntime) => {
                if let Some((index, model)) = app
                    .selected_model
                    .and_then(|index| app.snapshot.models.get(index).map(|model| (index, model)))
                {
                    track(
                        &format!("model-runtime-heading:{index}"),
                        &model.display_name,
                        super::model_runtime::heading_name_width(
                            self.runtime_search_input.width,
                            model,
                        ),
                    );
                }
            }
            Some(Overlay::ProfileEngine) => {
                if let Some(selection) = &app.profile_engine_selection
                    && let Some(popup) = self.profile_engine_popup
                {
                    let inner_width =
                        super::profile_engine::popup_inner_width(popup.width, self.compact);
                    track(
                        "profile-engine-heading",
                        &selection.model.display_name,
                        super::profile_engine::heading_name_width(inner_width),
                    );
                }
            }
            Some(Overlay::Help) => {}
            None => match app.screen {
                Screen::Overview => {}
                Screen::Models => {
                    for (index, row) in &self.download_job_rows {
                        let Some(job) = app.model_download_jobs.get(*index) else {
                            continue;
                        };
                        let active_job = app.selected_model_download_job.as_ref() == Some(&job.id)
                            || app.hover == Some(HoverTarget::DownloadJob(*index))
                            || self
                                .download_job_actions
                                .iter()
                                .any(|(action_index, action, _)| {
                                    *action_index == *index
                                        && app.hover
                                            == Some(HoverTarget::DownloadJobAction(*index, *action))
                                });
                        if !active_job {
                            continue;
                        }
                        let actions_start = self
                            .download_job_actions
                            .iter()
                            .filter(|(action_index, _, _)| action_index == index)
                            .map(|(_, _, area)| area.x)
                            .min()
                            .unwrap_or_else(|| row.right());
                        let prefix_width = UnicodeWidthStr::width(glyphs.download) + 2;
                        let identity_width = actions_start
                            .saturating_sub(row.x)
                            .saturating_sub(prefix_width as u16)
                            .saturating_sub(u16::from(actions_start < row.right()))
                            as usize;
                        track(
                            &format!("model-download:{index}"),
                            &super::screens::download_identity(job),
                            identity_width,
                        );
                    }
                }
                Screen::Runtimes => {}
                Screen::Settings | Screen::ModelProfiles => {
                    let definitions = app.settings_definitions();
                    for (index, row) in &self.settings_rows {
                        let id_active = app.settings_setting_index == *index
                            || app.hover == Some(HoverTarget::Setting(*index));
                        let value_active =
                            id_active || app.hover == Some(HoverTarget::SettingValue(*index));
                        if !id_active && !value_active {
                            continue;
                        }
                        let Some(definition) = definitions.get(*index) else {
                            continue;
                        };
                        let Some((_, value_area)) = self
                            .setting_values
                            .iter()
                            .find(|(value_index, _)| value_index == index)
                        else {
                            continue;
                        };
                        if id_active {
                            track(
                                &format!("setting-id:{index}"),
                                &definition.id.to_string(),
                                value_area.x.saturating_sub(row.x).saturating_sub(2) as usize,
                            );
                        }
                        if value_active {
                            let value = app.settings_value_display(&definition.id).value;
                            track(
                                &format!("setting-value:{index}"),
                                &format!("[ {value} ]"),
                                value_area.width as usize,
                            );
                        }
                    }
                    if app.screen == Screen::ModelProfiles
                        && app.settings_input.is_none()
                        && let Some(model) = app.selected_profile_model()
                    {
                        track(
                            "profile-model-path",
                            &model.path.display().to_string(),
                            self.settings_scopes.width as usize,
                        );
                    }
                }
                Screen::Server => {}
                Screen::Logs | Screen::Help | Screen::Benchmarks | Screen::Link => {}
            },
        }

        (!active.is_empty()).then_some(active)
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
