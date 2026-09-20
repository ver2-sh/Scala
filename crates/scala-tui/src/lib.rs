//! Interactive terminal interface for Scala.

mod app;
mod benchmarks;
mod commands;
mod settings_editor;
mod terminal;
mod theme;
mod ui;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use scala_core::{
    ApiKeyStore, AppPaths, ApplicationCore, EngineId, ModelProfilesStore, PublicAuthStatus,
    ServerConfig, SettingDefinition, SettingId, SettingsError, SettingsPatch, SettingsStore,
};
use scala_engine::{
    BackendLifecycle, ControlClient, ControlClientError, ControlStatus, RuntimePackManager,
};
use scala_model_library::{ModelDownloadJobId, ModelLibrary, ModelOperationPhase};

use app::{
    App, ControlAction, ModelLibraryAction, ModelLibraryTaskResult, ModelSettingsInspection,
    ProfileEngineSelection, RuntimeAction, RuntimeTaskResult, SettingsAction, SettingsLoad,
    SettingsScope, SettingsTaskResult, Update,
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
    observed_activity: bool,
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

    fn record_activity(&mut self, active: bool) {
        self.observed_activity = active;
    }

    fn cadence(&self) -> Duration {
        if self.local_load_intent != LocalLoadIntent::Idle
            || self.observed_loading
            || self.observed_activity
        {
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
    if let Ok(settings) = SettingsStore::new(&core.paths).read().await {
        model_library.set_max_parallel_downloads(
            scala_model_library::max_parallel_downloads_from_settings(&settings),
        );
    }
    let mut terminal = TerminalSession::enter()?;
    // JoinSet aborts network work on every exit/error path. No updater writes to
    // the terminal, and checks do not share inference/control task queues.
    let mut app_checks = tokio::task::JoinSet::new();
    let cache = core.paths.cache_dir.clone();
    app_checks.spawn(async move { scala_update::check(&cache, false).await });
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
    terminal.draw(|frame| layout = ui::render(frame, &mut app))?;

    core.start_model_discovery().await;
    let mut terminal_events = EventStream::new();
    app.link_config = core.config.link.clone();
    let (link_results, mut link_result_receiver) = tokio::sync::mpsc::channel(2);
    let (control_updates, mut control_update_receiver) = tokio::sync::mpsc::channel(2);
    let (control_results, mut control_result_receiver) =
        tokio::sync::mpsc::channel::<std::result::Result<ControlStatus, String>>(2);
    let (runtime_results, mut runtime_result_receiver) = tokio::sync::mpsc::channel(4);
    let (model_library_results, mut model_library_result_receiver) = tokio::sync::mpsc::channel(4);
    let (settings_results, mut settings_result_receiver) = tokio::sync::mpsc::channel(4);
    let (auth_updates, mut auth_update_receiver) = tokio::sync::mpsc::channel(2);
    let (benchmark_results, mut benchmark_receiver) = tokio::sync::mpsc::channel(2);
    let mut benchmark_tick = tokio::time::interval(Duration::from_secs(2));
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
                    .and_then(|status| match poll_state.local_load_intent {
                        LocalLoadIntent::Accepted { generation } => status
                            .backends
                            .iter()
                            .find(|backend| backend.generation == generation)
                            .or_else(|| status.loading_backend()),
                        LocalLoadIntent::Idle | LocalLoadIntent::PendingAdmission => {
                            status.loading_backend()
                        }
                    })
                    .map(|backend| (backend.generation, backend.lifecycle)),
            );
            poll_state.record_activity(observation.status.as_ref().is_some_and(|status| {
                status.backends.iter().any(|backend| {
                    backend.active_request_count > 0 || !backend.activities.is_empty()
                })
            }));
            if control_updates.send(observation).await.is_err() {
                break;
            }
        }
    });
    let mut render = false;
    let mut refreshed_installed_jobs = HashSet::new();
    let mut render_tick = tokio::time::interval(Duration::from_millis(150));
    render_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if render {
            terminal.draw(|frame| layout = ui::render(frame, &mut app))?;
        }
        let update = tokio::select! {
            result = app_checks.join_next(), if !app_checks.is_empty() => {
                let state = match result {
                    Some(Ok(Ok(state))) => state,
                    _ => scala_update::State {
                        checked: 0, latest: None,
                        error: Some("check failed; /update retries. Serving is unaffected".into()),
                    },
                };
                app.notice = Some(state.message());
                app.app_update = Some(state);
                Update::Render
            },
            event = terminal_events.next() => match event {
                Some(Ok(Event::Key(key))) => app.handle_key(key, &layout),
                Some(Ok(Event::Mouse(mouse))) => app.handle_mouse(mouse, &layout),
                Some(Ok(Event::Paste(text))) => app.handle_paste(&text),
                Some(Ok(Event::Resize(_, _))) => {
                    app.clear_hover();
                    if matches!(app.screen, app::Screen::Settings | app::Screen::ModelProfiles) {
                        app.settings_scroll = app.settings_setting_index;
                    }
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
                    app.replace_link(observation.link);
                    app.replace_control(observation.status, observation.error);
                    Update::Render
                }
                None => Update::None,
            },
            result = link_result_receiver.recv() => {
                if let Some(result) = result { app.handle_link_result(result); }
                Update::Render
            },
            result = control_result_receiver.recv() => match result {
                Some(result) => {
                    let intent = match &result {
                        Ok(status) if status.loading_backend().is_some() => {
                            LocalLoadIntent::Accepted {
                                generation: status.loading_backend().expect("checked above").generation,
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
            _ = benchmark_tick.tick() => {
                if app.screen == app::Screen::Benchmarks && !app.benchmarks.busy && app.benchmarks.pending.is_none() {
                    app.benchmarks.pending = Some(scala_engine::benchmark::BenchmarkRequest::Status);
                }
                Update::None
            },
            result = benchmark_receiver.recv() => {
                if let Some((request, result)) = result {
                    let identities = |state: &crate::benchmarks::Benchmarks| state.rows().iter()
                        .map(|row| (row["profile_id"].clone(), row["run_id"].clone())).collect::<Vec<_>>();
                    let before = identities(&app.benchmarks);
                    app.benchmarks.accept(&request, result);
                    if !matches!(request, scala_engine::benchmark::BenchmarkRequest::Status)
                        || before != identities(&app.benchmarks) {
                        app.clear_hover();
                    }
                }
                Update::Render
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
                    if let Some(settings) = &app.settings_state {
                        model_library.set_max_parallel_downloads(
                            scala_model_library::max_parallel_downloads_from_settings(settings),
                        );
                    }
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
                Ok(_) => {
                    reconcile_model_download_jobs(
                        &mut app,
                        &model_library,
                        &core,
                        &model_library_results,
                        &mut refreshed_installed_jobs,
                    );
                    Update::Render
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    reconcile_model_download_jobs(
                        &mut app,
                        &model_library,
                        &core,
                        &model_library_results,
                        &mut refreshed_installed_jobs,
                    );
                    Update::Render
                },
                Err(tokio::sync::broadcast::error::RecvError::Closed) => Update::None,
            },
            _ = render_tick.tick() => {
                let (has_live_downloads, download_jobs_changed) = reconcile_model_download_jobs(
                    &mut app,
                    &model_library,
                    &core,
                    &model_library_results,
                    &mut refreshed_installed_jobs,
                );
                let animation_changed =
                    app.advance_ui_animation(layout.active_marquee_target(&app));
                if has_live_downloads || download_jobs_changed || animation_changed {
                    Update::Render
                } else {
                    Update::None
                }
            }
        };
        if update == Update::Quit {
            break;
        }
        if app.app_update_pending && app_checks.is_empty() {
            app.app_update_pending = false;
            let cache = core.paths.cache_dir.clone();
            app_checks.spawn(async move { scala_update::check(&cache, true).await });
        }
        if !app.benchmarks.busy {
            if let Some(request) = app.benchmarks.pending.take() {
                app.benchmarks.busy = true;
                app.benchmarks.polling =
                    matches!(request, scala_engine::benchmark::BenchmarkRequest::Status);
                let paths = core.paths.clone();
                let results = benchmark_results.clone();
                tokio::spawn(async move {
                    let result = match ControlClient::discover(&paths).await {
                        Ok(client) => client
                            .benchmark(request.clone())
                            .await
                            .map_err(|e| e.to_string()),
                        Err(error) => Err(error.to_string()),
                    };
                    let _ = results.send((request, result)).await;
                });
            }
        }
        if let Some(config) = app.pending_link_config.take() {
            match config.save(&core.paths) {
                Ok(()) => {
                    app.link_config = config;
                    app.notice = Some("Link configuration saved. Restart Scala to apply.".into());
                }
                Err(error) => app.notice = Some(error),
            }
        }
        if let Some(action) = app.take_link_action() {
            let paths = core.paths.clone();
            let results = link_results.clone();
            tokio::spawn(async move {
                let result = async {
                    let client = ControlClient::discover(&paths)
                        .await
                        .map_err(|e| e.to_string())?;
                    client
                        .link_control(action)
                        .await
                        .map_err(|e| e.to_string())?;
                    client.link_status().await.map_err(|e| e.to_string())
                }
                .await;
                let _ = results.send(result).await;
            });
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

fn reconcile_model_download_jobs(
    app: &mut App,
    library: &ModelLibrary,
    core: &Arc<ApplicationCore>,
    results: &tokio::sync::mpsc::Sender<ModelLibraryTaskResult>,
    refreshed_installed_jobs: &mut HashSet<ModelDownloadJobId>,
) -> (bool, bool) {
    let jobs = library.download_jobs();
    let jobs_changed = download_job_snapshots_changed(&app.model_download_jobs, &jobs);
    let previous_selection = app.selected_model_download_job.clone();
    let current_ids = jobs
        .iter()
        .map(|job| job.id.clone())
        .collect::<HashSet<_>>();
    refreshed_installed_jobs.retain(|job_id| current_ids.contains(job_id));
    let installed = jobs
        .iter()
        .filter(|job| job.phase == ModelOperationPhase::Installed)
        .map(|job| job.id.clone())
        .filter(|job_id| !refreshed_installed_jobs.contains(job_id))
        .collect::<Vec<_>>();
    if !installed.is_empty() {
        refreshed_installed_jobs.extend(installed);
        let core = Arc::clone(core);
        let results = results.clone();
        tokio::spawn(async move {
            let result = core
                .refresh_models()
                .await
                .map_err(|error| error.to_string());
            let _ = results
                .send(ModelLibraryTaskResult::ModelsRefreshed(result))
                .await;
        });
    }
    let has_live_downloads = jobs.iter().any(|job| {
        matches!(
            job.phase,
            ModelOperationPhase::Resolving
                | ModelOperationPhase::Downloading
                | ModelOperationPhase::Verifying
                | ModelOperationPhase::Validating
                | ModelOperationPhase::Installing
        )
    });
    app.replace_model_download_jobs(jobs);
    (
        has_live_downloads,
        jobs_changed || app.selected_model_download_job != previous_selection,
    )
}

fn download_job_snapshots_changed(
    previous: &[scala_model_library::ModelDownloadJob],
    current: &[scala_model_library::ModelDownloadJob],
) -> bool {
    previous.len() != current.len()
        || previous.iter().zip(current).any(|(previous, current)| {
            previous.id != current.id
                || previous.model_ref != current.model_ref
                || previous.provider != current.provider
                || previous.repository != current.repository
                || previous.filename != current.filename
                || previous.phase != current.phase
                || previous.downloaded_bytes != current.downloaded_bytes
                || previous.total_bytes != current.total_bytes
                || previous.progress_percent != current.progress_percent
                || previous.transfer_bytes_per_second != current.transfer_bytes_per_second
                || previous.estimated_remaining != current.estimated_remaining
                || previous.queue_position != current.queue_position
                || previous.message != current.message
        })
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
            ModelLibraryAction::Download(model_ref) => ModelLibraryTaskResult::DownloadAdmitted(
                Box::new(library.enqueue_download(model_ref)),
            ),
            ModelLibraryAction::DownloadJob { id, action } => {
                let result = match action {
                    crate::ui::layout::DownloadJobAction::Pause => library.pause_download(&id),
                    crate::ui::layout::DownloadJobAction::Resume => library.resume_download(&id),
                    crate::ui::layout::DownloadJobAction::Cancel => library.cancel_download(&id),
                };
                ModelLibraryTaskResult::DownloadJobControlled {
                    action,
                    result: Box::new(result),
                }
            }
            ModelLibraryAction::Remove(model_id) => {
                let result = match core.model(&model_id).await {
                    Some(model) => match library.plan_removal(&model) {
                        Ok(plan) => {
                            let active = match ControlClient::discover(&core.paths).await {
                                Ok(client) => client
                                    .status()
                                    .await
                                    .map(|status| {
                                        status
                                            .backends
                                            .into_iter()
                                            .map(|backend| backend.model_id)
                                            .collect::<Vec<_>>()
                                    })
                                    .map_err(|error| error.to_string()),
                                Err(ControlClientError::Unavailable) => Ok(Vec::new()),
                                Err(error) => Err(error.to_string()),
                            };
                            match active {
                                Ok(active)
                                    if active
                                        .iter()
                                        .any(|model| plan.affected_model_ids.contains(model)) =>
                                {
                                    Err("An active model belongs to this managed acquisition; unload it before removal".to_owned())
                                }
                                Ok(_) => match library.remove(&plan).await {
                                    Ok(()) => match core.refresh_models().await {
                                        Ok(()) => Ok(model_id),
                                        Err(error) => Err(error.to_string()),
                                    },
                                    Err(error) => Err(error.to_string()),
                                },
                                Err(error) => Err(error),
                            }
                        }
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
) -> std::result::Result<(scala_core::SettingsState, scala_core::ModelProfilesState), String> {
    let (settings, profiles) = tokio::join!(settings.read(), profiles.read());
    Ok((
        settings.map_err(|error| error.to_string())?,
        profiles.map_err(|error| error.to_string())?,
    ))
}

async fn load_tui_settings(
    runtime_packs: &RuntimePackManager,
    settings: &SettingsStore,
    profiles: &ModelProfilesStore,
    structured_path_base: &std::path::Path,
) -> std::result::Result<SettingsLoad, String> {
    let (state, profiles) = read_tui_settings(settings, profiles).await?;
    let (runtime_schemas, runtime_schema_warnings) = runtime_packs
        .selected_runtime_settings_schemas(&state, structured_path_base)
        .await
        .unwrap_or_else(|error| (Default::default(), vec![error.to_string()]));
    Ok(SettingsLoad {
        state,
        profiles,
        runtime_schemas,
        runtime_schema_warnings,
        save_notice: None,
    })
}

async fn finish_settings_write(
    runtime_packs: &RuntimePackManager,
    paths: &AppPaths,
    settings: &SettingsStore,
    profiles: &ModelProfilesStore,
    result: std::result::Result<(), String>,
) -> SettingsTaskResult {
    SettingsTaskResult::Stored(match result {
        Ok(()) => load_tui_settings(runtime_packs, settings, profiles, &paths.data_dir).await,
        Err(error) => Err(error),
    })
}

fn resolved_patch(
    engine_id: &str,
    profile_id: &scala_core::ModelProfileId,
    patch: &SettingsPatch,
) -> scala_core::ResolvedSettings {
    scala_core::ResolvedSettings {
        engine_id: engine_id.to_owned(),
        model_profile_id: Some(profile_id.clone()),
        configured: patch
            .0
            .iter()
            .map(|(id, value)| {
                (
                    id.clone(),
                    scala_core::ResolvedSetting {
                        value: value.clone(),
                        source: scala_core::SettingSource::ModelProfile {
                            model_profile_id: profile_id.clone(),
                        },
                    },
                )
            })
            .collect(),
        effective: Default::default(),
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
        SettingsAction::Refresh => SettingsTaskResult::Loaded(
            load_tui_settings(
                &runtime_packs,
                &settings_store,
                &profiles_store,
                &paths.data_dir,
            )
            .await,
        ),
        SettingsAction::Set {
            scope,
            id,
            value,
            model,
        } => {
            let mut patch = SettingsPatch::default();
            patch.insert(id, value);
            if patch.0.keys().any(|id| id.as_str() == "q27.template_path")
                && let Err(error) = scala_engine::record_local_file_setting_identity(
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
                    && let Err(error) = scala_engine::record_local_file_setting_identity(
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
                let state = match settings_store.read().await {
                    Ok(state) => state,
                    Err(error) => return SettingsTaskResult::Stored(Err(error.to_string())),
                };
                let mut candidate = profile.overrides.clone();
                candidate.0.extend(patch.0.clone());
                if let Err(error) = runtime_packs
                    .validate_profile_settings(&state, profile, model, &candidate, &paths.data_dir)
                    .await
                {
                    return SettingsTaskResult::Stored(Err(error.to_string()));
                }
            }

            if let SettingsScope::Runtime(engine_id) = &scope {
                let state = match settings_store.read().await {
                    Ok(state) => state,
                    Err(error) => return SettingsTaskResult::Stored(Err(error.to_string())),
                };
                let (mut schemas, warnings) = match runtime_packs
                    .selected_runtime_settings_schemas(&state, &paths.data_dir)
                    .await
                {
                    Ok(result) => result,
                    Err(error) => return SettingsTaskResult::Stored(Err(error.to_string())),
                };
                let Some(schema) = schemas.remove(engine_id) else {
                    return SettingsTaskResult::Stored(Err(format!(
                        "Selected runtime schema for `{engine_id}` is unavailable{}",
                        if warnings.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", warnings.join("; "))
                        }
                    )));
                };
                let mut candidate = state;
                candidate
                    .runtime_defaults
                    .entry(engine_id.clone())
                    .or_default()
                    .0
                    .extend(patch.0.clone());
                let resolved = match candidate.resolve_runtime_defaults(engine_id, &paths.data_dir)
                {
                    Ok(resolved) => resolved,
                    Err(error) => return SettingsTaskResult::Stored(Err(error.to_string())),
                };
                if let Err(error) = runtime_packs
                    .validate_runtime_settings(&schema, &resolved)
                    .await
                {
                    return SettingsTaskResult::Stored(Err(error.to_string()));
                }
            }
            let result = match scope {
                SettingsScope::Server | SettingsScope::Runtime(_) => settings_store
                    .update(move |state| {
                        match scope {
                            SettingsScope::Server => {
                                if let Some(id) =
                                    patch.0.keys().find(|id| id.namespace() != Some("server"))
                                {
                                    return Err(SettingsError::InvalidServerSetting(id.clone()));
                                }
                                state.server_settings.0.extend(patch.0);
                            }
                            SettingsScope::Runtime(engine_id) => {
                                if let Some(id) =
                                    patch.0.keys().find(|id| !id.applies_to_engine(&engine_id))
                                {
                                    return Err(SettingsError::WrongEngineScope {
                                        setting_id: id.clone(),
                                        engine_id,
                                    });
                                }
                                state
                                    .runtime_defaults
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
            finish_settings_write(
                &runtime_packs,
                paths,
                &settings_store,
                &profiles_store,
                result,
            )
            .await
        }
        SettingsAction::Reset { scope } => {
            let result = match scope {
                SettingsScope::ModelProfile(id) => profiles_store
                    .update(move |state| {
                        let profile = state
                            .profiles
                            .get_mut(&id)
                            .ok_or(SettingsError::ModelProfileNotFound(id))?;
                        profile.overrides.0.clear();
                        Ok(())
                    })
                    .await
                    .map_err(|error| error.to_string()),
                scope => settings_store
                    .update(move |state| {
                        match scope {
                            SettingsScope::Server => state.server_settings.0.clear(),
                            SettingsScope::Runtime(engine) => {
                                state.runtime_defaults.remove(&engine);
                            }
                            SettingsScope::ModelProfile(_) => unreachable!(),
                        }
                        Ok(())
                    })
                    .await
                    .map_err(|error| error.to_string()),
            };
            finish_settings_write(
                &runtime_packs,
                paths,
                &settings_store,
                &profiles_store,
                result,
            )
            .await
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
                SettingsScope::Server | SettingsScope::Runtime(_) => settings_store
                    .update(move |state| {
                        match scope {
                            SettingsScope::Server => {
                                for id in &ids {
                                    state.server_settings.remove(id);
                                }
                            }
                            SettingsScope::Runtime(engine_id) => {
                                if let Some(patch) = state.runtime_defaults.get_mut(&engine_id) {
                                    for id in &ids {
                                        patch.remove(id);
                                    }
                                    if patch.is_empty() {
                                        state.runtime_defaults.remove(&engine_id);
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
            finish_settings_write(
                &runtime_packs,
                paths,
                &settings_store,
                &profiles_store,
                result,
            )
            .await
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
            finish_settings_write(
                &runtime_packs,
                paths,
                &settings_store,
                &profiles_store,
                result,
            )
            .await
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
            finish_settings_write(
                &runtime_packs,
                paths,
                &settings_store,
                &profiles_store,
                result,
            )
            .await
        }
        SettingsAction::DeleteProfile(profile_id) => {
            let active = match ControlClient::discover(paths).await {
                Ok(client) => client.status().await.ok().is_some_and(|status| {
                    status
                        .backends
                        .iter()
                        .any(|backend| backend.model_profile_id == profile_id)
                }),
                Err(_) => false,
            };
            let result = if active {
                Err(format!(
                    "Active Model Profile `{profile_id}` cannot be deleted; unload it first"
                ))
            } else {
                profiles_store
                    .update(move |state| state.delete(&profile_id).map(|_| ()))
                    .await
                    .map_err(|error| error.to_string())
            };
            finish_settings_write(
                &runtime_packs,
                paths,
                &settings_store,
                &profiles_store,
                result,
            )
            .await
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
            finish_settings_write(
                &runtime_packs,
                paths,
                &settings_store,
                &profiles_store,
                result,
            )
            .await
        }
        SettingsAction::CycleProfileEngine { profile_id, model } => {
            let compatible = runtime_packs.compatible_engine_ids(&model);
            let (settings, current) =
                match read_tui_settings(&settings_store, &profiles_store).await {
                    Ok(state) => state,
                    Err(error) => return SettingsTaskResult::Stored(Err(error.to_string())),
                };
            let Some(existing) = current.profiles.get(&profile_id) else {
                return SettingsTaskResult::Stored(Err(SettingsError::ModelProfileNotFound(
                    profile_id,
                )
                .to_string()));
            };
            let next = compatible
                .iter()
                .position(|engine| engine == existing.engine_id.as_str())
                .map(|index| (index + 1) % compatible.len())
                .unwrap_or(0);
            let Some(engine) = compatible.get(next) else {
                return SettingsTaskResult::Stored(Err(SettingsError::InvalidModelProfile(
                    format!(
                        "no registered engine is compatible with model `{}`",
                        model.id
                    ),
                )
                .to_string()));
            };
            let engine = match EngineId::new(engine.clone()) {
                Ok(engine) => engine,
                Err(error) => return SettingsTaskResult::Stored(Err(error.to_string())),
            };
            let candidate = match runtime_packs
                .model_profile_engine_switch_candidate(
                    &settings,
                    existing,
                    &model,
                    engine.as_str(),
                    &paths.data_dir,
                )
                .await
            {
                Ok(candidate) => candidate,
                Err(error) => return SettingsTaskResult::Stored(Err(error.to_string())),
            };
            let original_hash = existing.content_hash();
            let overrides = candidate.overrides;
            let removed = candidate
                .removed
                .into_iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>();
            let result = profiles_store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&profile_id)
                        .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?;
                    if profile.content_hash() != original_hash {
                        return Err(SettingsError::InvalidModelProfile(format!(
                            "Model Profile `{profile_id}` changed during engine-switch preflight"
                        )));
                    }
                    profile.engine_id = engine;
                    profile.overrides = overrides;
                    Ok(())
                })
                .await
                .map_err(|error| error.to_string());
            let mut stored = finish_settings_write(
                &runtime_packs,
                paths,
                &settings_store,
                &profiles_store,
                result,
            )
            .await;
            if let SettingsTaskResult::Stored(Ok(loaded)) = &mut stored
                && !removed.is_empty()
            {
                loaded.save_notice = Some(format!(
                    "Engine changed; removed incompatible overrides: {}",
                    removed.join(", ")
                ));
            }
            stored
        }
        SettingsAction::CycleProfileRole { profile_id } => {
            let result = profiles_store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&profile_id)
                        .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?;
                    profile.role = match profile.role {
                        scala_core::ModelRole::Primary => scala_core::ModelRole::Auxiliary,
                        scala_core::ModelRole::Auxiliary => scala_core::ModelRole::Primary,
                    };
                    Ok(())
                })
                .await
                .map_err(|error| error.to_string());
            finish_settings_write(
                &runtime_packs,
                paths,
                &settings_store,
                &profiles_store,
                result,
            )
            .await
        }
        SettingsAction::InspectProfile { profile, model } => {
            let model_id = model.id.clone();
            let inspected_profile = profile.clone();
            let result = async {
                let (state, profiles) = read_tui_settings(&settings_store, &profiles_store).await?;
                let profile = profiles.profiles.get(&profile.id).cloned()
                    .ok_or_else(|| format!("Model Profile `{}` no longer exists", profile.id))?;
                if profile.model_id != model.id {
                    return Err("Profile model binding changed during inspection; refresh the profile".to_owned());
                }
                let mut resolved = state
                    .resolve(
                        &profile.id,
                        profile.engine_id.as_str(),
                        &profile.overrides,
                        &SettingsPatch::default(),
                        &paths.data_dir,
                    )
                    .map_err(|error| error.to_string())?;
                runtime_packs
                    .normalize_settings(profile.engine_id.as_str(), &mut resolved)
                    .map_err(|error| error.to_string())?;
                let mut parent = state.resolve(&profile.id, profile.engine_id.as_str(), &SettingsPatch::default(), &SettingsPatch::default(), &paths.data_dir)
                    .map_err(|error| error.to_string())?;
                runtime_packs.normalize_settings(profile.engine_id.as_str(), &mut parent).map_err(|error| error.to_string())?;
                let model_schema = runtime_packs
                    .model_settings_schema_for_engine(&model, profile.engine_id.as_str())
                    .map_err(|error| error.to_string())?;
                let (runtime_id, mut schema, mut validation_error) = match runtime_packs
                    .resolve_for_settings(&model, profile.engine_id.as_str(), None, None).await {
                    Ok(selection) => {
                        let selected_id = selection.runtime.manifest.runtime_id.clone();
                        match runtime_packs.settings_schema_for_model_for_engine_with_settings(
                            &model, profile.engine_id.as_str(), Some(&selected_id), Some(&resolved)).await {
                            Ok((_, schema)) => {
                                match runtime_packs.settings_schema_for_model_for_engine_with_settings(
                                    &model, profile.engine_id.as_str(), Some(&selected_id), Some(&parent)).await {
                                    Ok((_, parent_schema)) => { parent_schema.materialize_effective(&mut parent).map_err(|error| error.to_string())?; }
                                    Err(error) => { return Err(format!("Parent settings inspection for runtime `{selected_id}` failed: {error}")); }
                                }
                                let error = schema.validate(&resolved).err().map(|error| error.to_string())
                                    .or(runtime_packs.validate_configuration(&selection.runtime, Some(&model), &resolved).await.err().map(|error| error.to_string()));
                                (Some(selected_id), schema, error)
                            }
                            Err(error) => {
                                let mut schema = model_schema;
                                schema.runtime_id = Some(selected_id.clone());
                                for definition in &mut schema.definitions {
                                    definition.supported = false;
                                    definition.default_preview = None;
                                    definition.unsupported_reason = Some("Runtime metadata/probe failed".to_owned());
                                }
                                (Some(selected_id), schema, Some(format!("Runtime metadata/probe failed: {error}")))
                            }
                        }
                    }
                    Err(error) => (None, model_schema, Some(format!("Runtime selection for Model Profile `{}` on engine `{}` failed: {error}", profile.id, profile.engine_id))),
                };
                schema.retain_override_definitions(&resolved);
                if let Err(error) = schema.materialize_effective(&mut resolved) {
                    validation_error = Some(error.to_string());
                }
                Ok(ModelSettingsInspection {
                    state,
                    profiles,
                    runtime_id,
                    schema,
                    resolved,
                    parent,
                    validation_error,
                })
            }
            .await;
            SettingsTaskResult::Inspected {
                profile: *inspected_profile,
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
                    Ok(status) => status
                        .backends
                        .into_iter()
                        .find_map(|backend| backend.runtime_id),
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
    link: Result<scala_engine::link::LinkSnapshot, String>,
    status: Option<ControlStatus>,
    error: Option<String>,
}

async fn observe_control(paths: &AppPaths) -> ControlObservation {
    match ControlClient::discover(paths).await {
        Ok(client) => {
            let (status, link) = tokio::join!(client.status(), client.link_status());
            let link = link.map_err(|e| e.to_string());
            match status {
                Ok(status) => ControlObservation {
                    status: Some(status),
                    error: None,
                    link,
                },
                Err(error) => ControlObservation {
                    status: None,
                    error: Some(error.to_string()),
                    link,
                },
            }
        }
        Err(error) => ControlObservation {
            status: None,
            error: Some(error.to_string()),
            link: Err(error.to_string()),
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
        ControlAction::Unload(profile_id) => client.unload(profile_id).await,
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
