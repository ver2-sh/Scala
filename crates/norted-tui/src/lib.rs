//! Interactive terminal interface for Norted Server.

mod app;
mod commands;
mod terminal;
mod theme;
mod ui;

use std::sync::Arc;
use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use norted_core::{
    ApiKeyStore, AppPaths, ApplicationCore, EngineId, ModelProfilesStore, PublicAuthStatus,
    ServerConfig, SettingDefinition, SettingId, SettingsError, SettingsPatch, SettingsStore,
};
use norted_engine::{
    BackendLifecycle, ControlClient, ControlClientError, ControlStatus, RuntimePackManager,
};
use norted_model_library::ModelLibrary;

use app::{
    App, ControlAction, ModelLibraryAction, ModelLibraryTaskResult, ModelSettingsInspection,
    ProfileEngineSelection, RuntimeAction, RuntimeTaskResult, SettingsAction, SettingsScope,
    SettingsTaskResult, Update,
};
use terminal::TerminalSession;
use ui::layout::UiLayout;

const CONTROL_IDLE_CADENCE: Duration = Duration::from_secs(2);
const CONTROL_LOADING_CADENCE: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
enum LocalLoadIntent {
    #[default]
    Idle,
    PendingAdmission,
    Accepted {
        generation: u64,
    },
}

#[derive(Debug, Default)]
struct ControlPollState {
    initial_observation_pending: bool,
    local_load_intent: LocalLoadIntent,
    observed_loading: bool,
}

impl ControlPollState {
    fn new() -> Self {
        Self {
            initial_observation_pending: true,
            ..Self::default()
        }
    }

    fn next_delay(&mut self) -> Option<Duration> {
        if self.initial_observation_pending {
            self.initial_observation_pending = false;
            None
        } else {
            Some(self.cadence())
        }
    }

    fn set_local_load_intent(&mut self, intent: LocalLoadIntent) {
        self.local_load_intent = intent;
    }

    fn record_observation(&mut self, observation: Option<(u64, BackendLifecycle)>) {
        self.observed_loading = observation.is_some_and(|(_, lifecycle)| lifecycle.is_loading());
        if let LocalLoadIntent::Accepted { generation } = self.local_load_intent
            && observation.is_some_and(|(observed_generation, _)| observed_generation == generation)
        {
            self.local_load_intent = LocalLoadIntent::Idle;
        }
    }

    fn cadence(&self) -> Duration {
        if self.local_load_intent != LocalLoadIntent::Idle || self.observed_loading {
            CONTROL_LOADING_CADENCE
        } else {
            CONTROL_IDLE_CADENCE
        }
    }
}

