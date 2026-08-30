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
    ApiKeyStore, AppPaths, ApplicationCore, LoadProfilesStore, LoadSettingDefinition,
    LoadSettingsError, LoadSettingsPatch, PublicAuthStatus, ServerConfig,
};
use norted_engine::{ControlClient, ControlClientError, ControlStatus, RuntimePackManager};

use app::{
    App, ControlAction, ModelSettingsInspection, RuntimeAction, RuntimeTaskResult, SettingsAction,
    SettingsScope, SettingsTaskResult, Update,
};
use terminal::TerminalSession;
use ui::layout::UiLayout;

pub async fn run(
    core: Arc<ApplicationCore>,
    runtime_packs: Arc<RuntimePackManager>,
    load_setting_definitions: Vec<LoadSettingDefinition>,
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
        load_setting_definitions,
    );
    let mut layout = UiLayout::default();
    terminal.draw(|frame| layout = ui::render(frame, &app))?;

    core.start_model_discovery().await;
    let mut terminal_events = EventStream::new();
    let (control_updates, mut control_update_receiver) = tokio::sync::mpsc::channel(2);
    let (control_results, mut control_result_receiver) = tokio::sync::mpsc::channel(2);
    let (runtime_results, mut runtime_result_receiver) = tokio::sync::mpsc::channel(4);
    let (settings_results, mut settings_result_receiver) = tokio::sync::mpsc::channel(4);
    let (auth_updates, mut auth_update_receiver) = tokio::sync::mpsc::channel(2);
    let mut runtime_progress = runtime_packs.progress();
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
    let control_cadence = Arc::new(tokio::sync::Mutex::new(Duration::from_secs(2)));
    let runtime_observer = tokio::spawn({
        let cadence = Arc::clone(&control_cadence);
        async move {
            loop {
                let delay = *cadence.lock().await;
                tokio::time::sleep(delay).await;
                let (_, observation) = tokio::join!(
                    observer_core.refresh_server_state(),
                    observe_control(&observer_paths)
                );
                let loading = observation
                    .status
                    .as_ref()
                    .is_some_and(|status| status.backend.lifecycle.is_loading());
                *cadence.lock().await = if loading {
                    Duration::from_millis(200)
                } else {
                    Duration::from_secs(2)
                };
                if control_updates.send(observation).await.is_err() {
                    break;
                }
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

async fn execute_settings_action(
    runtime_packs: Arc<RuntimePackManager>,
    paths: &AppPaths,
    action: SettingsAction,
) -> SettingsTaskResult {
    let store = LoadProfilesStore::new(paths);
    match action {
        SettingsAction::Refresh => {
            SettingsTaskResult::Loaded(store.read().await.map_err(|error| error.to_string()))
        }
        SettingsAction::Set { scope, id, value } => {
            let result = store
                .update(move |state| {
                    match scope {
                        SettingsScope::Global => {
                            if id.namespace().is_some() {
                                return Err(LoadSettingsError::InvalidGlobalSetting(id));
                            }
                            state.global_defaults.insert(id, value);
                        }
                        SettingsScope::Engine(engine_id) => {
                            if !id.applies_to_engine(&engine_id) {
                                return Err(LoadSettingsError::WrongEngineScope {
                                    setting_id: id,
                                    engine_id,
                                });
                            }
                            state
                                .engine_defaults
                                .entry(engine_id)
                                .or_default()
                                .insert(id, value);
                        }
                        SettingsScope::Profile(profile) => {
                            state
                                .profiles
                                .get_mut(&profile)
                                .ok_or_else(|| LoadSettingsError::ProfileNotFound(profile.clone()))?
                                .settings
                                .insert(id, value);
                        }
                        SettingsScope::Model(model) => {
                            state
                                .model_defaults
                                .entry(model)
                                .or_default()
                                .insert(id, value);
                        }
                    }
                    Ok(state.clone())
                })
                .await
                .map_err(|error| error.to_string());
            SettingsTaskResult::Stored(result)
        }
        SettingsAction::Unset { scope, id } => {
            let result = store
                .update(move |state| {
                    let cleanup_scope = scope.clone();
                    let patch = match scope {
                        SettingsScope::Global => &mut state.global_defaults,
                        SettingsScope::Engine(engine) => {
                            state.engine_defaults.entry(engine).or_default()
                        }
                        SettingsScope::Profile(profile) => {
                            &mut state
                                .profiles
                                .get_mut(&profile)
                                .ok_or_else(|| LoadSettingsError::ProfileNotFound(profile.clone()))?
                                .settings
                        }
                        SettingsScope::Model(model) => {
                            state.model_defaults.entry(model).or_default()
                        }
                    };
                    patch.remove(&id);
                    if patch.is_empty() {
                        match cleanup_scope {
                            SettingsScope::Engine(engine) => {
                                state.engine_defaults.remove(&engine);
                            }
                            SettingsScope::Model(model) => {
                                state.model_defaults.remove(&model);
                            }
                            SettingsScope::Global | SettingsScope::Profile(_) => {}
                        }
                    }
                    Ok(state.clone())
                })
                .await
                .map_err(|error| error.to_string());
            SettingsTaskResult::Stored(result)
        }
        SettingsAction::CreateProfile(profile) => {
            let result = store
                .update(move |state| {
                    state.create_profile(profile)?;
                    Ok(state.clone())
                })
                .await
                .map_err(|error| error.to_string());
            SettingsTaskResult::Stored(result)
        }
        SettingsAction::DeleteProfile(profile) => {
            let result = store
                .update(move |state| {
                    state.delete_profile(&profile)?;
                    Ok(state.clone())
                })
                .await
                .map_err(|error| error.to_string());
            SettingsTaskResult::Stored(result)
        }
        SettingsAction::AssignProfile { model_id, profile } => {
            let result = store
                .update(move |state| {
                    state.assign_profile(model_id, profile)?;
                    Ok(state.clone())
                })
                .await
                .map_err(|error| error.to_string());
            SettingsTaskResult::Stored(result)
        }
        SettingsAction::InspectModel(model) => {
            let model_id = model.id.clone();
            let result = async {
                let profiles = store.read().await.map_err(|error| error.to_string())?;
                runtime_packs.refresh_host_capabilities().await;
                let (selection, schema) = runtime_packs
                    .load_settings_schema_for_model(&model, None)
                    .await
                    .map_err(|error| format!(
                        "No compatible installed runtime is available to validate these settings. Open Runtimes search to install one: {error}"
                    ))?;
                let engine_id = &selection.runtime.manifest.identity.engine_id;
                let resolved = profiles
                    .resolve(
                        &model.id,
                        engine_id,
                        None,
                        &LoadSettingsPatch::default(),
                        &paths.data_dir,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(ModelSettingsInspection {
                    profiles,
                    runtime_id: selection.runtime.manifest.runtime_id,
                    schema,
                    resolved,
                })
            }
            .await;
            SettingsTaskResult::Inspected { model_id, result }
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
        } => {
            runtime_packs.refresh_host_capabilities().await;
            let result = match model {
                Some(model) => {
                    runtime_packs
                        .search_for_model(&query, &model, force_refresh)
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
        RuntimeAction::ModelCandidates { model } => {
            let model_id = model.id.clone();
            RuntimeTaskResult::ModelCandidates {
                model_id,
                result: runtime_packs
                    .compatible_installed_for_model(&model)
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
        ControlAction::Load(model_id) => client.load(model_id).await,
        ControlAction::Unload => client.unload().await,
    }
    .map_err(|error| error.to_string())
}
