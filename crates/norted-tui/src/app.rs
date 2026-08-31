use std::collections::{BTreeMap, BTreeSet};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use norted_core::{
    AppEvent, AppSnapshot, ArtifactFormat, ExternalTemplateReference, LoadSettingDefinition,
    LoadSettingId, LoadSettingKind, LoadSettingScope, LoadSettingSource, LoadSettingValue,
    LoadSettingsError, LoadSettingsSchema, LogLevel, ModelArtifact, ModelId, PromptDelivery,
    PromptMode, PublicAuthStatus, RegistryState, ResolvedLoadSettings, ResponseFilter,
    RuntimeCompatibility, RuntimeId, RuntimeOperationPhase, RuntimeOperationProgress,
    RuntimeUpdateState, ServeProfile, ServeProfileName, ServeProfileSource, ServeProfilesState,
    ServePromptProfile,
};
use norted_engine::{
    BackendLifecycle, BackendLoadProgress, ControlStatus, RuntimeListSnapshot,
    RuntimeModelCandidate, RuntimeNoticeLevel, RuntimeSearchSnapshot, RuntimeUpdateCheck,
};
use ratatui::layout::Position;

use crate::commands::{self, CommandAction};
use crate::ui::layout::{HoverTarget, UiLayout};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Screen {
    Overview,
    Models,
    Runtimes,
    Server,
    Logs,
    Settings,
    Help,
}