pub async fn run(
    core: Arc<ApplicationCore>,
    runtime_packs: Arc<RuntimePackManager>,
    model_library: Arc<ModelLibrary>,
    setting_definitions: Vec<SettingDefinition>,
) -> Result<()> {
    let mut terminal = TerminalSession::enter()?;
    let mut core_events = core.subscribe();
    let snapshot = core.snapshot().await;
    let initial_auth_status = core.config.server.public_auth_status(0)?;
    let mut app = App::new(
        snapshot,
        initial_auth_status,
        core.config.tui.no_color,
        core.config.tui.unicode,
        setting_definitions,
    );
    let mut layout = UiLayout::default();
    terminal.draw(|frame| layout = ui::render(frame, &app))?;

    core.start_model_discovery().await;
    let mut terminal_events = EventStream::new();
    let (control_updates, mut control_update_receiver) = tokio::sync::mpsc::channel(2);
    let (control_results, mut control_result_receiver) =
        tokio::sync::mpsc::channel::<std::result::Result<ControlStatus, String>>(2);
    let (runtime_results, mut runtime_result_receiver) = tokio::sync::mpsc::channel(4);
    let (model_library_results, mut model_library_result_receiver) = tokio::sync::mpsc::channel(4);
    let (settings_results, mut settings_result_receiver) = tokio::sync::mpsc::channel(4);
    let (auth_updates, mut auth_update_receiver) = tokio::sync::mpsc::channel(2);
    let mut runtime_progress = runtime_packs.progress();
    let mut model_progress = model_library.subscribe();
    spawn_runtime_action(
        Arc::clone(&runtime_packs),
        core.paths.clone(),
        runtime_results.clone(),
        RuntimeAction::RefreshList,
    );
    spawn_settings_action(
        Arc::clone(&runtime_packs),
        core.paths.clone(),
        settings_results.clone(),
        SettingsAction::Refresh,
    );
    let auth_paths = core.paths.clone();
    let auth_server_config = core.config.server.clone();
    let auth_observer = tokio::spawn(async move {
        observe_public_auth(auth_paths, auth_server_config, auth_updates).await;
    });
    let observer_core = Arc::clone(&core);
    let observer_paths = core.paths.clone();
    let (local_load_intent, mut local_load_intent_receiver) =
        tokio::sync::watch::channel(LocalLoadIntent::Idle);
    let runtime_observer = tokio::spawn(async move {
        let mut poll_state = ControlPollState::new();
        loop {
            if let Some(delay) = poll_state.next_delay() {
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    changed = local_load_intent_receiver.changed() => {
                        if changed.is_err() {
                            break;
                        }
                    }
                }
            }

            poll_state.set_local_load_intent(*local_load_intent_receiver.borrow_and_update());
            let (_, observation) = tokio::join!(
                observer_core.refresh_server_state(),
                observe_control(&observer_paths)
            );
            // A local load can finish while the status request is outstanding.
            // Consume the newest intent after the observation without allowing
            // an early Stopped response to erase a still-pending load intent.
            poll_state.set_local_load_intent(*local_load_intent_receiver.borrow_and_update());
            poll_state.record_observation(
                observation
                    .status
                    .as_ref()
                    .map(|status| (status.backend.generation, status.backend.lifecycle)),
            );
            if control_updates.send(observation).await.is_err() {
                break;
            }
        }
    });
    let mut render = false;
    let mut render_tick = tokio::time::interval(Duration::from_millis(150));
    render_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if render {
            terminal.draw(|frame| layout = ui::render(frame, &app))?;
        }
        let update = tokio::select! {
            event = terminal_events.next() => match event {
                Some(Ok(Event::Key(key))) => app.handle_key(key, &layout),
                Some(Ok(Event::Mouse(mouse))) => app.handle_mouse(mouse, &layout),
                Some(Ok(Event::Paste(text))) => app.handle_paste(&text),
                Some(Ok(Event::Resize(_, _))) => {
                    app.clear_hover();
                    Update::Render
                },
                Some(Ok(_)) => Update::None,
                Some(Err(error)) => return Err(error.into()),
                None => Update::Quit,
            },
            event = core_events.recv() => match event {
                Ok(event) => {
                    app.replace_snapshot(core.snapshot().await);
                    app.handle_core_event(event);
                    Update::Render
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    app.replace_snapshot(core.snapshot().await);
                    Update::Render
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => Update::None,
            },
            observation = control_update_receiver.recv() => match observation {
                Some(observation) => {
                    app.replace_control(observation.status, observation.error);
                    Update::Render
                }
                None => Update::None,
            },
            result = control_result_receiver.recv() => match result {
                Some(result) => {
                    let intent = match &result {
                        Ok(status) if status.backend.lifecycle.is_loading() => {
                            LocalLoadIntent::Accepted {
                                generation: status.backend.generation,
                            }
                        }
                        _ => LocalLoadIntent::Idle,
                    };
                    local_load_intent.send_replace(intent);
                    app.handle_control_result(result);
                    Update::Render
                }
                None => Update::None,
            },
            result = runtime_result_receiver.recv() => match result {
                Some(result) => {
                    app.handle_runtime_task_result(result);
                    Update::Render
                }
                None => Update::None,
            },
            result = model_library_result_receiver.recv() => match result {
                Some(result) => {
                    app.handle_model_library_result(result);
                    Update::Render
                }
                None => Update::None,
            },
            result = settings_result_receiver.recv() => match result {
                Some(result) => {
                    app.handle_settings_task_result(result);
                    Update::Render
                }
                None => Update::None,
            },
            result = auth_update_receiver.recv() => match result {
                Some(result) => {
                    app.replace_public_auth_status(result);
                    Update::Render
                }
                None => Update::None,
            },
            progress = runtime_progress.recv() => match progress {
                Ok(progress) => {
                    app.handle_runtime_progress(progress);
                    Update::Render
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => Update::Render,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => Update::None,
            },
            progress = model_progress.recv() => match progress {
                Ok(progress) => {
                    app.handle_model_operation_progress(progress);
                    Update::Render
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => Update::Render,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => Update::None,
            },
            _ = render_tick.tick() => {
                if app.advance_load_animation() {
                    Update::Render
                } else {
                    Update::None
                }
            }
        };
        if update == Update::Quit {
            break;
        }
        if let Some(action) = app.take_control_action() {
            if matches!(action, ControlAction::Load(_)) {
                local_load_intent.send_replace(LocalLoadIntent::PendingAdmission);
            }
            let paths = core.paths.clone();
            let results = control_results.clone();
            tokio::spawn(async move {
                let result = execute_control(&paths, action).await;
                let _ = results.send(result).await;
            });
        }
        if let Some(action) = app.take_runtime_action() {
            spawn_runtime_action(
                Arc::clone(&runtime_packs),
                core.paths.clone(),
                runtime_results.clone(),
                action,
            );
        }
        if let Some(action) = app.take_model_library_action() {
            spawn_model_library_action(
                Arc::clone(&model_library),
                Arc::clone(&core),
                model_library_results.clone(),
                action,
            );
        }
        if let Some(action) = app.take_settings_action() {
            spawn_settings_action(
                Arc::clone(&runtime_packs),
                core.paths.clone(),
                settings_results.clone(),
                action,
            );
        }
        render = update == Update::Render;
    }
    runtime_observer.abort();
    auth_observer.abort();
    terminal.leave()?;
    Ok(())
}

fn spawn_model_library_action(
    library: Arc<ModelLibrary>,
    core: Arc<ApplicationCore>,
    results: tokio::sync::mpsc::Sender<ModelLibraryTaskResult>,
    action: ModelLibraryAction,
) {
    tokio::spawn(async move {
        let result = match action {
            ModelLibraryAction::Search { query, format } => ModelLibraryTaskResult::Searched(
                library
                    .search(&query, format)
                    .await
                    .map_err(|error| error.to_string()),
            ),
            ModelLibraryAction::Download(model_ref) => {
                let result = match library.download(&model_ref).await {
                    Ok(model) => match core.refresh_models().await {
                        Ok(()) => Ok(model),
                        Err(error) => Err(error.to_string()),
                    },
                    Err(error) => Err(error.to_string()),
                };
                ModelLibraryTaskResult::Downloaded(Box::new(result))
            }
            ModelLibraryAction::Remove(model_id) => {
                let result = match core.model(&model_id).await {
                    Some(model) => match library.remove(&model).await {
                        Ok(()) => match core.refresh_models().await {
                            Ok(()) => Ok(model_id),
                            Err(error) => Err(error.to_string()),
                        },
                        Err(error) => Err(error.to_string()),
                    },
                    None => Err(format!("model `{model_id}` was not found")),
                };
                ModelLibraryTaskResult::Removed(result)
            }
        };
        let _ = results.send(result).await;
    });
}

async fn observe_public_auth(
    paths: AppPaths,
    server: ServerConfig,
    updates: tokio::sync::mpsc::Sender<Result<PublicAuthStatus, String>>,
) {
    let store = ApiKeyStore::new(&paths);
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        refresh.tick().await;
        let result = match store.active_count().await {
            Ok(active) => server
                .public_auth_status(active)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        if updates.send(result).await.is_err() {
            break;
        }
    }
}

fn spawn_settings_action(
    runtime_packs: Arc<RuntimePackManager>,
    paths: AppPaths,
    results: tokio::sync::mpsc::Sender<SettingsTaskResult>,
    action: SettingsAction,
) {
    tokio::spawn(async move {
        let result = execute_settings_action(runtime_packs, &paths, action).await;
        let _ = results.send(result).await;
    });
}

async fn read_tui_settings(
    settings: &SettingsStore,
    profiles: &ModelProfilesStore,
) -> std::result::Result<(norted_core::SettingsState, norted_core::ModelProfilesState), String> {
    let (settings, profiles) = tokio::join!(settings.read(), profiles.read());
    Ok((
        settings.map_err(|error| error.to_string())?,
        profiles.map_err(|error| error.to_string())?,
    ))
}

fn resolved_patch(
    engine_id: &str,
    profile_id: &norted_core::ModelProfileId,
    patch: &SettingsPatch,
) -> norted_core::ResolvedSettings {
    norted_core::ResolvedSettings {
        engine_id: engine_id.to_owned(),
        model_profile_id: Some(profile_id.clone()),
        effective: patch
            .0
            .iter()
            .map(|(id, value)| {
                (
                    id.clone(),
                    norted_core::ResolvedSetting {
                        value: value.clone(),
                        source: norted_core::SettingSource::ModelProfile {
                            model_profile_id: profile_id.clone(),
                        },
                    },
                )
            })
            .collect(),
    }
}

async fn execute_settings_action(
    runtime_packs: Arc<RuntimePackManager>,
    paths: &AppPaths,
    action: SettingsAction,
) -> SettingsTaskResult {
    let settings_store = SettingsStore::new(paths);
    let profiles_store = ModelProfilesStore::new(paths);
    match action {
        SettingsAction::Refresh => {
            SettingsTaskResult::Loaded(read_tui_settings(&settings_store, &profiles_store).await)
        }
        SettingsAction::Set {
            scope,
            id,
            value,
            model,
        } => {
            let mut patch = SettingsPatch::default();
            patch.insert(id, value);
            if patch.0.keys().any(|id| id.as_str() == "q27.template_path")
                && let Err(error) = norted_engine::record_local_file_setting_identity(
                    &mut patch,
                    "q27.template_path",
                    "q27.template_sha256",
                    &paths.data_dir,
                    4 * 1024 * 1024,
                )
                .await
            {
                return SettingsTaskResult::Stored(Err(error.to_string()));
            }
            for (path_setting, sha_setting, maximum_bytes) in [
                (
                    "llama.cpp.chat_template_file",
                    "llama.cpp.chat_template_sha256",
                    4_u64 * 1024 * 1024,
                ),
                (
                    "llama.cpp.speculative_draft_model",
                    "llama.cpp.speculative_draft_sha256",
                    1024_u64 * 1024 * 1024 * 1024,
                ),
            ] {
                if patch.0.keys().any(|id| id.as_str() == path_setting)
                    && let Err(error) = norted_engine::record_local_file_setting_identity(
                        &mut patch,
                        path_setting,
                        sha_setting,
                        &paths.data_dir,
                        maximum_bytes,
                    )
                    .await
                {
                    return SettingsTaskResult::Stored(Err(error.to_string()));
                }
            }
            if let SettingsScope::ModelProfile(profile_id) = &scope {
                let Some(model) = model.as_deref() else {
                    return SettingsTaskResult::Stored(Err(format!(
                        "Bound model for Model Profile `{profile_id}` is unavailable"
                    )));
                };
                let profiles = match profiles_store.read().await {
                    Ok(profiles) => profiles,
                    Err(error) => {
                        return SettingsTaskResult::Stored(Err(error.to_string()));
                    }
                };
                let Some(profile) = profiles.profiles.get(profile_id) else {
                    return SettingsTaskResult::Stored(Err(SettingsError::ModelProfileNotFound(
                        profile_id.clone(),
                    )
                    .to_string()));
                };
                let schema = match runtime_packs
                    .model_settings_schema_for_engine(model, profile.engine_id.as_str())
                {
                    Ok(schema) => schema,
                    Err(error) => {
                        return SettingsTaskResult::Stored(Err(error.to_string()));
                    }
                };
                let mut candidate = profile.overrides.clone();
                candidate.0.extend(patch.0.clone());
                if let Err(error) = schema.validate(&resolved_patch(
                    profile.engine_id.as_str(),
                    &profile.id,
                    &candidate,
                )) {
                    return SettingsTaskResult::Stored(Err(error.to_string()));
                }
            }
            let result = match scope {
                SettingsScope::Global | SettingsScope::Engine(_) => settings_store
                    .update(move |state| {
                        match scope {
                            SettingsScope::Global => {
                                if let Some(id) = patch.0.keys().find(|id| id.namespace().is_some())
                                {
                                    return Err(SettingsError::InvalidGlobalSetting(id.clone()));
                                }
                                state.global_defaults.0.extend(patch.0);
                            }
                            SettingsScope::Engine(engine_id) => {
                                if let Some(id) =
                                    patch.0.keys().find(|id| !id.applies_to_engine(&engine_id))
                                {
                                    return Err(SettingsError::WrongEngineScope {
                                        setting_id: id.clone(),
                                        engine_id,
                                    });
                                }
                                state
                                    .engine_defaults
                                    .entry(engine_id)
                                    .or_default()
                                    .0
                                    .extend(patch.0);
                            }
                            SettingsScope::ModelProfile(_) => unreachable!(),
                        }
                        Ok(())
                    })
                    .await
                    .map_err(|error| error.to_string()),
                SettingsScope::ModelProfile(profile_id) => profiles_store
                    .update(move |state| {
                        let profile = state.profiles.get_mut(&profile_id).ok_or_else(|| {
                            SettingsError::ModelProfileNotFound(profile_id.clone())
                        })?;
                        for id in patch.0.keys() {
                            if !id.applies_to_engine(profile.engine_id.as_str()) {
                                return Err(SettingsError::WrongEngineScope {
                                    setting_id: id.clone(),
                                    engine_id: profile.engine_id.to_string(),
                                });
                            }
                        }
                        profile.overrides.0.extend(patch.0);
                        profile.validate()?;
                        Ok(())
                    })
                    .await
                    .map_err(|error| error.to_string()),
            };
            SettingsTaskResult::Stored(match result {
                Ok(()) => read_tui_settings(&settings_store, &profiles_store).await,
                Err(error) => Err(error),
            })
        }
        SettingsAction::Unset { scope, id } => {
            let mut ids = vec![id];
            if ids[0].as_str() == "q27.template_path" {
                ids.push(SettingId::new("q27.template_sha256").expect("static setting ID"));
            }
            for (path, sha256) in [
                (
                    "llama.cpp.chat_template_file",
                    "llama.cpp.chat_template_sha256",
                ),
                (
                    "llama.cpp.speculative_draft_model",
                    "llama.cpp.speculative_draft_sha256",
                ),
            ] {
                if ids[0].as_str() == path {
                    ids.push(SettingId::new(sha256).expect("static setting ID"));
                }
            }
            let result = match scope {
                SettingsScope::Global | SettingsScope::Engine(_) => settings_store
                    .update(move |state| {
                        match scope {
                            SettingsScope::Global => {
                                for id in &ids {
                                    state.global_defaults.remove(id);
                                }
                            }
                            SettingsScope::Engine(engine_id) => {
                                if let Some(patch) = state.engine_defaults.get_mut(&engine_id) {
                                    for id in &ids {
                                        patch.remove(id);
                                    }
                                    if patch.is_empty() {
                                        state.engine_defaults.remove(&engine_id);
                                    }
                                }
                            }
                            SettingsScope::ModelProfile(_) => unreachable!(),
                        }
                        Ok(())
                    })
                    .await
                    .map_err(|error| error.to_string()),
                SettingsScope::ModelProfile(profile_id) => profiles_store
                    .update(move |state| {
                        let profile = state.profiles.get_mut(&profile_id).ok_or_else(|| {
                            SettingsError::ModelProfileNotFound(profile_id.clone())
                        })?;
                        for id in &ids {
                            profile.overrides.remove(id);
                        }
                        Ok(())
                    })
                    .await
                    .map_err(|error| error.to_string()),
            };
            SettingsTaskResult::Stored(match result {
                Ok(()) => read_tui_settings(&settings_store, &profiles_store).await,
                Err(error) => Err(error),
            })
        }
        SettingsAction::CreateProfile {
            id,
            display_name,
            model,
            engine_id,
        } => {
            let engines = match runtime_packs
                .compatible_engine_ids(&model)
                .into_iter()
                .map(EngineId::new)
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(engines) => engines,
                Err(error) => return SettingsTaskResult::Stored(Err(error.to_string())),
            };
            let engine_id = match (engine_id, engines.as_slice()) {
                (_, []) => {
                    return SettingsTaskResult::Stored(Err(format!(
                        "No registered engine is compatible with model `{}`",
                        model.id
                    )));
                }
                (Some(engine_id), _) if engines.contains(&engine_id) => engine_id,
                (Some(engine_id), _) => {
                    return SettingsTaskResult::Stored(Err(format!(
                        "Engine `{engine_id}` is not compatible with model `{}`",
                        model.id
                    )));
                }
                (None, [engine_id]) => engine_id.clone(),
                (None, _) => {
                    return SettingsTaskResult::ChooseProfileEngine(ProfileEngineSelection {
                        id,
                        display_name,
                        model,
                        engines,
                        selected: 0,
                    });
                }
            };
            let result = profiles_store
                .update(move |state| state.create(id, display_name, model.id, engine_id))
                .await
                .map_err(|error| error.to_string());
            SettingsTaskResult::Stored(match result {
                Ok(()) => read_tui_settings(&settings_store, &profiles_store).await,
                Err(error) => Err(error),
            })
        }
        SettingsAction::DuplicateProfile {
            source,
            destination,
        } => {
            let display_name = destination.to_string();
            let result = profiles_store
                .update(move |state| state.duplicate(&source, destination, display_name))
                .await
                .map_err(|error| error.to_string());
            SettingsTaskResult::Stored(match result {
                Ok(()) => read_tui_settings(&settings_store, &profiles_store).await,
                Err(error) => Err(error),
            })
        }
        SettingsAction::DeleteProfile(profile_id) => {
            let active = match ControlClient::discover(paths).await {
                Ok(client) => client
                    .status()
                    .await
                    .ok()
                    .and_then(|status| status.backend.model_profile_id),
                Err(_) => None,
            };
            let result = if active.as_ref() == Some(&profile_id) {
                Err(format!(
                    "Active Model Profile `{profile_id}` cannot be deleted; unload it first"
                ))
            } else {
                profiles_store
                    .update(move |state| state.delete(&profile_id).map(|_| ()))
                    .await
                    .map_err(|error| error.to_string())
            };
            SettingsTaskResult::Stored(match result {
                Ok(()) => read_tui_settings(&settings_store, &profiles_store).await,
                Err(error) => Err(error),
            })
        }
        SettingsAction::SetProfileModel { profile_id, model } => {
            let compatible = runtime_packs.compatible_engine_ids(&model);
            let current = match profiles_store.read().await {
                Ok(profiles) => profiles,
                Err(error) => return SettingsTaskResult::Stored(Err(error.to_string())),
            };
            let Some(existing) = current.profiles.get(&profile_id) else {
                return SettingsTaskResult::Stored(Err(SettingsError::ModelProfileNotFound(
                    profile_id,
                )
                .to_string()));
            };
            let schema = match runtime_packs
                .model_settings_schema_for_engine(&model, existing.engine_id.as_str())
            {
                Ok(schema) => schema,
                Err(error) => return SettingsTaskResult::Stored(Err(error.to_string())),
            };
            if let Err(error) = schema.validate(&resolved_patch(
                existing.engine_id.as_str(),
                &existing.id,
                &existing.overrides,
            )) {
                return SettingsTaskResult::Stored(Err(error.to_string()));
            }
            let result = profiles_store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&profile_id)
                        .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?;
                    if !compatible
                        .iter()
                        .any(|engine| engine == profile.engine_id.as_str())
                    {
                        return Err(SettingsError::InvalidModelProfile(format!(
                            "engine `{}` is incompatible with model `{}`",
                            profile.engine_id, model.id
                        )));
                    }
                    profile.model_id = model.id;
                    Ok(())
                })
                .await
                .map_err(|error| error.to_string());
            SettingsTaskResult::Stored(match result {
                Ok(()) => read_tui_settings(&settings_store, &profiles_store).await,
                Err(error) => Err(error),
            })
        }
        SettingsAction::CycleProfileEngine { profile_id, model } => {
            let compatible = runtime_packs.compatible_engine_ids(&model);
            let result = profiles_store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&profile_id)
                        .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?;
                    let next = compatible
                        .iter()
                        .position(|engine| engine == profile.engine_id.as_str())
                        .map(|index| (index + 1) % compatible.len())
                        .unwrap_or(0);
                    let engine = compatible.get(next).ok_or_else(|| {
                        SettingsError::InvalidModelProfile(format!(
                            "no registered engine is compatible with model `{}`",
                            model.id
                        ))
                    })?;
                    profile.engine_id = EngineId::new(engine.clone())?;
                    profile
                        .overrides
                        .0
                        .retain(|id, _| id.applies_to_engine(profile.engine_id.as_str()));
                    Ok(())
                })
                .await
                .map_err(|error| error.to_string());
            SettingsTaskResult::Stored(match result {
                Ok(()) => read_tui_settings(&settings_store, &profiles_store).await,
                Err(error) => Err(error),
            })
        }
        SettingsAction::InspectProfile { profile, model } => {
            let model_id = model.id.clone();
            let result = async {
                let (state, profiles) = read_tui_settings(&settings_store, &profiles_store).await?;
                let resolved = state
                    .resolve(
                        &profile.id,
                        profile.engine_id.as_str(),
                        &profile.overrides,
                        &SettingsPatch::default(),
                        &paths.data_dir,
                    )
                    .map_err(|error| error.to_string())?;
                let model_schema = runtime_packs
                    .model_settings_schema_for_engine(&model, profile.engine_id.as_str())
                    .map_err(|error| error.to_string())?;
                let model_validation_error = model_schema
                    .validate(&resolved)
                    .err()
                    .map(|error| error.to_string());
                let (runtime_id, schema, validation_error) = if let Some(error) =
                    model_validation_error
                {
                    (
                        None,
                        model_schema,
                        Some(format!(
                            "Model Profile `{}` contradicts capabilities proved by bound model `{}`: {error}",
                            profile.id, model.id
                        )),
                    )
                } else {
                    runtime_packs.refresh_host_capabilities().await;
                    match runtime_packs
                        .settings_schema_for_model_for_engine_with_settings(
                            &model,
                            profile.engine_id.as_str(),
                            None,
                            Some(&resolved),
                        )
                        .await
                    {
                        Ok((selection, schema)) => {
                            let validation_error = schema
                                .validate(&resolved)
                                .err()
                                .map(|error| error.to_string());
                            (
                                Some(selection.runtime.manifest.runtime_id),
                                schema,
                                validation_error,
                            )
                        }
                        Err(error) => (
                            None,
                            model_schema,
                            Some(format!(
                                "No installed runtime is compatible with Model Profile `{}` on bound engine `{}`: {error}",
                                profile.id, profile.engine_id
                            )),
                        ),
                    }
                };
                Ok(ModelSettingsInspection {
                    state,
                    profiles,
                    runtime_id,
                    schema,
                    resolved,
                    validation_error,
                })
            }
            .await;
            SettingsTaskResult::Inspected {
                model_id,
                result: Box::new(result),
            }
        }
    }
}

fn spawn_runtime_action(
    runtime_packs: Arc<RuntimePackManager>,
    paths: AppPaths,
    results: tokio::sync::mpsc::Sender<RuntimeTaskResult>,
    action: RuntimeAction,
) {
    tokio::spawn(async move {
        let result = execute_runtime_action(runtime_packs, &paths, action).await;
        let _ = results.send(result).await;
    });
}

async fn execute_runtime_action(
    runtime_packs: Arc<RuntimePackManager>,
    paths: &AppPaths,
    action: RuntimeAction,
) -> RuntimeTaskResult {
    match action {
        RuntimeAction::RefreshList => {
            runtime_packs.refresh_host_capabilities().await;
            RuntimeTaskResult::Listed(
                runtime_packs
                    .list()
                    .await
                    .map_err(|error| error.to_string()),
            )
        }
        RuntimeAction::Search {
            query,
            force_refresh,
            model,
            settings,
        } => {
            runtime_packs.refresh_host_capabilities().await;
            let result = match model {
                Some(model) => {
                    runtime_packs
                        .search_for_model_with_settings(
                            &query,
                            &model,
                            force_refresh,
                            settings.as_deref(),
                        )
                        .await
                }
                None => runtime_packs.search(&query, force_refresh).await,
            };
            RuntimeTaskResult::Searched(result.map_err(|error| error.to_string()))
        }
        RuntimeAction::Install(runtime_id) => {
            let result = match runtime_packs.install(&runtime_id).await {
                Ok(_) => runtime_packs
                    .list()
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            RuntimeTaskResult::Installed { runtime_id, result }
        }
        RuntimeAction::CheckUpdates => RuntimeTaskResult::Updates(
            runtime_packs
                .check_updates()
                .await
                .map_err(|error| error.to_string()),
        ),
        RuntimeAction::Update(runtime_id) => {
            let result = match runtime_packs.update(&runtime_id).await {
                Ok(installed) if installed.manifest.runtime_id != runtime_id => {
                    let installed_runtime_id = installed.manifest.runtime_id;
                    runtime_packs
                        .list()
                        .await
                        .map(|snapshot| (installed_runtime_id, snapshot))
                        .map_err(|error| error.to_string())
                }
                Ok(_) => Err("no newer compatible runtime is currently available".to_owned()),
                Err(error) => Err(error.to_string()),
            };
            RuntimeTaskResult::Updated {
                previous_runtime_id: runtime_id,
                result,
            }
        }
        RuntimeAction::Remove { runtime_id } => {
            let active_runtime = match ControlClient::discover(paths).await {
                Ok(client) => match client.status().await {
                    Ok(status) => status.backend.runtime_id,
                    Err(error) => {
                        return RuntimeTaskResult::Removed {
                            runtime_id,
                            result: Err(error.to_string()),
                        };
                    }
                },
                Err(ControlClientError::Unavailable) => None,
                Err(error) => {
                    return RuntimeTaskResult::Removed {
                        runtime_id,
                        result: Err(error.to_string()),
                    };
                }
            };
            let result = match runtime_packs
                .remove(&runtime_id, active_runtime.as_ref())
                .await
            {
                Ok(()) => runtime_packs
                    .list()
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            RuntimeTaskResult::Removed { runtime_id, result }
        }
        RuntimeAction::ModelCandidates { model, settings } => {
            let model_id = model.id.clone();
            RuntimeTaskResult::ModelCandidates {
                model_id,
                result: runtime_packs
                    .compatible_installed_for_model_with_settings(&model, settings.as_deref())
                    .await
                    .map_err(|error| error.to_string()),
            }
        }
        RuntimeAction::SelectFormat { format, runtime_id } => {
            let result = match runtime_packs.select_format(format, runtime_id).await {
                Ok(_) => runtime_packs
                    .list()
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            RuntimeTaskResult::Selected { format, result }
        }
        RuntimeAction::SelectModel { model, runtime_id } => {
            let model_id = model.id.clone();
            let result = match runtime_packs.select_model(&model, runtime_id).await {
                Ok(_) => runtime_packs
                    .list()
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            RuntimeTaskResult::ModelSelected { model_id, result }
        }
        RuntimeAction::ClearModelSelection { model_id } => {
            let result = match runtime_packs.clear_model_selection(&model_id).await {
                Ok(_) => runtime_packs
                    .list()
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            RuntimeTaskResult::ModelSelectionCleared { model_id, result }
        }
    }
}

struct ControlObservation {
    status: Option<ControlStatus>,
    error: Option<String>,
}

async fn observe_control(paths: &AppPaths) -> ControlObservation {
    match ControlClient::discover(paths).await {
        Ok(client) => match client.status().await {
            Ok(status) => ControlObservation {
                status: Some(status),
                error: None,
            },
            Err(error) => ControlObservation {
                status: None,
                error: Some(error.to_string()),
            },
        },
        Err(error) => ControlObservation {
            status: None,
            error: Some(error.to_string()),
        },
    }
}

async fn execute_control(
    paths: &AppPaths,
    action: ControlAction,
) -> std::result::Result<ControlStatus, String> {
    let client = ControlClient::discover(paths)
        .await
        .map_err(|error| error.to_string())?;
    match action {
        ControlAction::Load(profile_id) => client.start_load(profile_id).await,
        ControlAction::Unload => client.unload().await,
    }
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_polling_starts_with_an_immediate_observation() {
        let mut state = ControlPollState::new();
        assert_eq!(state.next_delay(), None);
        state.record_observation(Some((0, BackendLifecycle::Stopped)));
        assert_eq!(state.next_delay(), Some(CONTROL_IDLE_CADENCE));
    }

    #[test]
    fn local_load_intent_immediately_enters_and_holds_fast_polling() {
        let mut state = ControlPollState::new();
        let _ = state.next_delay();
        state.set_local_load_intent(LocalLoadIntent::PendingAdmission);
        assert_eq!(state.cadence(), CONTROL_LOADING_CADENCE);

        state.record_observation(Some((4, BackendLifecycle::Stopped)));
        assert_eq!(
            state.cadence(),
            CONTROL_LOADING_CADENCE,
            "an early Stopped observation must not erase pending local load intent"
        );

        state.set_local_load_intent(LocalLoadIntent::Accepted { generation: 5 });
        state.record_observation(Some((4, BackendLifecycle::Stopped)));
        assert_eq!(
            state.cadence(),
            CONTROL_LOADING_CADENCE,
            "a stale Stopped observation must not clear an accepted admission"
        );

        state.record_observation(Some((5, BackendLifecycle::Loading)));
        assert_eq!(state.cadence(), CONTROL_LOADING_CADENCE);

        state.record_observation(Some((5, BackendLifecycle::Running)));
        assert_eq!(state.cadence(), CONTROL_IDLE_CADENCE);
    }

    #[test]
    fn external_loading_observation_uses_fast_polling_until_completion() {
        let mut state = ControlPollState::new();
        let _ = state.next_delay();
        state.record_observation(Some((9, BackendLifecycle::Loading)));
        assert_eq!(state.cadence(), CONTROL_LOADING_CADENCE);

        state.record_observation(Some((9, BackendLifecycle::Failed)));
        assert_eq!(state.cadence(), CONTROL_IDLE_CADENCE);
        assert_eq!(state.next_delay(), Some(CONTROL_IDLE_CADENCE));
    }
}