impl Screen {
    pub const ALL: [Self; 7] = [
        Self::Overview,
        Self::Models,
        Self::Runtimes,
        Self::Server,
        Self::Logs,
        Self::Settings,
        Self::Help,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Models => "Models",
            Self::Runtimes => "Runtimes",
            Self::Server => "Server",
            Self::Logs => "Logs",
            Self::Settings => "Settings",
            Self::Help => "Help",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Overlay {
    Help,
    RuntimeSearch,
    ModelRuntime,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RuntimeSearchFocus {
    Query,
    Results,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FocusArea {
    Navigation,
    Content,
    Command,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Update {
    None,
    Render,
    Quit,
}

#[derive(Debug, Clone)]
pub enum ControlAction {
    Load(norted_core::ModelId),
    Unload,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ControlStatusSource {
    Observation,
    ControlResult,
}

#[derive(Debug, Clone)]
pub enum RuntimeAction {
    RefreshList,
    Search {
        query: String,
        force_refresh: bool,
        model: Option<Box<ModelArtifact>>,
        serve_profile: Option<Box<norted_core::ServeProfile>>,
    },
    Install(RuntimeId),
    CheckUpdates,
    Update(RuntimeId),
    Remove {
        runtime_id: RuntimeId,
    },
    ModelCandidates {
        model: Box<ModelArtifact>,
        serve_profile: Option<Box<norted_core::ServeProfile>>,
    },
    SelectFormat {
        format: ArtifactFormat,
        runtime_id: RuntimeId,
    },
    SelectModel {
        model: Box<ModelArtifact>,
        runtime_id: RuntimeId,
    },
    ClearModelSelection {
        model_id: ModelId,
    },
}

#[derive(Debug)]
pub enum RuntimeTaskResult {
    Listed(Result<RuntimeListSnapshot, String>),
    Searched(Result<RuntimeSearchSnapshot, String>),
    Updates(Result<Vec<RuntimeUpdateCheck>, String>),
    Updated {
        previous_runtime_id: RuntimeId,
        result: Result<(RuntimeId, RuntimeListSnapshot), String>,
    },
    Installed {
        runtime_id: RuntimeId,
        result: Result<RuntimeListSnapshot, String>,
    },
    Selected {
        format: ArtifactFormat,
        result: Result<RuntimeListSnapshot, String>,
    },
    ModelSelected {
        model_id: ModelId,
        result: Result<RuntimeListSnapshot, String>,
    },
    ModelSelectionCleared {
        model_id: ModelId,
        result: Result<RuntimeListSnapshot, String>,
    },
    ModelCandidates {
        model_id: ModelId,
        result: Result<Vec<RuntimeModelCandidate>, String>,
    },
    Removed {
        runtime_id: RuntimeId,
        result: Result<RuntimeListSnapshot, String>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SettingsScope {
    Global,
    Engine(String),
    Profile(ServeProfileName),
    BuilderProfile(String),
    Model(ModelId),
}

#[derive(Debug, Clone)]
pub enum SettingsAction {
    Refresh,
    Set {
        scope: SettingsScope,
        id: LoadSettingId,
        value: LoadSettingValue,
    },
    Unset {
        scope: SettingsScope,
        id: LoadSettingId,
    },
    CreateProfile(ServeProfileName),
    ForkBuilderProfile {
        source: Box<norted_core::ServeProfile>,
        name: ServeProfileName,
    },
    DeleteProfile(ServeProfileName),
    AssignProfile {
        model_id: ModelId,
        profile: Option<ServeProfileName>,
    },
    AssignBuilderProfile {
        model_id: ModelId,
        profile_id: String,
    },
    InheritRecommendedProfile {
        model_id: ModelId,
    },
    InspectModel {
        model: Box<ModelArtifact>,
        serve_profile: Option<Box<norted_core::ServeProfile>>,
    },
}

#[derive(Debug)]
pub struct ModelSettingsInspection {
    pub profiles: ServeProfilesState,
    pub runtime_id: RuntimeId,
    pub schema: LoadSettingsSchema,
    pub resolved: ResolvedLoadSettings,
}

#[derive(Debug)]
pub enum SettingsTaskResult {
    Loaded(Result<ServeProfilesState, String>),
    Stored(Result<ServeProfilesState, String>),
    Inspected {
        model_id: ModelId,
        result: Result<ModelSettingsInspection, String>,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SettingsInputKind {
    ProfileName,
    ForkProfile,
    SettingValue,
}

#[derive(Debug, Clone)]
pub struct SettingsInput {
    pub kind: SettingsInputKind,
    pub text: String,
    pub cursor: usize,
    setting_id: Option<LoadSettingId>,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
}

pub struct App {
    pub snapshot: AppSnapshot,
    pub public_auth_status: PublicAuthStatus,
    pub public_auth_loading: bool,
    pub public_auth_error: Option<String>,
    pub control: Option<ControlStatus>,
    pub control_observation_error: Option<String>,
    pub no_color: bool,
    pub unicode: bool,
    pub screen: Screen,
    pub overlay: Option<Overlay>,
    pub command_active: bool,
    pub command_input: String,
    pub command_cursor: usize,
    pub suggestion_index: usize,
    pub suggestion_scroll: usize,
    pub notice: Option<String>,
    pub logs: Vec<LogEntry>,
    pub focus: FocusArea,
    pub nav_focus: Screen,
    pub hover: Option<HoverTarget>,
    pub selected_model: Option<usize>,
    pub model_scroll: usize,
    pub log_scroll: usize,
    pub runtime_list: Option<RuntimeListSnapshot>,
    pub runtime_list_error: Option<String>,
    pub runtime_list_loading: bool,
    pub selected_runtime: Option<usize>,
    pub runtime_scroll: usize,
    pub runtime_search: Option<RuntimeSearchSnapshot>,
    pub runtime_search_error: Option<String>,
    pub runtime_search_loading: bool,
    pub runtime_search_query: String,
    pub runtime_search_cursor: usize,
    pub runtime_search_focus: RuntimeSearchFocus,
    pub selected_runtime_search_result: Option<usize>,
    pub runtime_search_scroll: usize,
    pub runtime_search_context: Option<String>,
    pub runtime_search_model: Option<Box<ModelArtifact>>,
    pub runtime_operation: Option<RuntimeOperationProgress>,
    pub runtime_updates: BTreeMap<RuntimeId, RuntimeUpdateState>,
    pub runtime_update_loading: bool,
    pub runtime_update_error: Option<String>,
    pub runtime_picker_selection: Option<usize>,
    pub runtime_picker_scroll: usize,
    pub runtime_picker_candidates: Vec<RuntimeModelCandidate>,
    pub runtime_picker_loading: bool,
    pub runtime_picker_error: Option<String>,
    pub load_profiles: Option<ServeProfilesState>,
    pub load_profiles_error: Option<String>,
    pub load_profiles_loading: bool,
    pub load_setting_definitions: Vec<LoadSettingDefinition>,
    serve_profile_definitions: Vec<LoadSettingDefinition>,
    pub settings_scope_index: usize,
    pub settings_setting_index: usize,
    pub settings_scroll: usize,
    pub settings_model: Option<ModelId>,
    pub settings_schema: Option<LoadSettingsSchema>,
    pub settings_resolved: Option<ResolvedLoadSettings>,
    pub settings_runtime_id: Option<RuntimeId>,
    pub settings_validation_error: Option<String>,
    pub settings_input: Option<SettingsInput>,
    pub load_animation_frame: u32,
    focus_before_command: FocusArea,
    pending_control_action: Option<ControlAction>,
    control_busy: bool,
    pending_runtime_action: Option<RuntimeAction>,
    runtime_mutation_busy: bool,
    pending_runtime_remove_confirmation: Option<RuntimeId>,
    pending_settings_action: Option<SettingsAction>,
    settings_busy: bool,
    seen_runtime_events: BTreeSet<(i64, u8, String)>,
}

impl App {
    pub fn new(
        snapshot: AppSnapshot,
        public_auth_status: PublicAuthStatus,
        no_color: bool,
        unicode: bool,
        load_setting_definitions: Vec<LoadSettingDefinition>,
    ) -> Self {
        let mut logs = vec![LogEntry {
            level: LogLevel::Info,
            message: "Control core initialized".into(),
        }];
        logs.extend(
            snapshot
                .registry_warnings
                .iter()
                .cloned()
                .map(|message| LogEntry {
                    level: LogLevel::Warning,
                    message,
                }),
        );
        Self {
            snapshot,
            public_auth_status,
            public_auth_loading: true,
            public_auth_error: None,
            control: None,
            control_observation_error: None,
            no_color,
            unicode,
            screen: Screen::Overview,
            overlay: None,
            command_active: false,
            command_input: String::new(),
            command_cursor: 0,
            suggestion_index: 0,
            suggestion_scroll: 0,
            notice: None,
            logs,
            focus: FocusArea::Navigation,
            nav_focus: Screen::Overview,
            hover: None,
            selected_model: None,
            model_scroll: 0,
            log_scroll: 0,
            runtime_list: None,
            runtime_list_error: None,
            runtime_list_loading: true,
            selected_runtime: None,
            runtime_scroll: 0,
            runtime_search: None,
            runtime_search_error: None,
            runtime_search_loading: false,
            runtime_search_query: String::new(),
            runtime_search_cursor: 0,
            runtime_search_focus: RuntimeSearchFocus::Query,
            selected_runtime_search_result: None,
            runtime_search_scroll: 0,
            runtime_search_context: None,
            runtime_search_model: None,
            runtime_operation: None,
            runtime_updates: BTreeMap::new(),
            runtime_update_loading: false,
            runtime_update_error: None,
            runtime_picker_selection: None,
            runtime_picker_scroll: 0,
            runtime_picker_candidates: Vec::new(),
            runtime_picker_loading: false,
            runtime_picker_error: None,
            load_profiles: None,
            load_profiles_error: None,
            load_profiles_loading: true,
            load_setting_definitions,
            serve_profile_definitions: serve_profile_editor_definitions(),
            settings_scope_index: 0,
            settings_setting_index: 0,
            settings_scroll: 0,
            settings_model: None,
            settings_schema: None,
            settings_resolved: None,
            settings_runtime_id: None,
            settings_validation_error: None,
            settings_input: None,
            load_animation_frame: 0,
            focus_before_command: FocusArea::Navigation,
            pending_control_action: None,
            control_busy: false,
            pending_runtime_action: None,
            runtime_mutation_busy: false,
            pending_runtime_remove_confirmation: None,
            pending_settings_action: None,
            settings_busy: false,
            seen_runtime_events: BTreeSet::new(),
        }
    }

    pub fn suggestions(&self) -> Vec<&'static commands::SlashCommand> {
        commands::suggestions(&self.command_input)
    }

    pub fn handle_key(&mut self, key: KeyEvent, layout: &UiLayout) -> Update {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Update::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Update::Quit;
        }
        if self.settings_input.is_some() {
            return self.handle_settings_input_key(key);
        }
        if let Some(overlay) = self.overlay {
            return match overlay {
                Overlay::Help => match key.code {
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter => {
                        self.overlay = None;
                        Update::Render
                    }
                    _ => Update::None,
                },
                Overlay::RuntimeSearch => self.handle_runtime_search_key(key, layout),
                Overlay::ModelRuntime => self.handle_model_runtime_key(key, layout),
            };
        }
        if self.command_active {
            return self.handle_command_key(key);
        }
        if key.code != KeyCode::Char('d') {
            self.pending_runtime_remove_confirmation = None;
        }
        match key.code {
            KeyCode::Char('/') => {
                self.open_command(true);
                self.command_input = "/".into();
                self.command_cursor = 1;
                Update::Render
            }
            KeyCode::Char('?') => {
                self.overlay = Some(Overlay::Help);
                Update::Render
            }
            KeyCode::Char('q') => Update::Quit,
            KeyCode::Tab => {
                self.cycle_focus(1);
                Update::Render
            }
            KeyCode::BackTab => {
                self.cycle_focus(-1);
                Update::Render
            }
            _ => match self.focus {
                FocusArea::Navigation => self.handle_navigation_key(key),
                FocusArea::Content => self.handle_content_key(key, layout),
                FocusArea::Command => {
                    if key.code == KeyCode::Enter {
                        self.open_command(false);
                        Update::Render
                    } else {
                        Update::None
                    }
                }
            },
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, layout: &UiLayout) -> Update {
        if let Some(overlay) = self.overlay {
            if overlay == Overlay::Help {
                if self.hover.take().is_some() {
                    return Update::Render;
                }
                return Update::None;
            }
            let position = Position::new(mouse.column, mouse.row);
            return match mouse.kind {
                MouseEventKind::Moved => {
                    let hover = layout.hit_test(position);
                    if hover == self.hover {
                        Update::None
                    } else {
                        self.hover = hover;
                        Update::Render
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => match overlay {
                    Overlay::RuntimeSearch => self.handle_runtime_search_click(position, layout),
                    Overlay::ModelRuntime => self.handle_model_runtime_click(position, layout),
                    Overlay::Help => Update::None,
                },
                MouseEventKind::ScrollUp => match overlay {
                    Overlay::RuntimeSearch => self.scroll_runtime_search(-3, layout),
                    Overlay::ModelRuntime => self.scroll_model_runtime_picker(-3, layout),
                    Overlay::Help => Update::None,
                },
                MouseEventKind::ScrollDown => match overlay {
                    Overlay::RuntimeSearch => self.scroll_runtime_search(3, layout),
                    Overlay::ModelRuntime => self.scroll_model_runtime_picker(3, layout),
                    Overlay::Help => Update::None,
                },
                _ => Update::None,
            };
        }
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Moved => {
                let hover = layout.hit_test(position);
                if hover == self.hover {
                    Update::None
                } else {
                    self.hover = hover;
                    Update::Render
                }
            }
            MouseEventKind::Down(MouseButton::Left) => self.handle_click(position, layout),
            MouseEventKind::ScrollUp => self.handle_wheel(position, layout, -1),
            MouseEventKind::ScrollDown => self.handle_wheel(position, layout, 1),
            _ => Update::None,
        }
    }

    pub fn handle_paste(&mut self, text: &str) -> Update {
        let normalized = text.replace(['\r', '\n', '\t'], " ");
        if let Some(input) = &mut self.settings_input {
            let index = byte_index(&input.text, input.cursor);
            input.text.insert_str(index, &normalized);
            input.cursor += normalized.chars().count();
            return Update::Render;
        }
        if self.overlay == Some(Overlay::RuntimeSearch)
            && self.runtime_search_focus == RuntimeSearchFocus::Query
        {
            self.insert_runtime_search_text(&normalized);
            return Update::Render;
        }
        if !self.command_active {
            return Update::None;
        }
        self.insert_text(&normalized);
        self.suggestion_index = 0;
        self.suggestion_scroll = 0;
        Update::Render
    }

    pub fn handle_core_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Log { level, message } => self.push_log(level, message),
            AppEvent::RegistryChanged(state) => match state {
                RegistryState::NotScanned => {}
                RegistryState::Scanning => {
                    self.push_log(LogLevel::Info, "Model registry scan started".into())
                }
                RegistryState::Ready => self.push_log(
                    LogLevel::Info,
                    format!(
                        "Model registry ready: {} discovered",
                        self.snapshot.models.len()
                    ),
                ),
                RegistryState::ReadyWithWarnings { warning_count } => {
                    self.push_log(
                        LogLevel::Warning,
                        format!("Model registry ready with {warning_count} warning(s)"),
                    );
                    for warning in self.snapshot.registry_warnings.clone() {
                        self.push_log(LogLevel::Warning, warning);
                    }
                }
                RegistryState::Failed { message } => self.push_log(
                    LogLevel::Error,
                    format!("Model registry scan failed: {message}"),
                ),
            },
            AppEvent::ServerChanged(state) => self.push_log(
                LogLevel::Info,
                format!("API server state changed to {}", state.label()),
            ),
        }
        self.reconcile_models();
    }

    pub fn replace_snapshot(&mut self, snapshot: AppSnapshot) {
        self.snapshot = snapshot;
        self.reconcile_models();
    }

    pub fn replace_public_auth_status(&mut self, result: Result<PublicAuthStatus, String>) {
        self.public_auth_loading = false;
        match result {
            Ok(status) => {
                self.public_auth_status = status;
                self.public_auth_error = None;
            }
            Err(error) => self.public_auth_error = Some(error),
        }
    }

    pub fn replace_control(
        &mut self,
        control: Option<ControlStatus>,
        observation_error: Option<String>,
    ) {
        if let Some(status) = control {
            self.apply_control_status(ControlStatusSource::Observation, status);
        } else {
            self.control = None;
        }
        self.control_observation_error = observation_error;
    }

    pub fn control_observation_pending(&self) -> bool {
        self.control.is_none() && self.control_observation_error.is_none()
    }

    pub fn is_loading(&self) -> bool {
        self.control
            .as_ref()
            .is_some_and(|status| status.backend.lifecycle.is_loading())
    }

    pub fn load_progress(&self) -> Option<&BackendLoadProgress> {
        if !self.is_loading() {
            return None;
        }
        self.control.as_ref()?.backend.load_progress.as_ref()
    }

    pub fn selected_model_load_progress(&self) -> Option<&BackendLoadProgress> {
        let progress = self.load_progress()?;
        let control = self.control.as_ref()?;
        let selected = self.snapshot.models.get(self.selected_model?)?;
        (control.backend.model_id.as_ref() == Some(&selected.id)).then_some(progress)
    }

    pub fn advance_load_animation(&mut self) -> bool {
        let indeterminate = self
            .load_progress()
            .is_some_and(|progress| progress.fraction.is_none());
        if !indeterminate {
            if self.load_animation_frame != 0 {
                self.load_animation_frame = 0;
                return true;
            }
            return false;
        }
        self.load_animation_frame = self.load_animation_frame.wrapping_add(1);
        true
    }

    pub fn take_control_action(&mut self) -> Option<ControlAction> {
        self.pending_control_action.take()
    }

    pub fn handle_control_result(&mut self, result: Result<ControlStatus, String>) {
        self.control_busy = false;
        match result {
            Ok(status) => {
                if self.apply_control_status(ControlStatusSource::ControlResult, status) {
                    self.control_observation_error = None;
                }
            }
            Err(error) => {
                self.notice = Some(error.clone());
                self.push_log(LogLevel::Error, error);
            }
        }
    }

    pub fn take_runtime_action(&mut self) -> Option<RuntimeAction> {
        self.pending_runtime_action.take()
    }

    pub fn take_settings_action(&mut self) -> Option<SettingsAction> {
        self.pending_settings_action.take()
    }

    pub fn handle_settings_task_result(&mut self, result: SettingsTaskResult) {
        self.settings_busy = false;
        self.load_profiles_loading = false;
        match result {
            SettingsTaskResult::Loaded(result) => match result {
                Ok(state) => {
                    self.load_profiles = Some(state);
                    self.load_profiles_error = None;
                    self.reconcile_settings_selection();
                }
                Err(error) => {
                    self.load_profiles_error = Some(error.clone());
                    self.notice = Some(error);
                }
            },
            SettingsTaskResult::Stored(result) => match result {
                Ok(state) => {
                    self.load_profiles = Some(state);
                    self.load_profiles_error = None;
                    self.notice =
                        Some("Load settings saved; changes apply on the next load".to_owned());
                    self.reconcile_settings_selection();
                    if self.settings_model.is_some() {
                        let _ = self.refresh_model_settings();
                    }
                }
                Err(error) => {
                    self.load_profiles_error = Some(error.clone());
                    self.notice = Some(error);
                }
            },
            SettingsTaskResult::Inspected { model_id, result } => {
                if self.settings_model.as_ref() != Some(&model_id) {
                    return;
                }
                match result {
                    Ok(inspection) => {
                        self.load_profiles = Some(inspection.profiles);
                        self.load_profiles_error = None;
                        self.settings_runtime_id = Some(inspection.runtime_id);
                        self.settings_schema = Some(inspection.schema);
                        self.settings_resolved = Some(inspection.resolved);
                        self.settings_validation_error = None;
                    }
                    Err(error) => {
                        self.settings_schema = None;
                        self.settings_resolved = None;
                        self.settings_runtime_id = None;
                        self.settings_validation_error = Some(error.clone());
                        self.notice = Some(error);
                    }
                }
                self.reconcile_settings_selection();
            }
        }
    }

    pub fn settings_scopes(&self) -> Vec<SettingsScope> {
        let mut scopes = vec![SettingsScope::Global];
        let engines = self
            .load_setting_definitions
            .iter()
            .filter_map(|definition| match &definition.scope {
                LoadSettingScope::Common => None,
                LoadSettingScope::Engine { engine_id } => Some(engine_id.clone()),
            })
            .collect::<BTreeSet<_>>();
        scopes.extend(engines.into_iter().map(SettingsScope::Engine));
        if let Some(state) = &self.load_profiles {
            scopes.extend(state.profiles.keys().cloned().map(SettingsScope::Profile));
        }
        let local_ids = self
            .load_profiles
            .as_ref()
            .map(|state| {
                state
                    .profiles
                    .keys()
                    .map(ToString::to_string)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let builder_ids = self
            .snapshot
            .models
            .iter()
            .filter_map(|model| {
                model
                    .norted_package
                    .as_ref()?
                    .recommended_serve_profile
                    .as_ref()
                    .map(|profile| profile.id.clone())
            })
            .collect::<BTreeSet<_>>();
        scopes.extend(
            builder_ids
                .into_iter()
                .filter(|id| !local_ids.contains(id))
                .map(SettingsScope::BuilderProfile),
        );
        if let Some(model) = &self.settings_model {
            scopes.push(SettingsScope::Model(model.clone()));
        }
        scopes
    }

    pub fn selected_settings_scope(&self) -> Option<SettingsScope> {
        self.settings_scopes()
            .get(self.settings_scope_index)
            .cloned()
    }

    pub fn settings_definitions(&self) -> Vec<&LoadSettingDefinition> {
        let scope = self.selected_settings_scope();
        if matches!(
            scope,
            Some(SettingsScope::Profile(_) | SettingsScope::BuilderProfile(_))
        ) {
            return self
                .serve_profile_definitions
                .iter()
                .chain(self.load_setting_definitions.iter())
                .collect();
        }
        let source = if matches!(scope, Some(SettingsScope::Model(_))) {
            self.settings_schema
                .as_ref()
                .map(|schema| schema.definitions.as_slice())
                .unwrap_or(&self.load_setting_definitions)
        } else {
            &self.load_setting_definitions
        };
        source
            .iter()
            .filter(|definition| match &scope {
                Some(SettingsScope::Global) => definition.scope == LoadSettingScope::Common,
                Some(SettingsScope::Engine(selected)) => match &definition.scope {
                    LoadSettingScope::Common => true,
                    LoadSettingScope::Engine { engine_id } => engine_id == selected,
                },
                Some(
                    SettingsScope::Profile(_)
                    | SettingsScope::BuilderProfile(_)
                    | SettingsScope::Model(_),
                ) => true,
                None => false,
            })
            .collect()
    }

    pub fn settings_value_display(&self, id: &LoadSettingId) -> (String, String, bool) {
        let Some(state) = &self.load_profiles else {
            return ("loading".to_owned(), "state".to_owned(), false);
        };
        let Some(scope) = self.selected_settings_scope() else {
            return (
                "<upstream default>".to_owned(),
                "upstream".to_owned(),
                false,
            );
        };
        if is_serve_profile_editor_field(id) {
            let profile = match &scope {
                SettingsScope::Profile(name) => state.profiles.get(name).cloned(),
                SettingsScope::BuilderProfile(profile_id) => {
                    self.builder_serve_profile(profile_id).cloned()
                }
                SettingsScope::Global | SettingsScope::Engine(_) | SettingsScope::Model(_) => None,
            };
            return profile.map_or_else(
                || ("<not set>".to_owned(), "Serve Profile".to_owned(), false),
                |profile| {
                    (
                        serve_profile_editor_value(&profile, id)
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "<runtime default>".to_owned()),
                        profile.source.to_string(),
                        !profile.read_only,
                    )
                },
            );
        }
        let current = match &scope {
            SettingsScope::Global => state.global_defaults.0.get(id),
            SettingsScope::Engine(engine) => state
                .engine_defaults
                .get(engine)
                .and_then(|patch| patch.0.get(id)),
            SettingsScope::Profile(profile) => state
                .profiles
                .get(profile)
                .and_then(|profile| profile.load.settings.0.get(id)),
            SettingsScope::BuilderProfile(profile_id) => self
                .builder_serve_profile(profile_id)
                .and_then(|profile| profile.load.settings.0.get(id)),
            SettingsScope::Model(model) => state
                .model_defaults
                .get(model)
                .and_then(|patch| patch.0.get(id)),
        };
        if let SettingsScope::Model(_) = scope
            && let Some(setting) = self
                .settings_resolved
                .as_ref()
                .and_then(|resolved| resolved.effective.get(id))
        {
            let set_here = matches!(setting.source, LoadSettingSource::ModelDefault { .. });
            return (
                setting.value.to_string(),
                setting.source.to_string(),
                set_here,
            );
        }
        if let Some(value) = current {
            return (value.to_string(), "set here".to_owned(), true);
        }
        let inherited = match id.namespace() {
            Some(engine) => state
                .engine_defaults
                .get(engine)
                .and_then(|patch| patch.0.get(id))
                .map(|value| (value, format!("engine-default:{engine}"))),
            None => state
                .global_defaults
                .0
                .get(id)
                .map(|value| (value, "global-default".to_owned())),
        };
        inherited.map_or_else(
            || {
                (
                    "<upstream default>".to_owned(),
                    "upstream".to_owned(),
                    false,
                )
            },
            |(value, source)| (value.to_string(), source, false),
        )
    }

    pub fn effective_serve_profile_for_model(
        &self,
        model: &ModelArtifact,
    ) -> Option<norted_core::ServeProfile> {
        let state = self.load_profiles.as_ref()?;
        if state.raw_profile_models.contains(&model.id) {
            return None;
        }
        if let Some(name) = state.model_assignments.get(&model.id) {
            return state.profiles.get(name).cloned();
        }
        if let Some(profile_id) = state.builder_profile_assignments.get(&model.id) {
            return self
                .snapshot
                .models
                .iter()
                .filter_map(|candidate| {
                    candidate
                        .norted_package
                        .as_ref()?
                        .recommended_serve_profile
                        .as_ref()
                })
                .find(|profile| &profile.id == profile_id)
                .cloned();
        }
        model
            .norted_package
            .as_ref()
            .and_then(|package| package.recommended_serve_profile.clone())
    }

    fn builder_serve_profile(&self, profile_id: &str) -> Option<&norted_core::ServeProfile> {
        self.snapshot.models.iter().find_map(|model| {
            let profile = model
                .norted_package
                .as_ref()?
                .recommended_serve_profile
                .as_ref()?;
            (profile.id == profile_id).then_some(profile)
        })
    }

    pub fn serve_profile_status_for_model(&self, model: &ModelArtifact) -> &'static str {
        let Some(state) = &self.load_profiles else {
            return "loading";
        };
        if state.raw_profile_models.contains(&model.id) {
            "None / Raw runtime defaults"
        } else if state.model_assignments.contains_key(&model.id) {
            "User / customized"
        } else if state.builder_profile_assignments.contains_key(&model.id) {
            "Builder profile from registry"
        } else if model
            .norted_package
            .as_ref()
            .and_then(|package| package.recommended_serve_profile.as_ref())
            .is_some()
        {
            "Recommended / canonical"
        } else {
            "None / Raw runtime defaults"
        }
    }

    pub fn runtime_mutation_busy(&self) -> bool {
        self.runtime_mutation_busy
    }

    pub fn handle_runtime_task_result(&mut self, result: RuntimeTaskResult) {
        match result {
            RuntimeTaskResult::Listed(result) => {
                self.runtime_list_loading = false;
                self.apply_runtime_list_result(result);
            }
            RuntimeTaskResult::Searched(result) => {
                self.runtime_search_loading = false;
                match result {
                    Ok(snapshot) => {
                        self.runtime_search = Some(snapshot);
                        self.runtime_search_error = None;
                        self.selected_runtime_search_result =
                            self.runtime_search_indices().first().copied();
                        self.runtime_search_scroll = 0;
                        self.runtime_search_focus = RuntimeSearchFocus::Results;
                    }
                    Err(error) => {
                        self.runtime_search_error = Some(error.clone());
                        self.notice = Some(error);
                    }
                }
            }
            RuntimeTaskResult::Updates(result) => {
                self.runtime_update_loading = false;
                match result {
                    Ok(checks) => {
                        self.runtime_updates = checks
                            .into_iter()
                            .map(|check| (check.runtime.manifest.runtime_id, check.state))
                            .collect();
                        self.runtime_update_error = None;
                        self.notice = Some("Runtime update check completed".to_owned());
                    }
                    Err(error) => {
                        self.runtime_update_error = Some(error.clone());
                        self.notice = Some(error.clone());
                        self.push_log(LogLevel::Error, error);
                    }
                }
            }
            RuntimeTaskResult::Updated {
                previous_runtime_id,
                result,
            } => {
                self.runtime_mutation_busy = false;
                match result {
                    Ok((installed_runtime_id, snapshot)) => {
                        self.runtime_updates.remove(&previous_runtime_id);
                        self.apply_runtime_list(snapshot);
                        self.notice = Some(format!(
                            "Installed update {installed_runtime_id} side by side; selections are unchanged"
                        ));
                        self.push_log(
                            LogLevel::Info,
                            format!(
                                "Runtime update installed side by side: {previous_runtime_id} -> {installed_runtime_id}"
                            ),
                        );
                    }
                    Err(error) => {
                        self.notice = Some(error.clone());
                        self.push_log(LogLevel::Error, error);
                    }
                }
            }
            RuntimeTaskResult::Installed { runtime_id, result } => {
                self.runtime_mutation_busy = false;
                if let Some(search) = &mut self.runtime_search {
                    if let Some(found) = search
                        .results
                        .iter_mut()
                        .find(|result| result.entry.available.runtime_id == runtime_id)
                    {
                        found.installed = result.is_ok();
                    }
                }
                match result {
                    Ok(snapshot) => {
                        self.apply_runtime_list(snapshot);
                        self.notice = Some(format!("Installed runtime {runtime_id}"));
                        self.push_log(
                            LogLevel::Info,
                            format!("Runtime installation completed: {runtime_id}"),
                        );
                    }
                    Err(error) => {
                        self.notice = Some(error.clone());
                        self.push_log(LogLevel::Error, error);
                    }
                }
            }
            RuntimeTaskResult::Selected { format, result } => {
                self.runtime_mutation_busy = false;
                match result {
                    Ok(snapshot) => {
                        let selected = snapshot
                            .selections
                            .format_defaults
                            .get(&format)
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "none".to_owned());
                        self.apply_runtime_list(snapshot);
                        self.notice = Some(format!(
                            "Selected {selected} as the {} runtime",
                            format.as_str().to_ascii_uppercase()
                        ));
                    }
                    Err(error) => {
                        self.notice = Some(error.clone());
                        self.push_log(LogLevel::Error, error);
                    }
                }
            }
            RuntimeTaskResult::ModelSelected { model_id, result } => {
                self.runtime_mutation_busy = false;
                match result {
                    Ok(snapshot) => {
                        let selected = snapshot
                            .selections
                            .model_overrides
                            .get(&model_id)
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "none".to_owned());
                        self.apply_runtime_list(snapshot);
                        self.overlay = None;
                        self.hover = None;
                        self.notice = Some(format!("Selected {selected} for model {model_id}"));
                    }
                    Err(error) => {
                        self.notice = Some(error.clone());
                        self.push_log(LogLevel::Error, error);
                    }
                }
            }
            RuntimeTaskResult::ModelSelectionCleared { model_id, result } => {
                self.runtime_mutation_busy = false;
                match result {
                    Ok(snapshot) => {
                        self.apply_runtime_list(snapshot);
                        self.notice =
                            Some(format!("Cleared the runtime override for model {model_id}"));
                    }
                    Err(error) => {
                        self.notice = Some(error.clone());
                        self.push_log(LogLevel::Error, error);
                    }
                }
            }
            RuntimeTaskResult::ModelCandidates { model_id, result } => {
                self.runtime_picker_loading = false;
                if self
                    .selected_model
                    .and_then(|index| self.snapshot.models.get(index))
                    .is_none_or(|model| model.id != model_id)
                {
                    return;
                }
                match result {
                    Ok(candidates) => {
                        self.runtime_picker_candidates = candidates;
                        self.runtime_picker_error = None;
                        let indices = self.runtime_picker_indices();
                        let selected_id = self.runtime_list.as_ref().and_then(|snapshot| {
                            snapshot.selections.model_overrides.get(&model_id)
                        });
                        self.runtime_picker_selection = selected_id
                            .and_then(|runtime_id| {
                                indices.iter().copied().find(|index| {
                                    self.runtime_list.as_ref().is_some_and(|snapshot| {
                                        snapshot.installed[*index].runtime.manifest.runtime_id
                                            == *runtime_id
                                    })
                                })
                            })
                            .or_else(|| indices.first().copied());
                        self.runtime_picker_scroll = self
                            .runtime_picker_selection
                            .and_then(|selected| {
                                indices.iter().position(|index| *index == selected)
                            })
                            .unwrap_or_default();
                        self.overlay = Some(Overlay::ModelRuntime);
                        self.hover = None;
                        self.notice = None;
                    }
                    Err(error) => {
                        self.runtime_picker_candidates.clear();
                        self.runtime_picker_selection = None;
                        self.runtime_picker_scroll = 0;
                        self.runtime_picker_error = Some(error.clone());
                        self.overlay = Some(Overlay::ModelRuntime);
                        self.hover = None;
                        self.notice = None;
                        self.push_log(LogLevel::Warning, error);
                    }
                }
            }
            RuntimeTaskResult::Removed { runtime_id, result } => {
                self.runtime_mutation_busy = false;
                self.pending_runtime_remove_confirmation = None;
                match result {
                    Ok(snapshot) => {
                        self.runtime_updates.remove(&runtime_id);
                        self.apply_runtime_list(snapshot);
                        self.notice = Some(format!("Removed runtime {runtime_id}"));
                    }
                    Err(error) => {
                        self.notice = Some(error.clone());
                        self.push_log(LogLevel::Error, error);
                    }
                }
            }
        }
    }

    pub fn handle_runtime_progress(&mut self, progress: RuntimeOperationProgress) {
        let failed = progress.phase == RuntimeOperationPhase::Failed;
        self.runtime_operation = Some(progress.clone());
        if failed {
            self.notice = Some(progress.detail.clone());
            self.push_log(LogLevel::Error, progress.detail);
        }
    }

    pub fn runtime_search_indices(&self) -> Vec<usize> {
        let Some(search) = &self.runtime_search else {
            return Vec::new();
        };
        let terms = self
            .runtime_search_query
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>();
        search
            .results
            .iter()
            .enumerate()
            .filter_map(|(index, result)| {
                let available = &result.entry.available;
                let formats = available
                    .supported_formats
                    .iter()
                    .map(|format| format.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let haystack = format!(
                    "{} {} {} {} {} {} {} {}",
                    available.display_name,
                    available.runtime_id,
                    available.identity.engine_id,
                    available.identity.package_family,
                    available.identity.version,
                    available.identity.accelerator,
                    available.identity.variant,
                    formats,
                )
                .to_ascii_lowercase();
                terms
                    .iter()
                    .all(|term| haystack.contains(term))
                    .then_some(index)
            })
            .collect()
    }

    pub fn runtime_picker_indices(&self) -> Vec<usize> {
        let Some(snapshot) = &self.runtime_list else {
            return Vec::new();
        };
        self.runtime_picker_candidates
            .iter()
            .filter_map(|candidate| {
                snapshot
                    .installed
                    .iter()
                    .position(|status| status.runtime.manifest.runtime_id == candidate.runtime_id)
            })
            .collect()
    }

    pub fn runtime_picker_compatibility(
        &self,
        runtime_id: &RuntimeId,
    ) -> Option<&RuntimeCompatibility> {
        self.runtime_picker_candidates
            .iter()
            .find(|candidate| &candidate.runtime_id == runtime_id)
            .map(|candidate| &candidate.compatibility)
    }

    pub fn selected_model_has_runtime_override(&self) -> bool {
        let Some(model) = self
            .selected_model
            .and_then(|index| self.snapshot.models.get(index))
        else {
            return false;
        };
        self.runtime_list
            .as_ref()
            .is_some_and(|snapshot| snapshot.selections.model_overrides.contains_key(&model.id))
    }

    pub fn clear_hover(&mut self) {
        self.hover = None;
    }

    fn push_log(&mut self, level: LogLevel, message: String) {
        if self.log_scroll > 0 {
            self.log_scroll = self.log_scroll.saturating_add(1);
        }
        self.logs.push(LogEntry { level, message });
        if self.logs.len() > 500 {
            self.logs.remove(0);
        }
    }

    fn apply_control_status(&mut self, source: ControlStatusSource, status: ControlStatus) -> bool {
        let previous = self
            .control
            .as_ref()
            .map(|current| (current.backend.generation, current.backend.lifecycle));
        let incoming = (status.backend.generation, status.backend.lifecycle);

        if previous.is_some_and(|(generation, lifecycle)| {
            incoming.0 < generation
                || (incoming.0 == generation && incoming.1.is_loading() && !lifecycle.is_loading())
        }) {
            return false;
        }

        if source == ControlStatusSource::ControlResult {
            self.set_control_result_notice(&status);
        }
        self.ingest_control_events(&status);
        if source == ControlStatusSource::Observation {
            self.reconcile_lifecycle_notice(previous, &status);
        }
        self.control = Some(status);
        true
    }

    fn set_control_result_notice(&mut self, status: &ControlStatus) {
        let lifecycle = status.backend.lifecycle;
        if lifecycle.is_loading() {
            self.notice = Some("Model load started".to_owned());
            self.push_log(LogLevel::Info, "Model load started".to_owned());
            return;
        }

        let runtime = status.backend.runtime_id.as_ref().map(|runtime_id| {
            status.backend.runtime_version.as_deref().map_or_else(
                || runtime_id.to_string(),
                |version| format!("{runtime_id} / {version}"),
            )
        });
        let completion = runtime.map_or_else(
            || format!("Backend is now {lifecycle:?}"),
            |runtime| format!("Backend is now {lifecycle:?} with runtime {runtime}"),
        );
        self.notice = Some(completion.clone());
        self.push_log(
            LogLevel::Info,
            format!("Control operation completed: {completion}"),
        );
    }

    fn reconcile_lifecycle_notice(
        &mut self,
        previous: Option<(u64, BackendLifecycle)>,
        status: &ControlStatus,
    ) {
        let generation = status.backend.generation;
        let lifecycle = status.backend.lifecycle;
        let generation_advanced =
            previous.is_some_and(|(previous_generation, _)| generation > previous_generation);
        let lifecycle_changed =
            previous.is_some_and(|(previous_generation, previous_lifecycle)| {
                generation > previous_generation || lifecycle != previous_lifecycle
            });

        if lifecycle == BackendLifecycle::Failed {
            if let Some(failure) = &status.backend.failure {
                self.notice = Some(failure.clone());
            }
            return;
        }
        if !lifecycle_changed {
            return;
        }

        let previous_lifecycle = previous.map(|(_, lifecycle)| lifecycle);
        let notice = match lifecycle {
            BackendLifecycle::Running
                if generation_advanced || previous_lifecycle == Some(BackendLifecycle::Loading) =>
            {
                Some("Model loaded")
            }
            BackendLifecycle::Stopping
                if generation_advanced || previous_lifecycle == Some(BackendLifecycle::Loading) =>
            {
                Some("Model load stopping")
            }
            BackendLifecycle::Stopped => Some(match previous_lifecycle {
                Some(BackendLifecycle::Loading) => "Model load stopped",
                _ => "Backend stopped",
            }),
            _ => None,
        };
        if let Some(notice) = notice {
            self.notice = Some(notice.to_owned());
            self.push_log(LogLevel::Info, notice.to_owned());
        }
    }

    fn ingest_control_events(&mut self, status: &ControlStatus) {
        for event in &status.recent_events {
            let (level, rank) = match event.level {
                RuntimeNoticeLevel::Info => (LogLevel::Info, 0),
                RuntimeNoticeLevel::Warning => (LogLevel::Warning, 1),
                RuntimeNoticeLevel::Error => (LogLevel::Error, 2),
            };
            let fingerprint = (event.timestamp_unix, rank, event.message.clone());
            if !self.seen_runtime_events.insert(fingerprint) {
                continue;
            }
            if matches!(
                event.level,
                RuntimeNoticeLevel::Warning | RuntimeNoticeLevel::Error
            ) {
                self.notice = Some(event.message.clone());
            }
            self.push_log(level, event.message.clone());
        }
        while self.seen_runtime_events.len() > 512 {
            self.seen_runtime_events.pop_first();
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) -> Update {
        match key.code {
            KeyCode::Esc => {
                self.close_command();
                Update::Render
            }
            KeyCode::Enter => self.submit_command(),
            KeyCode::Up => {
                let len = self.suggestions().len();
                if len > 0 {
                    self.suggestion_index = self.suggestion_index.saturating_sub(1);
                    self.ensure_suggestion_visible();
                }
                Update::Render
            }
            KeyCode::Down | KeyCode::Tab => {
                let len = self.suggestions().len();
                if len > 0 {
                    self.suggestion_index = (self.suggestion_index + 1).min(len - 1);
                    self.ensure_suggestion_visible();
                }
                Update::Render
            }
            KeyCode::BackTab => {
                self.suggestion_index = self.suggestion_index.saturating_sub(1);
                self.ensure_suggestion_visible();
                Update::Render
            }
            KeyCode::Backspace => {
                if self.command_cursor > 0 {
                    self.command_cursor -= 1;
                    let start = byte_index(&self.command_input, self.command_cursor);
                    let end = byte_index(&self.command_input, self.command_cursor + 1);
                    self.command_input.replace_range(start..end, "");
                }
                self.suggestion_index = 0;
                self.suggestion_scroll = 0;
                Update::Render
            }
            KeyCode::Delete => {
                if self.command_cursor < self.command_input.chars().count() {
                    let start = byte_index(&self.command_input, self.command_cursor);
                    let end = byte_index(&self.command_input, self.command_cursor + 1);
                    self.command_input.replace_range(start..end, "");
                }
                self.suggestion_index = 0;
                self.suggestion_scroll = 0;
                Update::Render
            }
            KeyCode::Left => {
                self.command_cursor = self.command_cursor.saturating_sub(1);
                Update::Render
            }
            KeyCode::Right => {
                self.command_cursor =
                    (self.command_cursor + 1).min(self.command_input.chars().count());
                Update::Render
            }
            KeyCode::Home => {
                self.command_cursor = 0;
                Update::Render
            }
            KeyCode::End => {
                self.command_cursor = self.command_input.chars().count();
                Update::Render
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_text(&character.to_string());
                self.suggestion_index = 0;
                self.suggestion_scroll = 0;
                Update::Render
            }
            _ => Update::None,
        }
    }

    fn insert_text(&mut self, text: &str) {
        let index = byte_index(&self.command_input, self.command_cursor);
        self.command_input.insert_str(index, text);
        self.command_cursor += text.chars().count();
    }

    fn submit_command(&mut self) -> Update {
        let selected = self
            .suggestions()
            .get(self.suggestion_index)
            .copied()
            .or_else(|| commands::exact(&self.command_input));
        let Some(command) = selected else {
            self.notice = Some(format!("Unknown command: {}", self.command_input.trim()));
            self.close_command();
            return Update::Render;
        };
        self.close_command();
        self.suggestion_index = 0;
        self.suggestion_scroll = 0;
        self.notice = None;
        match command.action {
            CommandAction::Navigate(screen) => {
                self.screen = screen;
                self.nav_focus = screen;
                self.focus = FocusArea::Content;
                self.hover = None;
                Update::Render
            }
            CommandAction::LoadSelected => self.request_load(),
            CommandAction::Unload => self.request_unload(),
            CommandAction::ShowHelp => {
                self.overlay = Some(Overlay::Help);
                Update::Render
            }
            CommandAction::Quit => Update::Quit,
        }
    }

    fn move_nav_focus(&mut self, direction: isize) {
        let current = Screen::ALL
            .iter()
            .position(|screen| *screen == self.nav_focus)
            .unwrap_or_default() as isize;
        let count = Screen::ALL.len() as isize;
        let next = (current + direction).rem_euclid(count) as usize;
        self.nav_focus = Screen::ALL[next];
    }

    fn ensure_suggestion_visible(&mut self) {
        const VISIBLE_SUGGESTIONS: usize = 8;
        if self.suggestion_index < self.suggestion_scroll {
            self.suggestion_scroll = self.suggestion_index;
        } else if self.suggestion_index >= self.suggestion_scroll + VISIBLE_SUGGESTIONS {
            self.suggestion_scroll = self.suggestion_index + 1 - VISIBLE_SUGGESTIONS;
        }
    }

    fn open_command(&mut self, reset: bool) {
        if !self.command_active {
            self.focus_before_command = self.focus;
        }
        self.command_active = true;
        self.focus = FocusArea::Command;
        if reset {
            self.command_input.clear();
            self.command_cursor = 0;
        }
        self.suggestion_index = 0;
        self.suggestion_scroll = 0;
    }

    fn close_command(&mut self) {
        self.command_active = false;
        self.command_input.clear();
        self.command_cursor = 0;
        self.suggestion_index = 0;
        self.suggestion_scroll = 0;
        self.focus = self.focus_before_command;
    }

    fn cycle_focus(&mut self, direction: isize) {
        let current = match self.focus {
            FocusArea::Navigation => 0_isize,
            FocusArea::Content => 1,
            FocusArea::Command => 2,
        };
        self.focus = match (current + direction).rem_euclid(3) {
            0 => FocusArea::Navigation,
            1 => FocusArea::Content,
            _ => FocusArea::Command,
        };
    }

    fn handle_navigation_key(&mut self, key: KeyEvent) -> Update {
        match key.code {
            KeyCode::Left => {
                self.move_nav_focus(-1);
                Update::Render
            }
            KeyCode::Right => {
                self.move_nav_focus(1);
                Update::Render
            }
            KeyCode::Enter => {
                self.screen = self.nav_focus;
                self.notice = None;
                Update::Render
            }
            _ => Update::None,
        }
    }

    fn handle_content_key(&mut self, key: KeyEvent, layout: &UiLayout) -> Update {
        match self.screen {
            Screen::Models => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_model_selection(-1, layout),
                KeyCode::Down | KeyCode::Char('j') => self.move_model_selection(1, layout),
                KeyCode::PageUp => self.scroll_models(-(layout.model_capacity() as isize), layout),
                KeyCode::PageDown => self.scroll_models(layout.model_capacity() as isize, layout),
                KeyCode::Home => self.select_model(0, layout),
                KeyCode::End if !self.snapshot.models.is_empty() => {
                    self.select_model(self.snapshot.models.len() - 1, layout)
                }
                KeyCode::Enter => self.request_load(),
                KeyCode::Char('u') => self.request_unload(),
                KeyCode::Char('v') => self.open_model_runtime_picker(),
                KeyCode::Char('p') => self.open_model_load_settings(),
                _ => Update::None,
            },
            Screen::Logs => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.scroll_logs(1, layout),
                KeyCode::Down | KeyCode::Char('j') => self.scroll_logs(-1, layout),
                KeyCode::PageUp => self.scroll_logs(layout.log_capacity() as isize, layout),
                KeyCode::PageDown => self.scroll_logs(-(layout.log_capacity() as isize), layout),
                KeyCode::Home => {
                    self.log_scroll = self.logs.len().saturating_sub(layout.log_capacity());
                    Update::Render
                }
                KeyCode::End => {
                    self.log_scroll = 0;
                    Update::Render
                }
                _ => Update::None,
            },
            Screen::Runtimes => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_runtime_selection(-1, layout),
                KeyCode::Down | KeyCode::Char('j') => self.move_runtime_selection(1, layout),
                KeyCode::PageUp => {
                    self.scroll_runtimes(-(layout.runtime_capacity() as isize), layout)
                }
                KeyCode::PageDown => {
                    self.scroll_runtimes(layout.runtime_capacity() as isize, layout)
                }
                KeyCode::Home => self.select_runtime(0, layout),
                KeyCode::End => {
                    let Some(last) = self
                        .runtime_list
                        .as_ref()
                        .and_then(|snapshot| snapshot.installed.len().checked_sub(1))
                    else {
                        return Update::None;
                    };
                    self.select_runtime(last, layout)
                }
                KeyCode::Char('s') => self.open_runtime_search(),
                KeyCode::Char('r') => self.request_runtime_list(),
                KeyCode::Char('u') => self.request_runtime_updates(),
                KeyCode::Char('U') => self.request_selected_runtime_update(),
                KeyCode::Char('d') => self.request_runtime_removal(),
                KeyCode::Char('g') => self.request_runtime_selection(ArtifactFormat::Gguf),
                KeyCode::Char('Q') | KeyCode::Char('2') => {
                    self.request_runtime_selection(ArtifactFormat::Q27)
                }
                KeyCode::Char('N') | KeyCode::Char('3') => {
                    self.request_runtime_selection(ArtifactFormat::Ninfer)
                }
                _ => Update::None,
            },
            Screen::Settings => self.handle_settings_key(key, layout),
            _ => Update::None,
        }
    }

    fn handle_settings_key(&mut self, key: KeyEvent, layout: &UiLayout) -> Update {
        if self.settings_busy {
            return Update::None;
        }
        match key.code {
            KeyCode::Left => self.move_settings_scope(-1),
            KeyCode::Right | KeyCode::Tab => self.move_settings_scope(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_settings_selection(-1, layout),
            KeyCode::Down | KeyCode::Char('j') => self.move_settings_selection(1, layout),
            KeyCode::PageUp => self.scroll_settings(-(layout.settings_capacity() as isize), layout),
            KeyCode::PageDown => self.scroll_settings(layout.settings_capacity() as isize, layout),
            KeyCode::Enter => self.edit_selected_setting(),
            KeyCode::Backspace | KeyCode::Delete => self.clear_selected_setting(),
            KeyCode::Char('n') => {
                self.settings_input = Some(SettingsInput {
                    kind: SettingsInputKind::ProfileName,
                    text: String::new(),
                    cursor: 0,
                    setting_id: None,
                });
                Update::Render
            }
            KeyCode::Char('d') => self.delete_selected_profile(),
            KeyCode::Char('f') => self.fork_selected_builder_profile(),
            KeyCode::Char('p') => self.cycle_model_profile(),
            KeyCode::Char('r') => self.refresh_model_settings(),
            _ => Update::None,
        }
    }

    fn handle_settings_input_key(&mut self, key: KeyEvent) -> Update {
        match key.code {
            KeyCode::Esc => {
                self.settings_input = None;
                Update::Render
            }
            KeyCode::Enter => self.submit_settings_input(),
            KeyCode::Backspace => {
                let input = self.settings_input.as_mut().expect("checked by caller");
                if input.cursor > 0 {
                    input.cursor -= 1;
                    let start = byte_index(&input.text, input.cursor);
                    let end = byte_index(&input.text, input.cursor + 1);
                    input.text.replace_range(start..end, "");
                }
                Update::Render
            }
            KeyCode::Delete => {
                let input = self.settings_input.as_mut().expect("checked by caller");
                if input.cursor < input.text.chars().count() {
                    let start = byte_index(&input.text, input.cursor);
                    let end = byte_index(&input.text, input.cursor + 1);
                    input.text.replace_range(start..end, "");
                }
                Update::Render
            }
            KeyCode::Left => {
                let input = self.settings_input.as_mut().expect("checked by caller");
                input.cursor = input.cursor.saturating_sub(1);
                Update::Render
            }
            KeyCode::Right => {
                let input = self.settings_input.as_mut().expect("checked by caller");
                input.cursor = (input.cursor + 1).min(input.text.chars().count());
                Update::Render
            }
            KeyCode::Home => {
                self.settings_input
                    .as_mut()
                    .expect("checked by caller")
                    .cursor = 0;
                Update::Render
            }
            KeyCode::End => {
                let input = self.settings_input.as_mut().expect("checked by caller");
                input.cursor = input.text.chars().count();
                Update::Render
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let input = self.settings_input.as_mut().expect("checked by caller");
                let index = byte_index(&input.text, input.cursor);
                input.text.insert(index, character);
                input.cursor += 1;
                Update::Render
            }
            _ => Update::None,
        }
    }

    fn submit_settings_input(&mut self) -> Update {
        let Some(input) = self.settings_input.take() else {
            return Update::None;
        };
        match input.kind {
            SettingsInputKind::ProfileName => match ServeProfileName::new(input.text) {
                Ok(name) => self.queue_settings_action(SettingsAction::CreateProfile(name)),
                Err(error) => {
                    self.notice = Some(error.to_string());
                    Update::Render
                }
            },
            SettingsInputKind::ForkProfile => match ServeProfileName::new(input.text) {
                Ok(name) => {
                    let Some(SettingsScope::BuilderProfile(profile_id)) =
                        self.selected_settings_scope()
                    else {
                        self.notice =
                            Some("The Builder Serve Profile is no longer selected".to_owned());
                        return Update::Render;
                    };
                    let Some(source) = self.builder_serve_profile(&profile_id).cloned() else {
                        self.notice =
                            Some("The Builder Serve Profile source is unavailable".to_owned());
                        return Update::Render;
                    };
                    self.queue_settings_action(SettingsAction::ForkBuilderProfile {
                        source: Box::new(source),
                        name,
                    })
                }
                Err(error) => {
                    self.notice = Some(error.to_string());
                    Update::Render
                }
            },
            SettingsInputKind::SettingValue => {
                let Some(id) = input.setting_id else {
                    return Update::None;
                };
                let definition = self
                    .settings_definitions()
                    .into_iter()
                    .find(|definition| definition.id == id)
                    .cloned();
                let Some(definition) = definition else {
                    self.notice = Some("The selected setting is no longer available".to_owned());
                    return Update::Render;
                };
                match definition.parse(&input.text) {
                    Ok(value) => self.set_selected_setting(id, value),
                    Err(error) => {
                        self.notice = Some(error.to_string());
                        Update::Render
                    }
                }
            }
        }
    }

    fn edit_selected_setting(&mut self) -> Update {
        if matches!(
            self.selected_settings_scope(),
            Some(SettingsScope::BuilderProfile(_))
        ) {
            self.notice = Some(
                "Builder Serve Profiles are read-only; press f to fork a mutable local copy"
                    .to_owned(),
            );
            return Update::Render;
        }
        let definition = self
            .settings_definitions()
            .get(self.settings_setting_index)
            .copied()
            .cloned();
        let Some(definition) = definition else {
            return Update::None;
        };
        if !definition.supported {
            self.notice =
                Some(definition.unsupported_reason.unwrap_or_else(|| {
                    "The exact runtime does not support this setting".to_owned()
                }));
            return Update::Render;
        }
        let current = self.current_layer_value(&definition.id);
        match &definition.kind {
            norted_core::LoadSettingKind::Toggle => {
                let value = !matches!(current, Some(LoadSettingValue::Toggle(true)));
                self.set_selected_setting(definition.id, LoadSettingValue::Toggle(value))
            }
            norted_core::LoadSettingKind::OneWayFlag => {
                self.set_selected_setting(definition.id, LoadSettingValue::FlagEnabled)
            }
            norted_core::LoadSettingKind::Choice { choices } => {
                let current = match current {
                    Some(LoadSettingValue::Choice(value)) => choices
                        .iter()
                        .position(|choice| choice == &value)
                        .map(|index| (index + 1) % choices.len()),
                    _ => None,
                }
                .unwrap_or(0);
                let Some(value) = choices.get(current).cloned() else {
                    self.notice = Some("This enum has no available choices".to_owned());
                    return Update::Render;
                };
                self.set_selected_setting(definition.id, LoadSettingValue::Choice(value))
            }
            _ => {
                let text = current.map(|value| value.to_string()).unwrap_or_default();
                let cursor = text.chars().count();
                self.settings_input = Some(SettingsInput {
                    kind: SettingsInputKind::SettingValue,
                    text,
                    cursor,
                    setting_id: Some(definition.id),
                });
                Update::Render
            }
        }
    }

    fn set_selected_setting(&mut self, id: LoadSettingId, value: LoadSettingValue) -> Update {
        let Some(scope) = self.selected_settings_scope() else {
            return Update::None;
        };
        self.queue_settings_action(SettingsAction::Set { scope, id, value })
    }

    fn clear_selected_setting(&mut self) -> Update {
        let id = self
            .settings_definitions()
            .get(self.settings_setting_index)
            .map(|definition| definition.id.clone());
        let (Some(scope), Some(id)) = (self.selected_settings_scope(), id) else {
            return Update::None;
        };
        if self.current_layer_value(&id).is_none() {
            self.notice =
                Some("This layer has no override; the value is already inherited".to_owned());
            return Update::Render;
        }
        self.queue_settings_action(SettingsAction::Unset { scope, id })
    }

    fn current_layer_value(&self, id: &LoadSettingId) -> Option<LoadSettingValue> {
        let state = self.load_profiles.as_ref()?;
        if is_serve_profile_editor_field(id) {
            return match self.selected_settings_scope()? {
                SettingsScope::Profile(name) => state
                    .profiles
                    .get(&name)
                    .cloned()
                    .and_then(|profile| serve_profile_editor_value(&profile, id)),
                SettingsScope::BuilderProfile(profile_id) => self
                    .builder_serve_profile(&profile_id)
                    .and_then(|profile| serve_profile_editor_value(profile, id)),
                SettingsScope::Global | SettingsScope::Engine(_) | SettingsScope::Model(_) => None,
            };
        }
        match self.selected_settings_scope()? {
            SettingsScope::Global => state.global_defaults.0.get(id).cloned(),
            SettingsScope::Engine(engine) => state.engine_defaults.get(&engine)?.0.get(id).cloned(),
            SettingsScope::Profile(profile) => state
                .profiles
                .get(&profile)?
                .load
                .settings
                .0
                .get(id)
                .cloned(),
            SettingsScope::BuilderProfile(profile_id) => self
                .builder_serve_profile(&profile_id)
                .and_then(|profile| profile.load.settings.0.get(id).cloned()),
            SettingsScope::Model(model) => state.model_defaults.get(&model)?.0.get(id).cloned(),
        }
    }

    fn queue_settings_action(&mut self, action: SettingsAction) -> Update {
        self.pending_settings_action = Some(action);
        self.settings_busy = true;
        self.notice = Some("Saving load settings…".to_owned());
        Update::Render
    }

    fn move_settings_scope(&mut self, direction: isize) -> Update {
        let scopes = self.settings_scopes();
        if scopes.is_empty() {
            return Update::None;
        }
        self.settings_scope_index = (self.settings_scope_index as isize + direction)
            .rem_euclid(scopes.len() as isize) as usize;
        self.settings_setting_index = 0;
        self.settings_scroll = 0;
        if matches!(
            self.selected_settings_scope(),
            Some(SettingsScope::Model(_))
        ) {
            return self.refresh_model_settings();
        }
        Update::Render
    }

    fn move_settings_selection(&mut self, direction: isize, layout: &UiLayout) -> Update {
        let len = self.settings_definitions().len();
        if len == 0 {
            return Update::None;
        }
        self.settings_setting_index =
            (self.settings_setting_index as isize + direction).clamp(0, len as isize - 1) as usize;
        let capacity = layout.settings_capacity().max(1);
        if self.settings_setting_index < self.settings_scroll {
            self.settings_scroll = self.settings_setting_index;
        } else if self.settings_setting_index >= self.settings_scroll + capacity {
            self.settings_scroll = self.settings_setting_index + 1 - capacity;
        }
        Update::Render
    }

    fn scroll_settings(&mut self, amount: isize, layout: &UiLayout) -> Update {
        let max_scroll = self
            .settings_definitions()
            .len()
            .saturating_sub(layout.settings_capacity().max(1));
        self.settings_scroll = if amount < 0 {
            self.settings_scroll.saturating_sub(amount.unsigned_abs())
        } else {
            self.settings_scroll
                .saturating_add(amount as usize)
                .min(max_scroll)
        };
        Update::Render
    }

    fn delete_selected_profile(&mut self) -> Update {
        if matches!(
            self.selected_settings_scope(),
            Some(SettingsScope::BuilderProfile(_))
        ) {
            self.notice = Some(
                "Builder Serve Profiles are read-only and cannot be deleted; press f to fork"
                    .to_owned(),
            );
            return Update::Render;
        }
        let Some(SettingsScope::Profile(profile)) = self.selected_settings_scope() else {
            self.notice = Some("Select a named profile before deleting it".to_owned());
            return Update::Render;
        };
        self.queue_settings_action(SettingsAction::DeleteProfile(profile))
    }

    fn fork_selected_builder_profile(&mut self) -> Update {
        let Some(SettingsScope::BuilderProfile(_)) = self.selected_settings_scope() else {
            self.notice = Some("Select a read-only Builder Serve Profile to fork".to_owned());
            return Update::Render;
        };
        self.settings_input = Some(SettingsInput {
            kind: SettingsInputKind::ForkProfile,
            text: String::new(),
            cursor: 0,
            setting_id: None,
        });
        Update::Render
    }

    fn cycle_model_profile(&mut self) -> Update {
        let Some(model_id) = self.settings_model.clone() else {
            self.notice = Some("Open load settings from a selected model first".to_owned());
            return Update::Render;
        };
        let Some(state) = &self.load_profiles else {
            return Update::None;
        };
        let local = state.profiles.keys().cloned().collect::<Vec<_>>();
        let builder = self
            .snapshot
            .models
            .iter()
            .filter_map(|model| {
                model
                    .norted_package
                    .as_ref()?
                    .recommended_serve_profile
                    .as_ref()
                    .map(|profile| profile.id.clone())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if !state.raw_profile_models.contains(&model_id)
            && !state.model_assignments.contains_key(&model_id)
            && !state.builder_profile_assignments.contains_key(&model_id)
        {
            return self.queue_settings_action(SettingsAction::AssignProfile {
                model_id,
                profile: None,
            });
        }
        if state.raw_profile_models.contains(&model_id) {
            if let Some(profile) = local.first() {
                return self.queue_settings_action(SettingsAction::AssignProfile {
                    model_id,
                    profile: Some(profile.clone()),
                });
            }
            if let Some(profile_id) = builder.first() {
                return self.queue_settings_action(SettingsAction::AssignBuilderProfile {
                    model_id,
                    profile_id: profile_id.clone(),
                });
            }
            return self
                .queue_settings_action(SettingsAction::InheritRecommendedProfile { model_id });
        }
        if let Some(current) = state.model_assignments.get(&model_id) {
            if let Some(next) = local
                .iter()
                .position(|profile| profile == current)
                .and_then(|index| local.get(index + 1))
            {
                return self.queue_settings_action(SettingsAction::AssignProfile {
                    model_id,
                    profile: Some(next.clone()),
                });
            }
            if let Some(profile_id) = builder.first() {
                return self.queue_settings_action(SettingsAction::AssignBuilderProfile {
                    model_id,
                    profile_id: profile_id.clone(),
                });
            }
        }
        if let Some(current) = state.builder_profile_assignments.get(&model_id)
            && let Some(next) = builder
                .iter()
                .position(|profile| profile == current)
                .and_then(|index| builder.get(index + 1))
        {
            return self.queue_settings_action(SettingsAction::AssignBuilderProfile {
                model_id,
                profile_id: next.clone(),
            });
        }
        self.queue_settings_action(SettingsAction::InheritRecommendedProfile { model_id })
    }

    fn refresh_model_settings(&mut self) -> Update {
        let model = self.settings_model.as_ref().and_then(|model_id| {
            self.snapshot
                .models
                .iter()
                .find(|model| &model.id == model_id)
                .cloned()
        });
        let Some(model) = model else {
            return Update::Render;
        };
        let serve_profile = self.effective_serve_profile_for_model(&model).map(Box::new);
        self.pending_settings_action = Some(SettingsAction::InspectModel {
            model: Box::new(model),
            serve_profile,
        });
        self.settings_busy = true;
        self.settings_validation_error = None;
        Update::Render
    }

    fn open_model_load_settings(&mut self) -> Update {
        let Some(model) = self
            .selected_model
            .and_then(|index| self.snapshot.models.get(index))
            .cloned()
        else {
            self.notice = Some("Select a model before opening load settings".to_owned());
            return Update::Render;
        };
        self.settings_model = Some(model.id.clone());
        self.screen = Screen::Settings;
        self.nav_focus = Screen::Settings;
        let scopes = self.settings_scopes();
        self.settings_scope_index = scopes
            .iter()
            .position(|scope| scope == &SettingsScope::Model(model.id.clone()))
            .unwrap_or(0);
        self.settings_setting_index = 0;
        self.settings_scroll = 0;
        let serve_profile = self.effective_serve_profile_for_model(&model).map(Box::new);
        self.pending_settings_action = Some(SettingsAction::InspectModel {
            model: Box::new(model),
            serve_profile,
        });
        self.settings_busy = true;
        self.notice = Some("Inspecting exact-runtime load settings…".to_owned());
        Update::Render
    }

    fn reconcile_settings_selection(&mut self) {
        let scope_len = self.settings_scopes().len();
        self.settings_scope_index = self.settings_scope_index.min(scope_len.saturating_sub(1));
        let setting_len = self.settings_definitions().len();
        self.settings_setting_index = self
            .settings_setting_index
            .min(setting_len.saturating_sub(1));
        self.settings_scroll = self.settings_scroll.min(setting_len.saturating_sub(1));
    }

    fn handle_click(&mut self, position: Position, layout: &UiLayout) -> Update {
        match layout.hit_test(position) {
            Some(HoverTarget::Navigation(screen)) => {
                if self.command_active {
                    self.close_command();
                }
                self.focus = FocusArea::Navigation;
                self.nav_focus = screen;
                self.screen = screen;
                self.notice = None;
                Update::Render
            }
            Some(HoverTarget::Model(index)) => {
                if self.command_active {
                    self.close_command();
                }
                self.focus = FocusArea::Content;
                self.selected_model = Some(index);
                Update::Render
            }
            Some(HoverTarget::Runtime(index)) => {
                if self.command_active {
                    self.close_command();
                }
                self.focus = FocusArea::Content;
                self.selected_runtime = Some(index);
                self.pending_runtime_remove_confirmation = None;
                Update::Render
            }
            Some(HoverTarget::RuntimeSearchAction) => {
                self.focus = FocusArea::Content;
                self.open_runtime_search()
            }
            Some(HoverTarget::RuntimeUpdateAction) => {
                self.focus = FocusArea::Content;
                self.request_runtime_updates()
            }
            Some(HoverTarget::SettingsScope(index)) => {
                self.focus = FocusArea::Content;
                self.settings_scope_index =
                    index.min(self.settings_scopes().len().saturating_sub(1));
                self.settings_setting_index = 0;
                self.settings_scroll = 0;
                if matches!(
                    self.selected_settings_scope(),
                    Some(SettingsScope::Model(_))
                ) {
                    self.refresh_model_settings()
                } else {
                    Update::Render
                }
            }
            Some(HoverTarget::Setting(index)) => {
                self.focus = FocusArea::Content;
                if self.settings_setting_index == index {
                    return self.edit_selected_setting();
                }
                self.settings_setting_index = index;
                Update::Render
            }
            Some(HoverTarget::CommandSuggestion(index)) => {
                self.suggestion_index = index;
                self.submit_command()
            }
            Some(HoverTarget::CommandBar) => {
                self.open_command(false);
                Update::Render
            }
            None if layout.contains_content(position) => {
                if self.command_active {
                    self.close_command();
                }
                self.focus = FocusArea::Content;
                Update::Render
            }
            Some(
                HoverTarget::RuntimeSearchInput
                | HoverTarget::RuntimeSearchResult(_)
                | HoverTarget::RuntimeSearchSubmit
                | HoverTarget::RuntimeInstall
                | HoverTarget::RuntimePickerResult(_)
                | HoverTarget::RuntimePickerApply,
            ) => Update::None,
            None => Update::None,
        }
    }

    fn handle_wheel(&mut self, position: Position, layout: &UiLayout, direction: isize) -> Update {
        if self.command_active && layout.contains_suggestions(position) {
            let len = self.suggestions().len();
            if len == 0 {
                return Update::None;
            }
            self.suggestion_index = if direction < 0 {
                self.suggestion_index.saturating_sub(1)
            } else {
                (self.suggestion_index + 1).min(len - 1)
            };
            self.ensure_suggestion_visible();
            return Update::Render;
        }
        if !layout.contains_content(position) {
            return Update::None;
        }
        self.focus = FocusArea::Content;
        match self.screen {
            Screen::Models => self.scroll_models(direction * 3, layout),
            Screen::Logs => self.scroll_logs(direction * -3, layout),
            Screen::Runtimes => self.scroll_runtimes(direction * 3, layout),
            Screen::Settings => self.scroll_settings(direction * 3, layout),
            _ => Update::None,
        }
    }

    fn handle_runtime_search_key(&mut self, key: KeyEvent, layout: &UiLayout) -> Update {
        match key.code {
            KeyCode::Esc => {
                self.overlay = None;
                self.hover = None;
                Update::Render
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.runtime_search_focus = match self.runtime_search_focus {
                    RuntimeSearchFocus::Query => RuntimeSearchFocus::Results,
                    RuntimeSearchFocus::Results => RuntimeSearchFocus::Query,
                };
                Update::Render
            }
            _ => match self.runtime_search_focus {
                RuntimeSearchFocus::Query => self.handle_runtime_search_query_key(key),
                RuntimeSearchFocus::Results => self.handle_runtime_search_result_key(key, layout),
            },
        }
    }

    fn handle_model_runtime_key(&mut self, key: KeyEvent, layout: &UiLayout) -> Update {
        match key.code {
            KeyCode::Esc => {
                self.overlay = None;
                self.hover = None;
                Update::Render
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_model_runtime_picker(-1, layout),
            KeyCode::Down | KeyCode::Char('j') => self.move_model_runtime_picker(1, layout),
            KeyCode::PageUp => {
                self.move_model_runtime_picker(-(layout.runtime_search_capacity() as isize), layout)
            }
            KeyCode::PageDown => {
                self.move_model_runtime_picker(layout.runtime_search_capacity() as isize, layout)
            }
            KeyCode::Home => self.select_model_runtime_position(0, layout),
            KeyCode::End => {
                let indices = self.runtime_picker_indices();
                if indices.is_empty() {
                    Update::None
                } else {
                    self.select_model_runtime_position(indices.len() - 1, layout)
                }
            }
            KeyCode::Enter => self.activate_model_runtime_primary(),
            KeyCode::Char('s') => self.open_runtime_search_for_selected_model(),
            KeyCode::Char('x') | KeyCode::Delete => self.request_clear_model_runtime_selection(),
            _ => Update::None,
        }
    }

    fn handle_model_runtime_click(&mut self, position: Position, layout: &UiLayout) -> Update {
        match layout.hit_test(position) {
            Some(HoverTarget::RuntimePickerResult(index)) => {
                self.runtime_picker_selection = Some(index);
                Update::Render
            }
            Some(HoverTarget::RuntimePickerApply) => self.activate_model_runtime_primary(),
            _ => Update::None,
        }
    }

    fn open_model_runtime_picker(&mut self) -> Update {
        let Some(model) = self
            .selected_model
            .and_then(|index| self.snapshot.models.get(index))
            .cloned()
        else {
            self.notice = Some("Select a model before choosing its runtime".to_owned());
            return Update::Render;
        };
        if self.runtime_list_loading && self.runtime_list.is_none() {
            self.notice = Some("Installed runtimes are still being inspected".to_owned());
            return Update::Render;
        }
        if self.runtime_picker_loading || self.runtime_mutation_busy {
            self.notice = Some("A runtime operation is already in progress".to_owned());
            return Update::Render;
        }
        self.runtime_picker_loading = true;
        self.runtime_picker_candidates.clear();
        self.runtime_picker_error = None;
        let serve_profile = self.effective_serve_profile_for_model(&model).map(Box::new);
        self.pending_runtime_action = Some(RuntimeAction::ModelCandidates {
            model: Box::new(model),
            serve_profile,
        });
        self.notice = Some("Checking installed runtime compatibility…".to_owned());
        Update::Render
    }

    fn request_model_runtime_selection(&mut self) -> Update {
        if self.runtime_mutation_busy {
            self.notice = Some("A runtime operation is already in progress".to_owned());
            return Update::Render;
        }
        let Some(model) = self
            .selected_model
            .and_then(|index| self.snapshot.models.get(index))
            .cloned()
        else {
            self.notice = Some("The selected model is no longer available".to_owned());
            return Update::Render;
        };
        let Some(runtime_id) = self.runtime_picker_selection.and_then(|index| {
            self.runtime_list
                .as_ref()?
                .installed
                .get(index)
                .map(|status| status.runtime.manifest.runtime_id.clone())
        }) else {
            self.notice = Some("Select an installed runtime".to_owned());
            return Update::Render;
        };
        self.pending_runtime_action = Some(RuntimeAction::SelectModel {
            model: Box::new(model),
            runtime_id,
        });
        self.runtime_mutation_busy = true;
        self.runtime_operation = None;
        self.notice = Some("Saving model-specific runtime selection…".to_owned());
        Update::Render
    }

    fn activate_model_runtime_primary(&mut self) -> Update {
        if self.runtime_mutation_busy {
            self.notice = Some("A runtime operation is already in progress".to_owned());
            return Update::Render;
        }
        if self.runtime_picker_indices().is_empty() {
            self.open_runtime_search_for_selected_model()
        } else {
            self.request_model_runtime_selection()
        }
    }

    fn request_clear_model_runtime_selection(&mut self) -> Update {
        if self.runtime_mutation_busy {
            self.notice = Some("A runtime operation is already in progress".to_owned());
            return Update::Render;
        }
        let Some(model_id) = self
            .selected_model
            .and_then(|index| self.snapshot.models.get(index))
            .map(|model| model.id.clone())
        else {
            self.notice = Some("The selected model is no longer available".to_owned());
            return Update::Render;
        };
        if !self.selected_model_has_runtime_override() {
            self.notice = Some("This model has no persisted runtime override".to_owned());
            return Update::Render;
        }
        self.pending_runtime_action = Some(RuntimeAction::ClearModelSelection {
            model_id: model_id.clone(),
        });
        self.runtime_mutation_busy = true;
        self.runtime_operation = None;
        self.notice = Some(format!(
            "Clearing the runtime override for model {model_id}…"
        ));
        Update::Render
    }

    fn move_model_runtime_picker(&mut self, direction: isize, layout: &UiLayout) -> Update {
        let indices = self.runtime_picker_indices();
        if indices.is_empty() {
            return Update::None;
        }
        let current = self
            .runtime_picker_selection
            .and_then(|selected| indices.iter().position(|index| *index == selected))
            .unwrap_or_default();
        let next = (current as isize + direction).clamp(0, indices.len() as isize - 1) as usize;
        self.select_model_runtime_position(next, layout)
    }

    fn select_model_runtime_position(&mut self, position: usize, layout: &UiLayout) -> Update {
        let indices = self.runtime_picker_indices();
        let Some(index) = indices.get(position.min(indices.len().saturating_sub(1))) else {
            return Update::None;
        };
        self.runtime_picker_selection = Some(*index);
        let capacity = layout.runtime_search_capacity().max(1);
        if position < self.runtime_picker_scroll {
            self.runtime_picker_scroll = position;
        } else if position >= self.runtime_picker_scroll + capacity {
            self.runtime_picker_scroll = position + 1 - capacity;
        }
        Update::Render
    }

    fn scroll_model_runtime_picker(&mut self, amount: isize, layout: &UiLayout) -> Update {
        let indices = self.runtime_picker_indices();
        if indices.is_empty() {
            return Update::None;
        }
        let capacity = layout.runtime_search_capacity().max(1);
        let max_scroll = indices.len().saturating_sub(capacity);
        self.runtime_picker_scroll = if amount < 0 {
            self.runtime_picker_scroll
                .saturating_sub(amount.unsigned_abs())
        } else {
            self.runtime_picker_scroll
                .saturating_add(amount as usize)
                .min(max_scroll)
        };
        if let Some(index) = indices.get(self.runtime_picker_scroll) {
            self.runtime_picker_selection = Some(*index);
        }
        Update::Render
    }

    fn handle_runtime_search_query_key(&mut self, key: KeyEvent) -> Update {
        match key.code {
            KeyCode::Enter => self.request_runtime_search(false),
            KeyCode::Down => {
                if self.runtime_search_indices().is_empty() {
                    Update::None
                } else {
                    self.runtime_search_focus = RuntimeSearchFocus::Results;
                    Update::Render
                }
            }
            KeyCode::Backspace => {
                if self.runtime_search_cursor > 0 {
                    self.runtime_search_cursor -= 1;
                    let start = byte_index(&self.runtime_search_query, self.runtime_search_cursor);
                    let end =
                        byte_index(&self.runtime_search_query, self.runtime_search_cursor + 1);
                    self.runtime_search_query.replace_range(start..end, "");
                    self.reconcile_runtime_search_selection();
                }
                Update::Render
            }
            KeyCode::Delete => {
                if self.runtime_search_cursor < self.runtime_search_query.chars().count() {
                    let start = byte_index(&self.runtime_search_query, self.runtime_search_cursor);
                    let end =
                        byte_index(&self.runtime_search_query, self.runtime_search_cursor + 1);
                    self.runtime_search_query.replace_range(start..end, "");
                    self.reconcile_runtime_search_selection();
                }
                Update::Render
            }
            KeyCode::Left => {
                self.runtime_search_cursor = self.runtime_search_cursor.saturating_sub(1);
                Update::Render
            }
            KeyCode::Right => {
                self.runtime_search_cursor =
                    (self.runtime_search_cursor + 1).min(self.runtime_search_query.chars().count());
                Update::Render
            }
            KeyCode::Home => {
                self.runtime_search_cursor = 0;
                Update::Render
            }
            KeyCode::End => {
                self.runtime_search_cursor = self.runtime_search_query.chars().count();
                Update::Render
            }
            KeyCode::F(5) => self.request_runtime_search(true),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_runtime_search_text(&character.to_string());
                Update::Render
            }
            _ => Update::None,
        }
    }

    fn handle_runtime_search_result_key(&mut self, key: KeyEvent, layout: &UiLayout) -> Update {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_runtime_search_selection(-1, layout),
            KeyCode::Down | KeyCode::Char('j') => self.move_runtime_search_selection(1, layout),
            KeyCode::PageUp => self.move_runtime_search_selection(
                -(layout.runtime_search_capacity() as isize),
                layout,
            ),
            KeyCode::PageDown => self
                .move_runtime_search_selection(layout.runtime_search_capacity() as isize, layout),
            KeyCode::Home => self.select_runtime_search_position(0, layout),
            KeyCode::End => {
                let indices = self.runtime_search_indices();
                if indices.is_empty() {
                    Update::None
                } else {
                    self.select_runtime_search_position(indices.len() - 1, layout)
                }
            }
            KeyCode::Enter | KeyCode::Char('i') => self.request_runtime_install(),
            KeyCode::Char('r') | KeyCode::F(5) => self.request_runtime_search(true),
            KeyCode::Char('/') => {
                self.runtime_search_focus = RuntimeSearchFocus::Query;
                Update::Render
            }
            _ => Update::None,
        }
    }

    fn handle_runtime_search_click(&mut self, position: Position, layout: &UiLayout) -> Update {
        match layout.hit_test(position) {
            Some(HoverTarget::RuntimeSearchInput) => {
                self.runtime_search_focus = RuntimeSearchFocus::Query;
                Update::Render
            }
            Some(HoverTarget::RuntimeSearchResult(index)) => {
                self.runtime_search_focus = RuntimeSearchFocus::Results;
                self.selected_runtime_search_result = Some(index);
                Update::Render
            }
            Some(HoverTarget::RuntimeSearchSubmit) => self.request_runtime_search(false),
            Some(HoverTarget::RuntimeInstall) => self.request_runtime_install(),
            _ => Update::None,
        }
    }

    fn open_runtime_search(&mut self) -> Update {
        if self.command_active {
            self.close_command();
        }
        if self.runtime_search_context.take().is_some() {
            self.runtime_search_query.clear();
        }
        self.runtime_search_model = None;
        self.overlay = Some(Overlay::RuntimeSearch);
        self.pending_runtime_remove_confirmation = None;
        self.hover = None;
        self.runtime_search_focus = RuntimeSearchFocus::Query;
        self.runtime_search_cursor = self.runtime_search_query.chars().count();
        let _ = self.request_runtime_search(false);
        Update::Render
    }

    fn open_runtime_search_for_selected_model(&mut self) -> Update {
        if self.runtime_mutation_busy || self.runtime_search_loading {
            self.notice = Some("A runtime operation is already in progress".to_owned());
            return Update::Render;
        }
        let Some(model) = self
            .selected_model
            .and_then(|index| self.snapshot.models.get(index))
            .cloned()
        else {
            self.notice = Some("The selected model is no longer available".to_owned());
            return Update::Render;
        };
        let format = model.format.as_str().to_owned();
        self.runtime_search_context = Some(format!(
            "recommended {} runtimes for {}",
            format.to_ascii_uppercase(),
            model.display_name
        ));
        self.runtime_search_model = Some(Box::new(model));
        self.runtime_search_query = format;
        self.runtime_search_cursor = self.runtime_search_query.chars().count();
        self.reconcile_runtime_search_selection();
        self.overlay = Some(Overlay::RuntimeSearch);
        self.pending_runtime_remove_confirmation = None;
        self.hover = None;
        self.runtime_search_focus = RuntimeSearchFocus::Query;
        let _ = self.request_runtime_search(false);
        Update::Render
    }

    fn request_runtime_list(&mut self) -> Update {
        if self.runtime_list_loading {
            return Update::None;
        }
        self.runtime_list_loading = true;
        self.runtime_list_error = None;
        self.pending_runtime_action = Some(RuntimeAction::RefreshList);
        self.notice = Some("Refreshing installed runtimes…".to_owned());
        Update::Render
    }

    fn request_runtime_updates(&mut self) -> Update {
        self.pending_runtime_remove_confirmation = None;
        if self.runtime_update_loading {
            return Update::None;
        }
        if self.runtime_mutation_busy {
            self.notice = Some("A runtime operation is already in progress".to_owned());
            return Update::Render;
        }
        self.runtime_update_loading = true;
        self.runtime_update_error = None;
        self.pending_runtime_action = Some(RuntimeAction::CheckUpdates);
        self.notice = Some("Checking upstream runtime candidates…".to_owned());
        Update::Render
    }

    fn request_selected_runtime_update(&mut self) -> Update {
        self.pending_runtime_remove_confirmation = None;
        if self.runtime_mutation_busy || self.runtime_update_loading {
            self.notice = Some("A runtime operation is already in progress".to_owned());
            return Update::Render;
        }
        let Some(runtime_id) = self.selected_runtime.and_then(|index| {
            self.runtime_list
                .as_ref()?
                .installed
                .get(index)
                .map(|status| status.runtime.manifest.runtime_id.clone())
        }) else {
            self.notice = Some("Select an installed runtime first".to_owned());
            return Update::Render;
        };
        match self.runtime_updates.get(&runtime_id) {
            Some(RuntimeUpdateState::NewerCompatibleVersion { .. })
            | Some(RuntimeUpdateState::Pinned {
                newer_runtime_id: Some(_),
                ..
            }) => {}
            Some(RuntimeUpdateState::Current) => {
                self.notice = Some("The selected runtime is current".to_owned());
                return Update::Render;
            }
            Some(RuntimeUpdateState::Pinned { .. }) => {
                self.notice = Some("No newer compatible version is available".to_owned());
                return Update::Render;
            }
            Some(state) => {
                self.notice = Some(format!("The selected runtime cannot be updated: {state:?}"));
                return Update::Render;
            }
            None => {
                self.notice = Some("Press u to check for updates first".to_owned());
                return Update::Render;
            }
        }
        self.pending_runtime_action = Some(RuntimeAction::Update(runtime_id.clone()));
        self.runtime_mutation_busy = true;
        self.runtime_operation = None;
        self.notice = Some(format!(
            "Installing the update for {runtime_id} side by side…"
        ));
        Update::Render
    }

    fn request_runtime_removal(&mut self) -> Update {
        if self.runtime_mutation_busy {
            self.notice = Some("A runtime operation is already in progress".to_owned());
            return Update::Render;
        }
        if self.control_observation_pending() {
            self.notice =
                Some("Wait for server control observation before removing a runtime".to_owned());
            return Update::Render;
        }
        let Some(status) = self
            .selected_runtime
            .and_then(|index| self.runtime_list.as_ref()?.installed.get(index))
        else {
            self.notice = Some("Select an installed runtime first".to_owned());
            return Update::Render;
        };
        let runtime_id = status.runtime.manifest.runtime_id.clone();
        if !status.selected_for.is_empty() {
            self.notice = Some(format!(
                "{runtime_id} is selected for {}; remap that selection before removal",
                status.selected_for.join(", ")
            ));
            return Update::Render;
        }
        let active_runtime = self
            .control
            .as_ref()
            .and_then(|control| control.backend.runtime_id.clone());
        if active_runtime.as_ref() == Some(&runtime_id) {
            self.notice = Some("The active runtime cannot be removed".to_owned());
            return Update::Render;
        }
        if self.pending_runtime_remove_confirmation.as_ref() != Some(&runtime_id) {
            self.pending_runtime_remove_confirmation = Some(runtime_id.clone());
            self.notice = Some(format!(
                "Press d again to confirm removal of exact runtime {runtime_id}"
            ));
            return Update::Render;
        }
        self.pending_runtime_action = Some(RuntimeAction::Remove {
            runtime_id: runtime_id.clone(),
        });
        self.runtime_mutation_busy = true;
        self.notice = Some(format!("Removing runtime {runtime_id}…"));
        Update::Render
    }

    fn request_runtime_search(&mut self, force_refresh: bool) -> Update {
        if self.runtime_search_loading {
            return Update::None;
        }
        self.runtime_search_loading = true;
        self.runtime_search_error = None;
        let serve_profile = self
            .runtime_search_model
            .as_deref()
            .and_then(|model| self.effective_serve_profile_for_model(model))
            .map(Box::new);
        self.pending_runtime_action = Some(RuntimeAction::Search {
            query: self.runtime_search_query.clone(),
            force_refresh,
            model: self.runtime_search_model.clone(),
            serve_profile,
        });
        Update::Render
    }

    fn request_runtime_install(&mut self) -> Update {
        if self.runtime_mutation_busy {
            self.notice = Some("A runtime operation is already in progress".to_owned());
            return Update::Render;
        }
        let Some(index) = self.selected_runtime_search_result else {
            self.notice = Some("Select an available runtime to install".to_owned());
            return Update::Render;
        };
        let Some(result) = self
            .runtime_search
            .as_ref()
            .and_then(|snapshot| snapshot.results.get(index))
        else {
            self.notice = Some("The selected search result is no longer available".to_owned());
            return Update::Render;
        };
        if result.installed {
            self.notice = Some("That runtime is already installed".to_owned());
            return Update::Render;
        }
        if let RuntimeCompatibility::Incompatible(reason) = &result.entry.compatibility {
            self.notice = Some(format!("This runtime is incompatible: {reason}"));
            return Update::Render;
        }
        let runtime_id = result.entry.available.runtime_id.clone();
        self.pending_runtime_action = Some(RuntimeAction::Install(runtime_id.clone()));
        self.runtime_mutation_busy = true;
        self.runtime_operation = None;
        self.notice = Some(format!("Installing runtime {runtime_id}…"));
        Update::Render
    }

    fn request_runtime_selection(&mut self, format: ArtifactFormat) -> Update {
        if self.runtime_mutation_busy {
            self.notice = Some("A runtime operation is already in progress".to_owned());
            return Update::Render;
        }
        let Some(index) = self.selected_runtime else {
            self.notice = Some("Select an installed runtime first".to_owned());
            return Update::Render;
        };
        let Some(status) = self
            .runtime_list
            .as_ref()
            .and_then(|snapshot| snapshot.installed.get(index))
        else {
            self.notice = Some("The selected runtime is no longer installed".to_owned());
            return Update::Render;
        };
        if !status.runtime.manifest.supported_formats.contains(&format) {
            self.notice = Some(format!(
                "The selected runtime does not support {}",
                format.as_str().to_ascii_uppercase()
            ));
            return Update::Render;
        }
        if !status.compatibility.is_usable() {
            let reason = match &status.compatibility {
                RuntimeCompatibility::Incompatible(reason) => reason.as_str(),
                _ => "not usable on this host",
            };
            self.notice = Some(format!("The selected runtime is incompatible: {reason}"));
            return Update::Render;
        }
        let runtime_id = status.runtime.manifest.runtime_id.clone();
        self.pending_runtime_action = Some(RuntimeAction::SelectFormat { format, runtime_id });
        self.runtime_mutation_busy = true;
        self.runtime_operation = None;
        self.notice = Some(format!(
            "Selecting runtime for {}…",
            format.as_str().to_ascii_uppercase()
        ));
        Update::Render
    }

    fn insert_runtime_search_text(&mut self, text: &str) {
        let index = byte_index(&self.runtime_search_query, self.runtime_search_cursor);
        self.runtime_search_query.insert_str(index, text);
        self.runtime_search_cursor += text.chars().count();
        self.reconcile_runtime_search_selection();
    }

    fn reconcile_runtime_search_selection(&mut self) {
        let indices = self.runtime_search_indices();
        if indices.is_empty() {
            self.selected_runtime_search_result = None;
            self.runtime_search_scroll = 0;
            return;
        }
        if !self
            .selected_runtime_search_result
            .is_some_and(|selected| indices.contains(&selected))
        {
            self.selected_runtime_search_result = indices.first().copied();
            self.runtime_search_scroll = 0;
        }
        self.runtime_search_scroll = self
            .runtime_search_scroll
            .min(indices.len().saturating_sub(1));
    }

    fn move_runtime_search_selection(&mut self, direction: isize, layout: &UiLayout) -> Update {
        let indices = self.runtime_search_indices();
        if indices.is_empty() {
            return Update::None;
        }
        let current = self
            .selected_runtime_search_result
            .and_then(|selected| indices.iter().position(|index| *index == selected))
            .unwrap_or_default();
        let next = (current as isize + direction).clamp(0, indices.len() as isize - 1) as usize;
        self.select_runtime_search_position(next, layout)
    }

    fn select_runtime_search_position(&mut self, position: usize, layout: &UiLayout) -> Update {
        let indices = self.runtime_search_indices();
        let Some(index) = indices.get(position.min(indices.len().saturating_sub(1))) else {
            return Update::None;
        };
        self.selected_runtime_search_result = Some(*index);
        let capacity = layout.runtime_search_capacity().max(1);
        if position < self.runtime_search_scroll {
            self.runtime_search_scroll = position;
        } else if position >= self.runtime_search_scroll + capacity {
            self.runtime_search_scroll = position + 1 - capacity;
        }
        Update::Render
    }

    fn scroll_runtime_search(&mut self, amount: isize, layout: &UiLayout) -> Update {
        let indices = self.runtime_search_indices();
        if indices.is_empty() {
            return Update::None;
        }
        let capacity = layout.runtime_search_capacity().max(1);
        let max_scroll = indices.len().saturating_sub(capacity);
        self.runtime_search_scroll = if amount < 0 {
            self.runtime_search_scroll
                .saturating_sub(amount.unsigned_abs())
        } else {
            self.runtime_search_scroll
                .saturating_add(amount as usize)
                .min(max_scroll)
        };
        if let Some(index) = indices.get(self.runtime_search_scroll) {
            self.selected_runtime_search_result = Some(*index);
        }
        self.runtime_search_focus = RuntimeSearchFocus::Results;
        Update::Render
    }

    fn move_runtime_selection(&mut self, direction: isize, layout: &UiLayout) -> Update {
        let Some(len) = self
            .runtime_list
            .as_ref()
            .map(|snapshot| snapshot.installed.len())
        else {
            return Update::None;
        };
        if len == 0 {
            return Update::None;
        }
        let next = self.selected_runtime.map_or(0, |current| {
            (current as isize + direction).clamp(0, len as isize - 1) as usize
        });
        self.select_runtime(next, layout)
    }

    fn select_runtime(&mut self, index: usize, layout: &UiLayout) -> Update {
        let Some(len) = self
            .runtime_list
            .as_ref()
            .map(|snapshot| snapshot.installed.len())
        else {
            return Update::None;
        };
        if len == 0 {
            return Update::None;
        }
        let index = index.min(len - 1);
        self.selected_runtime = Some(index);
        self.pending_runtime_remove_confirmation = None;
        let capacity = layout.runtime_capacity().max(1);
        if index < self.runtime_scroll {
            self.runtime_scroll = index;
        } else if index >= self.runtime_scroll + capacity {
            self.runtime_scroll = index + 1 - capacity;
        }
        Update::Render
    }

    fn scroll_runtimes(&mut self, amount: isize, layout: &UiLayout) -> Update {
        let len = self
            .runtime_list
            .as_ref()
            .map_or(0, |snapshot| snapshot.installed.len());
        let capacity = layout.runtime_capacity().max(1);
        let max_scroll = len.saturating_sub(capacity);
        self.runtime_scroll = if amount < 0 {
            self.runtime_scroll.saturating_sub(amount.unsigned_abs())
        } else {
            self.runtime_scroll
                .saturating_add(amount as usize)
                .min(max_scroll)
        };
        Update::Render
    }

    fn apply_runtime_list_result(&mut self, result: Result<RuntimeListSnapshot, String>) {
        match result {
            Ok(snapshot) => self.apply_runtime_list(snapshot),
            Err(error) => {
                self.runtime_list_error = Some(error.clone());
                self.notice = Some(error.clone());
                self.push_log(LogLevel::Error, error);
            }
        }
    }

    fn apply_runtime_list(&mut self, snapshot: RuntimeListSnapshot) {
        let selected_id = self.selected_runtime.and_then(|index| {
            self.runtime_list
                .as_ref()?
                .installed
                .get(index)
                .map(|status| status.runtime.manifest.runtime_id.clone())
        });
        self.runtime_list_error = None;
        self.selected_runtime = selected_id
            .and_then(|runtime_id| {
                snapshot
                    .installed
                    .iter()
                    .position(|status| status.runtime.manifest.runtime_id == runtime_id)
            })
            .or_else(|| (!snapshot.installed.is_empty()).then_some(0));
        self.runtime_scroll = self
            .runtime_scroll
            .min(snapshot.installed.len().saturating_sub(1));
        self.runtime_list = Some(snapshot);
    }

    fn move_model_selection(&mut self, direction: isize, layout: &UiLayout) -> Update {
        if self.snapshot.models.is_empty() {
            return Update::None;
        }
        let next = self.selected_model.map_or(0, |current| {
            (current as isize + direction).clamp(0, self.snapshot.models.len() as isize - 1)
                as usize
        });
        self.select_model(next, layout)
    }

    fn select_model(&mut self, index: usize, layout: &UiLayout) -> Update {
        if self.snapshot.models.is_empty() {
            return Update::None;
        }
        let index = index.min(self.snapshot.models.len() - 1);
        self.selected_model = Some(index);
        let capacity = layout.model_capacity().max(1);
        if index < self.model_scroll {
            self.model_scroll = index;
        } else if index >= self.model_scroll + capacity {
            self.model_scroll = index + 1 - capacity;
        }
        Update::Render
    }

    fn scroll_models(&mut self, amount: isize, layout: &UiLayout) -> Update {
        let capacity = layout.model_capacity().max(1);
        let max_scroll = self.snapshot.models.len().saturating_sub(capacity);
        self.model_scroll = if amount < 0 {
            self.model_scroll.saturating_sub(amount.unsigned_abs())
        } else {
            self.model_scroll
                .saturating_add(amount as usize)
                .min(max_scroll)
        };
        Update::Render
    }

    fn scroll_logs(&mut self, amount: isize, layout: &UiLayout) -> Update {
        let max_scroll = self.logs.len().saturating_sub(layout.log_capacity());
        let current = self.log_scroll.min(max_scroll);
        self.log_scroll = if amount < 0 {
            current.saturating_sub(amount.unsigned_abs())
        } else {
            current.saturating_add(amount as usize).min(max_scroll)
        };
        Update::Render
    }

    fn reconcile_models(&mut self) {
        let len = self.snapshot.models.len();
        self.selected_model = self
            .selected_model
            .and_then(|index| (len > 0).then(|| index.min(len - 1)));
        self.model_scroll = self.model_scroll.min(len.saturating_sub(1));
    }

    fn request_load(&mut self) -> Update {
        if self.control_busy {
            self.notice = Some("A control operation is already in progress".to_owned());
            return Update::Render;
        }
        let Some(index) = self.selected_model else {
            self.notice = Some("Select a model before loading it".to_owned());
            return Update::Render;
        };
        let Some(model) = self.snapshot.models.get(index) else {
            self.notice = Some("The selected model is no longer available".to_owned());
            return Update::Render;
        };
        let Some(control) = &self.control else {
            self.notice = Some(if self.control_observation_pending() {
                "Server control observation is still in progress".to_owned()
            } else {
                "No running Norted Server control instance is available".to_owned()
            });
            return Update::Render;
        };
        if control.backend.lifecycle == BackendLifecycle::Running
            && control.backend.model_id.as_ref() == Some(&model.id)
        {
            self.notice = Some("The selected model is already active".to_owned());
            return Update::Render;
        }
        self.pending_control_action = Some(ControlAction::Load(model.id.clone()));
        self.control_busy = true;
        self.notice = Some(format!("Loading {}…", model.display_name));
        Update::Render
    }

    fn request_unload(&mut self) -> Update {
        if self.control_busy {
            self.notice = Some("A control operation is already in progress".to_owned());
            return Update::Render;
        }
        let Some(control) = &self.control else {
            self.notice = Some(if self.control_observation_pending() {
                "Server control observation is still in progress".to_owned()
            } else {
                "No running Norted Server control instance is available".to_owned()
            });
            return Update::Render;
        };
        if matches!(control.backend.lifecycle, BackendLifecycle::Stopped) {
            self.notice = Some("No model is currently loaded".to_owned());
            return Update::Render;
        }
        self.pending_control_action = Some(ControlAction::Unload);
        self.control_busy = true;
        self.notice = Some("Unloading the active model…".to_owned());
        Update::Render
    }
}

const PROFILE_TEMPERATURE: &str = "profile.generation.temperature";
const PROFILE_TOP_P: &str = "profile.generation.top_p";
const PROFILE_TOP_K: &str = "profile.generation.top_k";
const PROFILE_MIN_P: &str = "profile.generation.min_p";
const PROFILE_REASONING_EFFORT: &str = "profile.generation.reasoning_effort";
const PROFILE_THINKING_DEFAULT: &str = "profile.generation.thinking_default";
const PROFILE_THINKING_REQUIRED: &str = "profile.generation.thinking_required";
const PROFILE_ALLOWED_OVERRIDES: &str = "profile.generation.allowed_request_overrides";
const PROFILE_PROMPT_MODE: &str = "profile.prompt.mode";
const PROFILE_EXTERNAL_TEMPLATE: &str = "profile.prompt.external_template";
const PROFILE_RENDER_GENERATION_PROMPT: &str = "profile.prompt.render_generation_prompt";
const PROFILE_TEMPLATE_THINKING: &str = "profile.prompt.thinking_enabled";
const PROFILE_RESPONSE_FILTER: &str = "profile.prompt.response_filter";

fn serve_profile_editor_definitions() -> Vec<LoadSettingDefinition> {
    let definition = |id: &str,
                      label: &str,
                      description: &str,
                      kind: LoadSettingKind,
                      upstream_default: &str| LoadSettingDefinition {
        id: LoadSettingId::new(id).expect("static Serve Profile editor setting ID is valid"),
        label: label.to_owned(),
        description: description.to_owned(),
        kind,
        scope: LoadSettingScope::Common,
        supported: true,
        unsupported_reason: None,
        unit: None,
        upstream_default: Some(upstream_default.to_owned()),
        recommendation: None,
    };
    vec![
        definition(
            PROFILE_TEMPERATURE,
            "Profile temperature",
            "Optional Serve Profile generation default",
            LoadSettingKind::Float {
                minimum: Some(0.0),
                maximum: None,
            },
            "runtime default",
        ),
        definition(
            PROFILE_TOP_P,
            "Profile top-p",
            "Optional Serve Profile nucleus-sampling default",
            LoadSettingKind::Float {
                minimum: Some(0.0),
                maximum: Some(1.0),
            },
            "runtime default",
        ),
        definition(
            PROFILE_TOP_K,
            "Profile top-k",
            "Optional Serve Profile top-k default",
            LoadSettingKind::UnsignedInteger {
                minimum: None,
                maximum: None,
            },
            "runtime default",
        ),
        definition(
            PROFILE_MIN_P,
            "Profile min-p",
            "Optional Serve Profile minimum-probability default",
            LoadSettingKind::Float {
                minimum: Some(0.0),
                maximum: Some(1.0),
            },
            "runtime default",
        ),
        definition(
            PROFILE_REASONING_EFFORT,
            "Profile reasoning effort",
            "Optional reasoning-effort default passed by the Serve Profile",
            LoadSettingKind::String,
            "runtime default",
        ),
        definition(
            PROFILE_THINKING_DEFAULT,
            "Profile thinking default",
            "Select the effective thinking state",
            LoadSettingKind::Toggle,
            "false",
        ),
        definition(
            PROFILE_THINKING_REQUIRED,
            "Profile thinking required",
            "Prevent permitted invocation settings from disabling thinking",
            LoadSettingKind::Toggle,
            "false",
        ),
        definition(
            PROFILE_ALLOWED_OVERRIDES,
            "Allowed request overrides",
            "Comma-separated request fields that may override profile generation defaults",
            LoadSettingKind::String,
            "none",
        ),
        definition(
            PROFILE_PROMPT_MODE,
            "Profile prompt mode",
            "Use the runtime prompt path or the configured external template",
            LoadSettingKind::Choice {
                choices: vec!["runtime_default".to_owned(), "external_template".to_owned()],
            },
            "runtime_default",
        ),
        definition(
            PROFILE_EXTERNAL_TEMPLATE,
            "External template",
            "Enter identity | path | sha256; setting this selects external-template/raw-completions mode",
            LoadSettingKind::String,
            "none",
        ),
        definition(
            PROFILE_RENDER_GENERATION_PROMPT,
            "Render generation prompt",
            "Append the template's generation-prompt framing",
            LoadSettingKind::Toggle,
            "false",
        ),
        definition(
            PROFILE_TEMPLATE_THINKING,
            "Template thinking state",
            "Render the external template with thinking enabled",
            LoadSettingKind::Toggle,
            "false",
        ),
        definition(
            PROFILE_RESPONSE_FILTER,
            "Profile response filter",
            "Select a typed response filter supported by the target adapter",
            LoadSettingKind::Choice {
                choices: vec!["none".to_owned(), "dirk_sharp_reasoning".to_owned()],
            },
            "none",
        ),
    ]
}

pub(crate) fn is_serve_profile_editor_field(id: &LoadSettingId) -> bool {
    matches!(
        id.as_str(),
        PROFILE_TEMPERATURE
            | PROFILE_TOP_P
            | PROFILE_TOP_K
            | PROFILE_MIN_P
            | PROFILE_REASONING_EFFORT
            | PROFILE_THINKING_DEFAULT
            | PROFILE_THINKING_REQUIRED
            | PROFILE_ALLOWED_OVERRIDES
            | PROFILE_PROMPT_MODE
            | PROFILE_EXTERNAL_TEMPLATE
            | PROFILE_RENDER_GENERATION_PROMPT
            | PROFILE_TEMPLATE_THINKING
            | PROFILE_RESPONSE_FILTER
    )
}

fn serve_profile_editor_value(
    profile: &ServeProfile,
    id: &LoadSettingId,
) -> Option<LoadSettingValue> {
    match id.as_str() {
        PROFILE_TEMPERATURE => profile
            .generation
            .defaults
            .temperature
            .map(LoadSettingValue::Float),
        PROFILE_TOP_P => profile
            .generation
            .defaults
            .top_p
            .map(LoadSettingValue::Float),
        PROFILE_TOP_K => profile
            .generation
            .defaults
            .top_k
            .map(LoadSettingValue::UnsignedInteger),
        PROFILE_MIN_P => profile
            .generation
            .defaults
            .min_p
            .map(LoadSettingValue::Float),
        PROFILE_REASONING_EFFORT => profile
            .generation
            .defaults
            .reasoning_effort
            .clone()
            .map(LoadSettingValue::String),
        PROFILE_THINKING_DEFAULT => Some(LoadSettingValue::Toggle(
            profile.generation.thinking.default,
        )),
        PROFILE_THINKING_REQUIRED => Some(LoadSettingValue::Toggle(
            profile.generation.thinking.required,
        )),
        PROFILE_ALLOWED_OVERRIDES => Some(LoadSettingValue::String(
            profile.generation.allowed_user_overrides.join(", "),
        )),
        PROFILE_PROMPT_MODE => Some(LoadSettingValue::Choice(
            match profile.prompt.mode {
                PromptMode::RuntimeDefault => "runtime_default",
                PromptMode::ExternalTemplate => "external_template",
            }
            .to_owned(),
        )),
        PROFILE_EXTERNAL_TEMPLATE => profile.prompt.template.as_ref().map(|template| {
            LoadSettingValue::String(format!(
                "{} | {} | {}",
                template.identity,
                template.path.display(),
                template.sha256
            ))
        }),
        PROFILE_RENDER_GENERATION_PROMPT => Some(LoadSettingValue::Toggle(
            profile.prompt.render_generation_prompt,
        )),
        PROFILE_TEMPLATE_THINKING => {
            Some(LoadSettingValue::Toggle(profile.prompt.thinking_enabled))
        }
        PROFILE_RESPONSE_FILTER => Some(LoadSettingValue::Choice(
            match profile.prompt.response_filter {
                ResponseFilter::None => "none",
                ResponseFilter::DirkSharpReasoning => "dirk_sharp_reasoning",
            }
            .to_owned(),
        )),
        _ => None,
    }
}

pub(crate) fn apply_serve_profile_editor_value(
    profile: &mut ServeProfile,
    id: &LoadSettingId,
    value: Option<LoadSettingValue>,
) -> Result<(), LoadSettingsError> {
    if profile.read_only || profile.source != ServeProfileSource::UserLocal {
        return Err(LoadSettingsError::InvalidServeProfile(format!(
            "Builder Serve Profile `{}` is read-only; fork it before editing",
            profile.id
        )));
    }
    let invalid = |message: &str| {
        LoadSettingsError::InvalidServeProfile(format!("Serve Profile field `{id}` {message}"))
    };
    match (id.as_str(), value) {
        (PROFILE_TEMPERATURE, Some(LoadSettingValue::Float(value))) => {
            profile.generation.defaults.temperature = Some(value);
        }
        (PROFILE_TOP_P, Some(LoadSettingValue::Float(value))) => {
            profile.generation.defaults.top_p = Some(value);
        }
        (PROFILE_TOP_K, Some(LoadSettingValue::UnsignedInteger(value))) => {
            profile.generation.defaults.top_k = Some(value);
        }
        (PROFILE_MIN_P, Some(LoadSettingValue::Float(value))) => {
            profile.generation.defaults.min_p = Some(value);
        }
        (PROFILE_REASONING_EFFORT, Some(LoadSettingValue::String(value))) => {
            profile.generation.defaults.reasoning_effort = Some(value);
        }
        (PROFILE_THINKING_DEFAULT, Some(LoadSettingValue::Toggle(value))) => {
            profile.generation.thinking.default = value;
        }
        (PROFILE_THINKING_REQUIRED, Some(LoadSettingValue::Toggle(value))) => {
            profile.generation.thinking.required = value;
        }
        (PROFILE_ALLOWED_OVERRIDES, Some(LoadSettingValue::String(value))) => {
            profile.generation.allowed_user_overrides = value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
        }
        (PROFILE_PROMPT_MODE, Some(LoadSettingValue::Choice(mode))) => match mode.as_str() {
            "runtime_default" => profile.prompt = ServePromptProfile::default(),
            "external_template" if profile.prompt.template.is_some() => {
                profile.prompt.mode = PromptMode::ExternalTemplate;
                profile.prompt.delivery = PromptDelivery::RawCompletions;
            }
            "external_template" => {
                return Err(invalid(
                    "needs an external template; edit the `External template` row first",
                ));
            }
            _ => return Err(invalid("has an unknown prompt mode")),
        },
        (PROFILE_EXTERNAL_TEMPLATE, Some(LoadSettingValue::String(value))) => {
            let mut fields = value.splitn(3, '|').map(str::trim);
            let identity = fields.next().unwrap_or_default();
            let path = fields.next().unwrap_or_default();
            let sha256 = fields.next().unwrap_or_default();
            if identity.is_empty() || path.is_empty() || sha256.is_empty() {
                return Err(invalid("requires `identity | path | sha256`"));
            }
            let path = std::fs::canonicalize(path).map_err(|error| {
                invalid(&format!(
                    "could not resolve template path `{path}`: {error}"
                ))
            })?;
            profile.prompt.mode = PromptMode::ExternalTemplate;
            profile.prompt.delivery = PromptDelivery::RawCompletions;
            profile.prompt.template = Some(ExternalTemplateReference {
                identity: identity.to_owned(),
                path,
                sha256: sha256.to_owned(),
            });
        }
        (PROFILE_RENDER_GENERATION_PROMPT, Some(LoadSettingValue::Toggle(value))) => {
            profile.prompt.render_generation_prompt = value;
        }
        (PROFILE_TEMPLATE_THINKING, Some(LoadSettingValue::Toggle(value))) => {
            profile.prompt.thinking_enabled = value;
        }
        (PROFILE_RESPONSE_FILTER, Some(LoadSettingValue::Choice(filter))) => {
            profile.prompt.response_filter = match filter.as_str() {
                "none" => ResponseFilter::None,
                "dirk_sharp_reasoning" => ResponseFilter::DirkSharpReasoning,
                _ => return Err(invalid("has an unknown response filter")),
            };
        }
        (PROFILE_TEMPERATURE, None) => profile.generation.defaults.temperature = None,
        (PROFILE_TOP_P, None) => profile.generation.defaults.top_p = None,
        (PROFILE_TOP_K, None) => profile.generation.defaults.top_k = None,
        (PROFILE_MIN_P, None) => profile.generation.defaults.min_p = None,
        (PROFILE_REASONING_EFFORT, None) => profile.generation.defaults.reasoning_effort = None,
        (PROFILE_THINKING_DEFAULT, None) => profile.generation.thinking.default = false,
        (PROFILE_THINKING_REQUIRED, None) => profile.generation.thinking.required = false,
        (PROFILE_ALLOWED_OVERRIDES, None) => {
            profile.generation.allowed_user_overrides.clear();
        }
        (PROFILE_PROMPT_MODE | PROFILE_EXTERNAL_TEMPLATE, None) => {
            profile.prompt = ServePromptProfile::default();
        }
        (PROFILE_RENDER_GENERATION_PROMPT, None) => {
            profile.prompt.render_generation_prompt = false;
        }
        (PROFILE_TEMPLATE_THINKING, None) => profile.prompt.thinking_enabled = false,
        (PROFILE_RESPONSE_FILTER, None) => profile.prompt.response_filter = ResponseFilter::None,
        _ => return Err(invalid("received a value of the wrong type")),
    }
    profile
        .validate()
        .map_err(LoadSettingsError::InvalidServeProfile)
}

fn byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

#[cfg(test)]
mod tests {
    use norted_core::{
        AppSnapshot, EffectivePublicAuthMode, PublicAuthMode, PublicAuthStatus, RegistryState,
        ServeProfile, ServeProfileSource, ServerState,
    };
    use norted_engine::{
        BackendLifecycle, BackendLoadPhase, BackendLoadProgress, BackendStatus, ControlStatus,
        RuntimeNotice, RuntimeNoticeLevel,
    };

    use super::{
        App, PROFILE_ALLOWED_OVERRIDES, PROFILE_EXTERNAL_TEMPLATE, PROFILE_MIN_P,
        PROFILE_PROMPT_MODE, PROFILE_REASONING_EFFORT, PROFILE_RENDER_GENERATION_PROMPT,
        PROFILE_RESPONSE_FILTER, PROFILE_TEMPERATURE, PROFILE_TEMPLATE_THINKING,
        PROFILE_THINKING_DEFAULT, PROFILE_THINKING_REQUIRED, PROFILE_TOP_K, PROFILE_TOP_P,
        apply_serve_profile_editor_value, serve_profile_editor_definitions,
    };

    fn test_app() -> App {
        App::new(
            AppSnapshot {
                server: ServerState::Stopped,
                registry_state: RegistryState::Ready,
                models: Vec::new(),
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

    fn status(generation: u64, lifecycle: BackendLifecycle) -> ControlStatus {
        ControlStatus {
            public_endpoint: Some("http://127.0.0.1:8080".to_owned()),
            available_engine_count: 1,
            installed_engine_count: 1,
            running_engine_count: usize::from(lifecycle == BackendLifecycle::Running),
            engines: Vec::new(),
            backend: BackendStatus {
                generation,
                lifecycle,
                model_id: None,
                engine_id: None,
                runtime_id: None,
                runtime_version: None,
                runtime_variant: None,
                runtime_executable_sha256: None,
                process_id: None,
                private_endpoint: None,
                load_progress: None,
                failure: None,
                provenance: None,
            },
            recent_events: Vec::new(),
        }
    }

    #[test]
    fn mutable_profile_editor_covers_common_fields_and_rejects_builder_mutation() {
        let definitions = serve_profile_editor_definitions();
        let ids = definitions
            .iter()
            .map(|definition| definition.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        for expected in [
            PROFILE_TEMPERATURE,
            PROFILE_TOP_P,
            PROFILE_TOP_K,
            PROFILE_MIN_P,
            PROFILE_REASONING_EFFORT,
            PROFILE_THINKING_DEFAULT,
            PROFILE_THINKING_REQUIRED,
            PROFILE_ALLOWED_OVERRIDES,
            PROFILE_PROMPT_MODE,
            PROFILE_EXTERNAL_TEMPLATE,
            PROFILE_RENDER_GENERATION_PROMPT,
            PROFILE_TEMPLATE_THINKING,
            PROFILE_RESPONSE_FILTER,
        ] {
            assert!(ids.contains(expected), "missing editor row {expected}");
        }

        let mut profile = ServeProfile::local("editable");
        let set = |profile: &mut ServeProfile, id: &str, value: norted_core::LoadSettingValue| {
            apply_serve_profile_editor_value(
                profile,
                &norted_core::LoadSettingId::new(id).unwrap(),
                Some(value),
            )
            .unwrap();
        };
        set(
            &mut profile,
            PROFILE_TEMPERATURE,
            norted_core::LoadSettingValue::Float(0.7),
        );
        set(
            &mut profile,
            PROFILE_TOP_P,
            norted_core::LoadSettingValue::Float(0.9),
        );
        set(
            &mut profile,
            PROFILE_TOP_K,
            norted_core::LoadSettingValue::UnsignedInteger(32),
        );
        set(
            &mut profile,
            PROFILE_MIN_P,
            norted_core::LoadSettingValue::Float(0.05),
        );
        set(
            &mut profile,
            PROFILE_REASONING_EFFORT,
            norted_core::LoadSettingValue::String("high".to_owned()),
        );
        set(
            &mut profile,
            PROFILE_THINKING_DEFAULT,
            norted_core::LoadSettingValue::Toggle(true),
        );
        set(
            &mut profile,
            PROFILE_THINKING_REQUIRED,
            norted_core::LoadSettingValue::Toggle(true),
        );
        set(
            &mut profile,
            PROFILE_ALLOWED_OVERRIDES,
            norted_core::LoadSettingValue::String("top_p, temperature, top_p".to_owned()),
        );
        let template = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        set(
            &mut profile,
            PROFILE_EXTERNAL_TEMPLATE,
            norted_core::LoadSettingValue::String(format!(
                "sharp-test | {} | {}",
                template.display(),
                "a".repeat(64)
            )),
        );
        set(
            &mut profile,
            PROFILE_RENDER_GENERATION_PROMPT,
            norted_core::LoadSettingValue::Toggle(true),
        );
        set(
            &mut profile,
            PROFILE_TEMPLATE_THINKING,
            norted_core::LoadSettingValue::Toggle(true),
        );
        set(
            &mut profile,
            PROFILE_RESPONSE_FILTER,
            norted_core::LoadSettingValue::Choice("dirk_sharp_reasoning".to_owned()),
        );

        assert_eq!(profile.generation.defaults.temperature, Some(0.7));
        assert_eq!(profile.generation.defaults.top_p, Some(0.9));
        assert_eq!(profile.generation.defaults.top_k, Some(32));
        assert_eq!(profile.generation.defaults.min_p, Some(0.05));
        assert_eq!(
            profile.generation.defaults.reasoning_effort.as_deref(),
            Some("high")
        );
        assert!(profile.generation.thinking.default);
        assert!(profile.generation.thinking.required);
        assert_eq!(
            profile.generation.allowed_user_overrides,
            vec!["temperature".to_owned(), "top_p".to_owned()]
        );
        assert_eq!(
            profile.prompt.mode,
            norted_core::PromptMode::ExternalTemplate
        );
        assert_eq!(
            profile.prompt.delivery,
            norted_core::PromptDelivery::RawCompletions
        );
        assert_eq!(
            profile
                .prompt
                .template
                .as_ref()
                .map(|template| template.identity.as_str()),
            Some("sharp-test")
        );
        assert!(profile.prompt.render_generation_prompt);
        assert!(profile.prompt.thinking_enabled);
        assert_eq!(
            profile.prompt.response_filter,
            norted_core::ResponseFilter::DirkSharpReasoning
        );

        set(
            &mut profile,
            PROFILE_PROMPT_MODE,
            norted_core::LoadSettingValue::Choice("runtime_default".to_owned()),
        );
        assert_eq!(profile.prompt, norted_core::ServePromptProfile::default());

        let mut builder = profile;
        builder.source = ServeProfileSource::BuilderRecommended;
        builder.read_only = true;
        let error = apply_serve_profile_editor_value(
            &mut builder,
            &norted_core::LoadSettingId::new(PROFILE_TEMPERATURE).unwrap(),
            Some(norted_core::LoadSettingValue::Float(0.1)),
        )
        .unwrap_err();
        assert!(error.to_string().contains("read-only"));
        assert_eq!(builder.generation.defaults.temperature, Some(0.7));
    }

    fn lifecycle(app: &App) -> BackendLifecycle {
        app.control.as_ref().unwrap().backend.lifecycle
    }

    #[test]
    fn loading_to_running_reports_success() {
        let mut app = test_app();
        app.handle_control_result(Ok(status(7, BackendLifecycle::Loading)));
        assert_eq!(app.notice.as_deref(), Some("Model load started"));

        app.replace_control(Some(status(7, BackendLifecycle::Running)), None);

        assert_eq!(lifecycle(&app), BackendLifecycle::Running);
        assert_eq!(app.notice.as_deref(), Some("Model loaded"));
    }

    #[test]
    fn loading_to_failed_surfaces_exact_backend_failure() {
        let mut app = test_app();
        app.handle_control_result(Ok(status(8, BackendLifecycle::Loading)));
        let mut failed = status(8, BackendLifecycle::Failed);
        failed.backend.failure = Some("q27 rejected tensor metadata at offset 42".to_owned());

        app.replace_control(Some(failed), None);

        assert_eq!(lifecycle(&app), BackendLifecycle::Failed);
        assert_eq!(
            app.notice.as_deref(),
            Some("q27 rejected tensor metadata at offset 42")
        );
    }

    #[test]
    fn stale_loading_admission_cannot_regress_failed_status_or_notice() {
        let mut app = test_app();
        let mut failed = status(9, BackendLifecycle::Failed);
        failed.backend.failure = Some("runtime exited before readiness probe".to_owned());
        app.replace_control(Some(failed), None);

        app.handle_control_result(Ok(status(9, BackendLifecycle::Loading)));

        assert_eq!(lifecycle(&app), BackendLifecycle::Failed);
        assert_eq!(
            app.notice.as_deref(),
            Some("runtime exited before readiness probe")
        );
    }

    #[test]
    fn stale_loading_admission_cannot_regress_running_status() {
        let mut app = test_app();
        app.replace_control(Some(status(10, BackendLifecycle::Running)), None);

        app.handle_control_result(Ok(status(10, BackendLifecycle::Loading)));

        assert_eq!(lifecycle(&app), BackendLifecycle::Running);
    }

    #[test]
    fn lower_generation_status_cannot_replace_newer_status() {
        let mut app = test_app();
        app.replace_control(Some(status(12, BackendLifecycle::Running)), None);

        app.replace_control(Some(status(11, BackendLifecycle::Stopped)), None);

        assert_eq!(
            app.control.as_ref().unwrap().backend.generation,
            12,
            "the newer generation must remain displayed"
        );
        assert_eq!(lifecycle(&app), BackendLifecycle::Running);
    }

    #[test]
    fn same_generation_loading_status_refreshes_progress_fields() {
        let mut app = test_app();
        let mut initial = status(13, BackendLifecycle::Loading);
        initial.backend.load_progress = Some(BackendLoadProgress::with_message(
            BackendLoadPhase::PreparingModel,
            "Preparing model input",
        ));
        app.replace_control(Some(initial), None);

        let mut refreshed = status(13, BackendLifecycle::Loading);
        refreshed.backend.engine_id = Some("q27".to_owned());
        refreshed.backend.load_progress = Some(BackendLoadProgress::with_message(
            BackendLoadPhase::PreparingLaunch,
            "Revalidating package before launch",
        ));
        app.replace_control(Some(refreshed), None);

        let backend = &app.control.as_ref().unwrap().backend;
        assert_eq!(backend.engine_id.as_deref(), Some("q27"));
        assert_eq!(
            backend
                .load_progress
                .as_ref()
                .and_then(|progress| progress.message.as_deref()),
            Some("Revalidating package before launch")
        );
    }

    #[test]
    fn running_to_failed_is_accepted_and_surfaces_failure() {
        let mut app = test_app();
        app.replace_control(Some(status(14, BackendLifecycle::Running)), None);
        let mut failed = status(14, BackendLifecycle::Failed);
        failed.backend.failure = Some("backend process exited with status 137".to_owned());

        app.replace_control(Some(failed), None);

        assert_eq!(lifecycle(&app), BackendLifecycle::Failed);
        assert_eq!(
            app.notice.as_deref(),
            Some("backend process exited with status 137")
        );
    }

    #[test]
    fn repeated_failed_observation_restores_failure_without_a_new_runtime_event() {
        let mut app = test_app();
        app.replace_control(Some(status(15, BackendLifecycle::Loading)), None);
        let failure = "runtime health check failed: connection refused";
        let mut failed = status(15, BackendLifecycle::Failed);
        failed.backend.failure = Some(failure.to_owned());
        failed.recent_events.push(RuntimeNotice {
            timestamp_unix: 123,
            level: RuntimeNoticeLevel::Error,
            message: failure.to_owned(),
        });
        app.replace_control(Some(failed.clone()), None);
        assert_eq!(app.notice.as_deref(), Some(failure));

        app.notice = Some("Model load started".to_owned());
        app.replace_control(Some(failed), None);

        assert_eq!(app.notice.as_deref(), Some(failure));
    }

    #[test]
    fn loading_admission_for_newer_generation_is_accepted() {
        let mut app = test_app();
        let mut failed = status(20, BackendLifecycle::Failed);
        failed.backend.failure = Some("previous load failed".to_owned());
        app.replace_control(Some(failed), None);

        app.handle_control_result(Ok(status(21, BackendLifecycle::Loading)));

        assert_eq!(app.control.as_ref().unwrap().backend.generation, 21);
        assert_eq!(lifecycle(&app), BackendLifecycle::Loading);
        assert_eq!(app.notice.as_deref(), Some("Model load started"));
    }
}
