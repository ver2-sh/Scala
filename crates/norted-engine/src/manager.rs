use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use norted_core::{
    ApplicationCore, EnvironmentVariableProvenance, LoadProfileName, LoadProfilesStore,
    LoadSettingsPatch, LoadSettingsProvenance, ModelId, NativeArgumentProvenance, ProcessIdentity,
    RuntimeId, RuntimeProvenance, RuntimeSelection,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};

use crate::{
    EffectiveGenerationSettings, EngineAdapter, EngineError, EngineIdentity, EngineProbe,
    EngineRegistry, InferenceRequest, InstallationState, LaunchRequest, ProcessDescriptor,
    ProcessExit, ProcessSupervisor, RoutedInferenceOutput, RoutedInferenceStream, RuntimeLease,
    RuntimePackError, RuntimePackManager, StartupObservation,
};

const NOTICE_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendLifecycle {
    Stopped,
    Loading,
    Running,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatus {
    pub identity: EngineIdentity,
    pub probe: EngineProbe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendStatus {
    pub lifecycle: BackendLifecycle,
    pub model_id: Option<ModelId>,
    pub engine_id: Option<String>,
    pub runtime_id: Option<RuntimeId>,
    pub runtime_version: Option<String>,
    pub runtime_variant: Option<String>,
    pub runtime_executable_sha256: Option<String>,
    pub process_id: Option<u32>,
    pub private_endpoint: Option<String>,
    pub failure: Option<String>,
    pub provenance: Option<RuntimeProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeNotice {
    pub timestamp_unix: i64,
    pub level: RuntimeNoticeLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeNoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlStatus {
    pub public_endpoint: Option<String>,
    pub available_engine_count: usize,
    pub installed_engine_count: usize,
    pub running_engine_count: usize,
    pub engines: Vec<EngineStatus>,
    pub backend: BackendStatus,
    pub recent_events: Vec<RuntimeNotice>,
}

#[derive(Debug, Clone)]
pub struct RuntimeManagerOptions {
    pub startup_timeout: Duration,
    pub health_poll_interval: Duration,
}

impl Default for RuntimeManagerOptions {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(5 * 60),
            health_poll_interval: Duration::from_millis(250),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("model `{0}` does not exist in the discovered registry")]
    ModelNotFound(ModelId),
    #[error("model `{0}` is not loaded")]
    ModelNotLoaded(ModelId),
    #[error("model `{model_id}` is already active with engine `{engine_id}`")]
    AlreadyActive {
        model_id: ModelId,
        engine_id: String,
    },
    #[error("the backend is currently {0:?}")]
    Busy(BackendLifecycle),
    #[error("no registered engine is compatible with model `{model_id}`: {reason}")]
    Incompatible { model_id: ModelId, reason: String },
    #[error("no compatible installed engine is available: {0}")]
    EngineUnavailable(String),
    #[error("engine failed during startup: {0}")]
    StartupFailed(String),
    #[error("engine startup timed out after {0:?}")]
    StartupTimedOut(Duration),
    #[error("the loaded backend crashed: {0}")]
    BackendCrashed(String),
    #[error("invalid generation settings: {0}")]
    InvalidGenerationSettings(String),
    #[error("backend inference failed: {0}")]
    Inference(String),
    #[error("backend inference timed out: {0}")]
    InferenceTimedOut(String),
    #[error("backend inference is unavailable: {0}")]
    InferenceUnavailable(String),
    #[error("runtime operation failed: {0}")]
    Operation(String),
}

struct ActiveBackend {
    adapter: Arc<dyn EngineAdapter>,
    process: ProcessDescriptor,
    model_id: ModelId,
    engine_id: String,
    endpoint: String,
    effective_generation_settings: EffectiveGenerationSettings,
    _runtime_lease: RuntimeLease,
}

struct ManagerState {
    public_endpoint: Option<String>,
    engines: BTreeMap<String, EngineStatus>,
    lifecycle: BackendLifecycle,
    model_id: Option<ModelId>,
    engine_id: Option<String>,
    runtime_id: Option<RuntimeId>,
    active: Option<ActiveBackend>,
    loading_process: Option<ProcessDescriptor>,
    loading_runtime_lease: Option<RuntimeLease>,
    cancel_loading: bool,
    failure: Option<String>,
    provenance: Option<RuntimeProvenance>,
    generation: u64,
    notices: VecDeque<RuntimeNotice>,
}

pub struct RuntimeManager {
    core: Arc<ApplicationCore>,
    registry: EngineRegistry,
    packs: Arc<RuntimePackManager>,
    supervisor: Arc<dyn ProcessSupervisor>,
    options: RuntimeManagerOptions,
    state: RwLock<ManagerState>,
    operation: Mutex<()>,
    cancellation_epoch: AtomicU64,
    shutting_down: AtomicBool,
    load_profiles: LoadProfilesStore,
}

impl RuntimeManager {
    pub async fn initialize(
        core: Arc<ApplicationCore>,
        registry: EngineRegistry,
        packs: Arc<RuntimePackManager>,
        supervisor: Arc<dyn ProcessSupervisor>,
        options: RuntimeManagerOptions,
    ) -> Arc<Self> {
        let load_profiles = LoadProfilesStore::new(&core.paths);
        let manager = Arc::new(Self {
            core,
            registry,
            packs,
            supervisor,
            options,
            state: RwLock::new(ManagerState {
                public_endpoint: None,
                engines: BTreeMap::new(),
                lifecycle: BackendLifecycle::Stopped,
                model_id: None,
                engine_id: None,
                runtime_id: None,
                active: None,
                loading_process: None,
                loading_runtime_lease: None,
                cancel_loading: false,
                failure: None,
                provenance: None,
                generation: 0,
                notices: VecDeque::new(),
            }),
            operation: Mutex::new(()),
            cancellation_epoch: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
            load_profiles,
        });
        manager.refresh_engine_probes().await;
        manager
    }

    pub async fn set_public_endpoint(&self, endpoint: String) {
        self.state.write().await.public_endpoint = Some(endpoint);
    }

    pub async fn refresh_engine_probes(&self) {
        for adapter in self.registry.adapters() {
            let identity = adapter.identity();
            let probe = adapter.probe().await.unwrap_or_else(|error| EngineProbe {
                installation: InstallationState::Invalid {
                    reason: error.to_string(),
                },
                update: crate::UpdateState::Unknown,
                healthy: false,
                detail: error.to_string(),
            });
            self.state
                .write()
                .await
                .engines
                .insert(identity.id.clone(), EngineStatus { identity, probe });
        }
    }

    pub async fn status(&self) -> ControlStatus {
        let state = self.state.read().await;
        let engines = state.engines.values().cloned().collect::<Vec<_>>();
        let installed_engine_count = engines
            .iter()
            .filter(|status| {
                matches!(
                    status.probe.installation,
                    InstallationState::Installed { .. }
                )
            })
            .count();
        let active = state.active.as_ref();
        let process_id = active.map(|active| active.process.process_id).or_else(|| {
            state
                .loading_process
                .as_ref()
                .map(|process| process.process_id)
        });
        ControlStatus {
            public_endpoint: state.public_endpoint.clone(),
            available_engine_count: engines.len(),
            installed_engine_count,
            running_engine_count: usize::from(state.lifecycle == BackendLifecycle::Running),
            engines,
            backend: BackendStatus {
                lifecycle: state.lifecycle,
                model_id: state.model_id.clone(),
                engine_id: state.engine_id.clone(),
                runtime_id: state.runtime_id.clone(),
                runtime_version: state
                    .runtime_id
                    .as_ref()
                    .and(state.provenance.as_ref())
                    .map(|provenance| provenance.runtime.identity.version.clone()),
                runtime_variant: state
                    .runtime_id
                    .as_ref()
                    .and(state.provenance.as_ref())
                    .map(|provenance| provenance.runtime.identity.variant.clone()),
                runtime_executable_sha256: state
                    .runtime_id
                    .as_ref()
                    .and(state.provenance.as_ref())
                    .map(|provenance| provenance.runtime.entrypoint_sha256.clone()),
                process_id,
                private_endpoint: if state.lifecycle == BackendLifecycle::Stopped {
                    None
                } else {
                    active.map(|active| active.endpoint.clone()).or_else(|| {
                        state
                            .provenance
                            .as_ref()
                            .map(|provenance| provenance.private_backend_endpoint.clone())
                    })
                },
                failure: state.failure.clone(),
                provenance: state.provenance.clone(),
            },
            recent_events: state.notices.iter().cloned().collect(),
        }
    }

    pub async fn load(self: &Arc<Self>, model_id: ModelId) -> Result<ControlStatus, RuntimeError> {
        self.load_with_runtime(model_id, None).await
    }

    pub async fn load_with_runtime(
        self: &Arc<Self>,
        model_id: ModelId,
        runtime_id: Option<RuntimeId>,
    ) -> Result<ControlStatus, RuntimeError> {
        self.load_with_settings(model_id, runtime_id, None, LoadSettingsPatch::default())
            .await
    }

    pub async fn load_with_settings(
        self: &Arc<Self>,
        model_id: ModelId,
        runtime_id: Option<RuntimeId>,
        profile: Option<LoadProfileName>,
        settings: LoadSettingsPatch,
    ) -> Result<ControlStatus, RuntimeError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(RuntimeError::Operation(
                "the runtime is shutting down".to_owned(),
            ));
        }
        let cancellation_epoch = self.cancellation_epoch.load(Ordering::Acquire);
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            manager
                .load_inner(model_id, runtime_id, profile, settings, cancellation_epoch)
                .await
        })
        .await
        .map_err(|error| RuntimeError::Operation(format!("load task failed: {error}")))?
    }

    async fn load_inner(
        self: &Arc<Self>,
        model_id: ModelId,
        runtime_id: Option<RuntimeId>,
        profile: Option<LoadProfileName>,
        invocation_settings: LoadSettingsPatch,
        cancellation_epoch: u64,
    ) -> Result<ControlStatus, RuntimeError> {
        let _operation = self.operation.lock().await;
        self.ensure_idle_for_load().await?;
        if self.load_cancelled(cancellation_epoch) {
            return Err(RuntimeError::Operation(
                "model load was cancelled".to_owned(),
            ));
        }
        let model = self
            .core
            .model(&model_id)
            .await
            .ok_or_else(|| RuntimeError::ModelNotFound(model_id.clone()))?;

        let generation = {
            let mut state = self.state.write().await;
            state.generation = state.generation.wrapping_add(1);
            state.lifecycle = BackendLifecycle::Loading;
            state.model_id = Some(model_id.clone());
            state.engine_id = None;
            state.runtime_id = None;
            state.failure = None;
            state.active = None;
            state.loading_process = None;
            state.loading_runtime_lease = None;
            state.cancel_loading = self.load_cancelled(cancellation_epoch);
            state.provenance = None;
            push_notice(
                &mut state,
                RuntimeNoticeLevel::Info,
                format!("Loading model {model_id}"),
            );
            state.generation
        };
        if self.load_cancelled(cancellation_epoch) {
            let detail = "model load was cancelled".to_owned();
            self.fail_loading(generation, detail.clone(), None).await;
            return Err(RuntimeError::Operation(detail));
        }

        let (adapter, selection) = match self.select_runtime(&model, runtime_id.as_ref()).await {
            Ok(selection) => selection,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(error);
            }
        };
        let engine_id = adapter.identity().id;
        if let Some(id) = invocation_settings
            .0
            .keys()
            .find(|id| !id.applies_to_engine(&engine_id))
        {
            let error = RuntimeError::Operation(format!(
                "invocation load setting `{id}` does not apply to selected engine `{engine_id}`"
            ));
            self.fail_loading(generation, error.to_string(), None).await;
            return Err(error);
        }
        let profile_state = match self.load_profiles.read().await {
            Ok(state) => state,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::Operation(error.to_string()));
            }
        };
        let mut resolved_load_settings = match profile_state.resolve(
            &model_id,
            &engine_id,
            profile.as_ref(),
            &invocation_settings,
            &self.core.paths.data_dir,
        ) {
            Ok(settings) => settings,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::Operation(error.to_string()));
            }
        };
        if let Err(error) =
            norted_core::apply_norted_package_load_policy(&model, &mut resolved_load_settings)
        {
            self.fail_loading(generation, error.clone(), None).await;
            return Err(RuntimeError::Operation(error));
        }
        let host = self.packs.host_capabilities().await;
        let load_settings_schema = match adapter
            .load_settings_schema(&selection.runtime, &model, &host)
            .await
        {
            Ok(schema) => schema,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::StartupFailed(error.to_string()));
            }
        };
        let selected_runtime_id = selection.runtime.manifest.runtime_id.clone();
        let runtime_lease = match self.packs.acquire_runtime_lease(&selected_runtime_id).await {
            Ok(lease) => lease,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::Operation(error.to_string()));
            }
        };
        let cancelled = {
            let mut state = self.state.write().await;
            if state.generation != generation
                || state.lifecycle != BackendLifecycle::Loading
                || state.cancel_loading
                || self.load_cancelled(cancellation_epoch)
            {
                true
            } else {
                state.engine_id = Some(engine_id.clone());
                state.runtime_id = Some(selected_runtime_id.clone());
                state.loading_runtime_lease = Some(runtime_lease);
                for notice in &selection.notices {
                    push_notice(&mut state, RuntimeNoticeLevel::Warning, notice.clone());
                }
                false
            }
        };
        if cancelled {
            let detail = "model load was cancelled".to_owned();
            self.fail_loading(generation, detail.clone(), None).await;
            return Err(RuntimeError::Operation(detail));
        }

        let backend_address = match reserve_loopback_address() {
            Ok(address) => address,
            Err(error) => {
                let detail = format!("could not allocate a private backend port: {error}");
                self.fail_loading(generation, detail.clone(), None).await;
                return Err(RuntimeError::Operation(detail));
            }
        };
        let prepared_model = match adapter.prepare_model_input(&model).await {
            Ok(model) => model,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::StartupFailed(error.to_string()));
            }
        };
        let launch_attempts = match adapter
            .build_launch_attempts(LaunchRequest {
                model: prepared_model,
                runtime: selection.runtime.clone(),
                accelerator: selection.accelerator.clone(),
                backend_address,
                load_settings: resolved_load_settings,
                load_settings_schema,
            })
            .await
        {
            Ok(attempts) if !attempts.is_empty() => attempts,
            Ok(_) => {
                let detail = "engine adapter produced no launch attempts".to_owned();
                self.fail_loading(generation, detail.clone(), None).await;
                return Err(RuntimeError::StartupFailed(detail));
            }
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::StartupFailed(error.to_string()));
            }
        };
        let mut launch_attempts = VecDeque::from(launch_attempts);
        if self.load_cancelled(cancellation_epoch) {
            cleanup_pending_launch_files(&launch_attempts).await;
            let detail = "model load was cancelled".to_owned();
            self.fail_loading(generation, detail.clone(), None).await;
            return Err(RuntimeError::Operation(detail));
        }
        let mut context_attempts = Vec::new();
        let (process, endpoint, exit, mut startup_observation, effective_generation_settings) = loop {
            let launch_spec = launch_attempts
                .pop_front()
                .expect("launch attempts were checked as non-empty");
            if self.load_cancelled(cancellation_epoch) {
                cleanup_temporary_launch_files(&launch_spec.temporary_files).await;
                cleanup_pending_launch_files(&launch_attempts).await;
                let detail = "model load was cancelled".to_owned();
                self.fail_loading(generation, detail.clone(), None).await;
                return Err(RuntimeError::Operation(detail));
            }
            if let Err(error) = adapter.prepare_launch_attempt(&launch_spec).await {
                cleanup_temporary_launch_files(&launch_spec.temporary_files).await;
                cleanup_pending_launch_files(&launch_attempts).await;
                adapter
                    .clear_launch_state(launch_spec.endpoint.as_deref())
                    .await;
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::StartupFailed(error.to_string()));
            }

            let installation = launch_spec.installation.clone();
            let selected_runtime = launch_spec.runtime.clone();
            let selected_accelerator = launch_spec.accelerator.clone();
            let mut model_identity = launch_spec.model.runtime_identity();
            let normalized_settings = launch_spec.normalized_settings.clone();
            let load_settings = launch_spec.load_settings.clone();
            if let Some(package) = &mut model_identity.norted_package
                && let Some(norted_core::LoadSettingValue::Choice(profile)) =
                    load_settings.value("ninfer.package_profile")
            {
                package.selected_package_profile = Some(profile.clone());
            }
            let native_arguments = launch_spec.native_arguments.clone();
            let inherits_parent_environment = launch_spec.inherits_parent_environment;
            let native_environment = environment_provenance(
                &launch_spec.environment,
                &launch_spec.environment_remove,
                inherits_parent_environment,
            );
            let temporary_files = launch_spec.temporary_files.clone();
            let launch_endpoint = launch_spec.endpoint.clone();
            let process = match self
                .supervisor
                .spawn(launch_spec, installation.engine.clone(), model_id.clone())
                .await
            {
                Ok(process) => process,
                Err(error) => {
                    cleanup_temporary_launch_files(&temporary_files).await;
                    cleanup_pending_launch_files(&launch_attempts).await;
                    adapter.clear_launch_state(launch_endpoint.as_deref()).await;
                    self.fail_loading(generation, error.to_string(), None).await;
                    return Err(RuntimeError::StartupFailed(error.to_string()));
                }
            };
            let cancelled = {
                let mut state = self.state.write().await;
                if state.generation != generation
                    || state.lifecycle != BackendLifecycle::Loading
                    || state.cancel_loading
                    || self.load_cancelled(cancellation_epoch)
                {
                    true
                } else {
                    state.loading_process = Some(process.clone());
                    false
                }
            };
            if cancelled {
                cleanup_pending_launch_files(&launch_attempts).await;
                let detail = "model load was cancelled".to_owned();
                let retained = self.terminate_or_retain(&process).await;
                adapter
                    .clear_launch_state(process.endpoint.as_deref())
                    .await;
                self.fail_loading(generation, detail.clone(), retained.as_ref())
                    .await;
                return Err(RuntimeError::Operation(detail));
            }
            let endpoint = match process.endpoint.clone() {
                Some(endpoint) => endpoint,
                None => {
                    cleanup_pending_launch_files(&launch_attempts).await;
                    let detail = "engine launch did not provide a backend endpoint".to_owned();
                    let retained = self.terminate_or_retain(&process).await;
                    adapter.clear_launch_state(None).await;
                    self.fail_loading(generation, detail.clone(), retained.as_ref())
                        .await;
                    return Err(RuntimeError::StartupFailed(detail));
                }
            };
            let provenance = RuntimeProvenance {
                model: model_identity,
                runtime: selected_runtime.manifest.clone(),
                runtime_entrypoint: selected_runtime.entrypoint_path(),
                selection_source: selection.source,
                accelerator: selected_accelerator,
                installation,
                profile: load_settings
                    .selected_profile
                    .as_ref()
                    .map(ToString::to_string),
                load_settings: LoadSettingsProvenance {
                    effective: load_settings.effective,
                },
                normalized_settings,
                native_arguments: native_argument_provenance(native_arguments),
                native_environment,
                inherits_parent_environment,
                process: ProcessIdentity {
                    process_id: process.process_id,
                    process_start_identity: Some(process.supervisor_id.clone()),
                },
                private_backend_endpoint: endpoint.clone(),
                launched_at_unix: process.launched_at_unix,
            };
            self.state.write().await.provenance = Some(provenance);
            let mut exit = match self.supervisor.subscribe(&process).await {
                Ok(exit) => exit,
                Err(error) => {
                    cleanup_pending_launch_files(&launch_attempts).await;
                    let detail = error.to_string();
                    let retained = self.terminate_or_retain(&process).await;
                    adapter.clear_launch_state(Some(&endpoint)).await;
                    self.fail_loading(generation, detail.clone(), retained.as_ref())
                        .await;
                    return Err(RuntimeError::StartupFailed(detail));
                }
            };
            if let Err(error) = self
                .wait_for_readiness(adapter.as_ref(), &process, &mut exit)
                .await
            {
                cleanup_pending_launch_files(&launch_attempts).await;
                let detail = error.to_string();
                let retained = self.terminate_or_retain(&process).await;
                adapter.clear_launch_state(Some(&endpoint)).await;
                self.fail_loading(generation, detail.clone(), retained.as_ref())
                    .await;
                return Err(error);
            }
            let mut observation_poll = 0_u8;
            let observation = loop {
                let stderr_tail = match self.supervisor.stderr_tail(&process).await {
                    Ok(stderr_tail) => stderr_tail,
                    Err(error) => {
                        break Err(EngineError::Operation(format!(
                            "could not read bounded engine startup output: {error}"
                        )));
                    }
                };
                match adapter.startup_observation(&process, &stderr_tail).await {
                    Ok(observation) => break Ok(observation),
                    Err(EngineError::BackendUnavailable(_)) if observation_poll < 20 => {
                        observation_poll += 1;
                        if exit.borrow().is_some() {
                            break Err(EngineError::Operation(
                                "engine exited before startup policy observation completed"
                                    .to_owned(),
                            ));
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(error) => break Err(error),
                }
            };
            let observation = match observation {
                Ok(observation) => observation,
                Err(error) => {
                    cleanup_pending_launch_files(&launch_attempts).await;
                    let detail =
                        format!("engine startup did not prove the selected model policy: {error}");
                    let retained = self.terminate_or_retain(&process).await;
                    adapter.clear_launch_state(Some(&endpoint)).await;
                    self.fail_loading(generation, detail.clone(), retained.as_ref())
                        .await;
                    return Err(RuntimeError::StartupFailed(detail));
                }
            };
            match observation {
                StartupObservation::Ready(observation) => {
                    let settings = match adapter.effective_generation_settings(&process).await {
                        Ok(settings) => settings,
                        Err(error) => {
                            cleanup_pending_launch_files(&launch_attempts).await;
                            let detail = format!(
                                "could not obtain effective generation settings from the engine: {error}"
                            );
                            let retained = self.terminate_or_retain(&process).await;
                            adapter.clear_launch_state(Some(&endpoint)).await;
                            self.fail_loading(generation, detail.clone(), retained.as_ref())
                                .await;
                            return Err(RuntimeError::StartupFailed(detail));
                        }
                    };
                    break (process, endpoint, exit, observation, settings);
                }
                StartupObservation::RetryContextCapacity {
                    kv_mode,
                    observed_context,
                    minimum_context,
                } => {
                    context_attempts.push((kv_mode, observed_context));
                    if let Err(error) = self.supervisor.terminate(&process).await {
                        cleanup_pending_launch_files(&launch_attempts).await;
                        let detail = format!(
                            "could not stop the insufficient-context backend before KV fallback: {error}"
                        );
                        adapter.clear_launch_state(Some(&endpoint)).await;
                        self.fail_loading(generation, detail.clone(), Some(&process))
                            .await;
                        return Err(RuntimeError::StartupFailed(detail));
                    }
                    adapter.clear_launch_state(Some(&endpoint)).await;
                    self.state.write().await.loading_process = None;
                    if launch_attempts.is_empty() {
                        let attempts = context_attempts
                            .iter()
                            .map(|(mode, context)| format!("{mode}={context}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let detail = format!(
                            "no exact-runtime-supported package KV mode met the {minimum_context}-token minimum; attempted modes and observed contexts: {attempts}"
                        );
                        self.fail_loading(generation, detail.clone(), None).await;
                        return Err(RuntimeError::StartupFailed(detail));
                    }
                }
            }
        };

        if let (Some(mode), Some(context)) = (
            startup_observation
                .get("observed_kv_mode")
                .and_then(serde_json::Value::as_str),
            startup_observation
                .get("observed_served_context")
                .and_then(serde_json::Value::as_u64),
        ) {
            let mode = mode.to_owned();
            context_attempts.push((mode.clone(), context));
            startup_observation.insert(
                "package_kv_attempts".to_owned(),
                serde_json::Value::Array(
                    context_attempts
                        .iter()
                        .map(|(mode, context)| {
                            serde_json::json!({
                                "kv_mode": mode,
                                "observed_context": context,
                            })
                        })
                        .collect(),
                ),
            );
            startup_observation
                .insert("final_selected_kv_mode".to_owned(), serde_json::json!(mode));
            startup_observation.insert(
                "final_proven_served_context".to_owned(),
                serde_json::json!(context),
            );
        }

        {
            let mut state = self.state.write().await;
            if state.generation != generation
                || state.lifecycle != BackendLifecycle::Loading
                || self.load_cancelled(cancellation_epoch)
            {
                let detail = if state.cancel_loading
                    || state.lifecycle == BackendLifecycle::Stopping
                    || self.load_cancelled(cancellation_epoch)
                {
                    "model load was cancelled"
                } else {
                    "runtime generation changed while the model was loading"
                }
                .to_owned();
                drop(state);
                let retained = self.terminate_or_retain(&process).await;
                adapter
                    .clear_launch_state(process.endpoint.as_deref())
                    .await;
                self.fail_loading(generation, detail.clone(), retained.as_ref())
                    .await;
                return Err(RuntimeError::Operation(detail));
            }
            state.lifecycle = BackendLifecycle::Running;
            state.loading_process = None;
            state.cancel_loading = false;
            if let Some(provenance) = state.provenance.as_mut() {
                if let Some(package) = provenance.model.norted_package.as_mut() {
                    if startup_observation
                        .get("sharp_application")
                        .and_then(serde_json::Value::as_str)
                        == Some("pretokenized-raw-prompt")
                    {
                        package.sharp_applied = Some(true);
                    }
                    if let Some(context) = startup_observation
                        .get("observed_served_context")
                        .and_then(serde_json::Value::as_u64)
                    {
                        package.proven_served_context_tokens = Some(context);
                    }
                }
                provenance.normalized_settings.extend(startup_observation);
                provenance.normalized_settings.insert(
                    "temperature".to_owned(),
                    serde_json::json!(effective_generation_settings.temperature),
                );
                provenance.normalized_settings.insert(
                    "top_p".to_owned(),
                    serde_json::json!(effective_generation_settings.top_p),
                );
            }
            let runtime_lease = state
                .loading_runtime_lease
                .take()
                .expect("a loading runtime must retain its store lease");
            state.active = Some(ActiveBackend {
                adapter,
                process: process.clone(),
                model_id: model_id.clone(),
                engine_id: engine_id.clone(),
                endpoint,
                effective_generation_settings,
                _runtime_lease: runtime_lease,
            });
            push_notice(
                &mut state,
                RuntimeNoticeLevel::Info,
                format!("Model {model_id} is ready"),
            );
        }
        spawn_exit_monitor(Arc::downgrade(self), generation, process, exit);
        Ok(self.status().await)
    }

    pub async fn unload(self: &Arc<Self>) -> Result<ControlStatus, RuntimeError> {
        self.cancellation_epoch.fetch_add(1, Ordering::AcqRel);
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            manager.cancel_loading().await;
            manager.unload_inner().await
        })
        .await
        .map_err(|error| RuntimeError::Operation(format!("unload task failed: {error}")))?
    }

    async fn unload_inner(&self) -> Result<ControlStatus, RuntimeError> {
        let _operation = self.operation.lock().await;
        let (process, adapter) = {
            let mut state = self.state.write().await;
            match state.lifecycle {
                BackendLifecycle::Stopped => {
                    return Ok(self.status_from_state(&state));
                }
                BackendLifecycle::Loading | BackendLifecycle::Stopping => {
                    return Err(RuntimeError::Busy(state.lifecycle));
                }
                BackendLifecycle::Running => {
                    state.lifecycle = BackendLifecycle::Stopping;
                    state.generation = state.generation.wrapping_add(1);
                    push_notice(
                        &mut state,
                        RuntimeNoticeLevel::Info,
                        "Stopping the active backend".to_owned(),
                    );
                    (
                        state.active.as_ref().map(|active| active.process.clone()),
                        state
                            .active
                            .as_ref()
                            .map(|active| Arc::clone(&active.adapter)),
                    )
                }
                BackendLifecycle::Failed => (
                    state
                        .active
                        .as_ref()
                        .map(|active| active.process.clone())
                        .or_else(|| state.loading_process.clone()),
                    state
                        .active
                        .as_ref()
                        .map(|active| Arc::clone(&active.adapter))
                        .or_else(|| {
                            state
                                .engine_id
                                .as_deref()
                                .and_then(|engine_id| self.registry.get(engine_id))
                        }),
                ),
            }
        };
        if let Some(process) = process.as_ref()
            && let Err(error) = self.supervisor.terminate(process).await
        {
            let mut state = self.state.write().await;
            state.lifecycle = BackendLifecycle::Failed;
            state.failure = Some(error.to_string());
            push_notice(
                &mut state,
                RuntimeNoticeLevel::Error,
                format!("Backend termination failed: {error}"),
            );
            return Err(RuntimeError::Operation(error.to_string()));
        }
        if let Some(adapter) = adapter {
            adapter
                .clear_launch_state(
                    process
                        .as_ref()
                        .and_then(|process| process.endpoint.as_deref()),
                )
                .await;
        }
        let mut state = self.state.write().await;
        state.lifecycle = BackendLifecycle::Stopped;
        state.model_id = None;
        state.engine_id = None;
        state.runtime_id = None;
        state.active = None;
        state.loading_process = None;
        state.loading_runtime_lease = None;
        state.cancel_loading = false;
        state.failure = None;
        push_notice(
            &mut state,
            RuntimeNoticeLevel::Info,
            "Backend stopped".to_owned(),
        );
        Ok(self.status_from_state(&state))
    }

    pub async fn infer(
        &self,
        request: InferenceRequest,
    ) -> Result<RoutedInferenceOutput, RuntimeError> {
        let (adapter, endpoint, backend_generation_settings) =
            self.inference_target(&request.model_id).await?;
        adapter
            .validate_generation_settings(
                &request.generation_settings,
                &backend_generation_settings,
            )
            .map_err(map_inference_error)?;
        let effective_generation_settings =
            backend_generation_settings.merged(&request.generation_settings);
        let output = adapter
            .infer(&endpoint, request)
            .await
            .map_err(map_inference_error)?;
        Ok(RoutedInferenceOutput {
            output,
            effective_generation_settings,
        })
    }

    pub async fn infer_stream(
        &self,
        request: InferenceRequest,
    ) -> Result<RoutedInferenceStream, RuntimeError> {
        let (adapter, endpoint, backend_generation_settings) =
            self.inference_target(&request.model_id).await?;
        adapter
            .validate_generation_settings(
                &request.generation_settings,
                &backend_generation_settings,
            )
            .map_err(map_inference_error)?;
        let effective_generation_settings =
            backend_generation_settings.merged(&request.generation_settings);
        let stream = adapter
            .infer_stream(&endpoint, request)
            .await
            .map_err(map_inference_error)?;
        Ok(RoutedInferenceStream {
            stream,
            effective_generation_settings,
        })
    }

    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.cancellation_epoch.fetch_add(1, Ordering::AcqRel);
        self.cancel_loading().await;
        if let Err(error) = self.unload_inner().await {
            tracing::warn!(%error, "could not unload active backend during shutdown");
        }
        self.supervisor.shutdown().await;
    }

    fn load_cancelled(&self, cancellation_epoch: u64) -> bool {
        self.shutting_down.load(Ordering::Acquire)
            || self.cancellation_epoch.load(Ordering::Acquire) != cancellation_epoch
    }

    async fn cancel_loading(&self) {
        let (process, adapter) = {
            let mut state = self.state.write().await;
            if state.lifecycle != BackendLifecycle::Loading {
                return;
            }
            state.lifecycle = BackendLifecycle::Stopping;
            state.cancel_loading = true;
            push_notice(
                &mut state,
                RuntimeNoticeLevel::Info,
                "Cancelling the model load".to_owned(),
            );
            (
                state.loading_process.clone(),
                state
                    .engine_id
                    .as_deref()
                    .and_then(|engine_id| self.registry.get(engine_id)),
            )
        };
        if let Some(process) = process.as_ref()
            && let Err(error) = self.supervisor.terminate(process).await
        {
            tracing::warn!(
                process_id = process.process_id,
                %error,
                "could not terminate a loading backend immediately"
            );
        }
        if let Some(adapter) = adapter {
            adapter
                .clear_launch_state(
                    process
                        .as_ref()
                        .and_then(|process| process.endpoint.as_deref()),
                )
                .await;
        }
    }

    async fn ensure_idle_for_load(&self) -> Result<(), RuntimeError> {
        let state = self.state.read().await;
        if let Some(active) = &state.active {
            return Err(RuntimeError::AlreadyActive {
                model_id: active.model_id.clone(),
                engine_id: active.engine_id.clone(),
            });
        }
        if state.loading_process.is_some() {
            return Err(RuntimeError::Busy(state.lifecycle));
        }
        match state.lifecycle {
            BackendLifecycle::Stopped | BackendLifecycle::Failed => Ok(()),
            lifecycle => Err(RuntimeError::Busy(lifecycle)),
        }
    }

    async fn select_runtime(
        &self,
        model: &norted_core::ModelArtifact,
        explicit_runtime: Option<&RuntimeId>,
    ) -> Result<(Arc<dyn EngineAdapter>, RuntimeSelection), RuntimeError> {
        let selection = self
            .packs
            .resolve(model, explicit_runtime)
            .await
            .map_err(|error| match error {
                RuntimePackError::Incompatible { reason, .. } => RuntimeError::Incompatible {
                    model_id: model.id.clone(),
                    reason,
                },
                error => RuntimeError::EngineUnavailable(error.to_string()),
            })?;
        let engine_id = &selection.runtime.manifest.identity.engine_id;
        let adapter = self.registry.get(engine_id).ok_or_else(|| {
            RuntimeError::EngineUnavailable(format!(
                "selected runtime uses unregistered engine `{engine_id}`"
            ))
        })?;
        Ok((adapter, selection))
    }

    async fn wait_for_readiness(
        &self,
        adapter: &dyn EngineAdapter,
        process: &ProcessDescriptor,
        exit: &mut tokio::sync::watch::Receiver<Option<ProcessExit>>,
    ) -> Result<(), RuntimeError> {
        let deadline = tokio::time::Instant::now() + self.options.startup_timeout;
        loop {
            if let Some(exit) = exit.borrow().clone() {
                return Err(RuntimeError::StartupFailed(exit_detail(&exit)));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(RuntimeError::StartupTimedOut(self.options.startup_timeout));
            }
            match adapter.health(process).await {
                Ok(true) => return Ok(()),
                Ok(false)
                | Err(EngineError::BackendUnavailable(_))
                | Err(EngineError::Operation(_)) => {}
                Err(error) => return Err(RuntimeError::StartupFailed(error.to_string())),
            }
            tokio::select! {
                changed = exit.changed() => {
                    if changed.is_err() {
                        return Err(RuntimeError::StartupFailed(
                            "process exit monitor closed during startup".to_owned(),
                        ));
                    }
                }
                () = tokio::time::sleep(self.options.health_poll_interval) => {}
            }
        }
    }

    async fn fail_loading(
        &self,
        generation: u64,
        detail: String,
        process: Option<&ProcessDescriptor>,
    ) {
        let mut state = self.state.write().await;
        if state.generation != generation {
            return;
        }
        state.lifecycle = BackendLifecycle::Failed;
        state.failure = Some(detail.clone());
        state.active = None;
        state.loading_process = process.cloned();
        if process.is_none() {
            state.runtime_id = None;
            state.loading_runtime_lease = None;
        }
        state.cancel_loading = false;
        if let Some(process) = process
            && let Some(provenance) = &mut state.provenance
        {
            provenance.process.process_id = process.process_id;
        }
        push_notice(
            &mut state,
            RuntimeNoticeLevel::Error,
            format!("Backend startup failed: {detail}"),
        );
    }

    async fn terminate_or_retain(&self, process: &ProcessDescriptor) -> Option<ProcessDescriptor> {
        match self.supervisor.terminate(process).await {
            Ok(()) => None,
            Err(error) => {
                tracing::warn!(
                    process_id = process.process_id,
                    %error,
                    "retaining failed backend process for a later cleanup attempt"
                );
                Some(process.clone())
            }
        }
    }

    async fn inference_target(
        &self,
        model_id: &ModelId,
    ) -> Result<(Arc<dyn EngineAdapter>, String, EffectiveGenerationSettings), RuntimeError> {
        if self.core.model(model_id).await.is_none() {
            return Err(RuntimeError::ModelNotFound(model_id.clone()));
        }
        let state = self.state.read().await;
        match (&state.lifecycle, &state.active) {
            (BackendLifecycle::Running, Some(active)) if &active.model_id == model_id => Ok((
                Arc::clone(&active.adapter),
                active.endpoint.clone(),
                active.effective_generation_settings,
            )),
            (BackendLifecycle::Failed, _) => Err(RuntimeError::BackendCrashed(
                state
                    .failure
                    .clone()
                    .unwrap_or_else(|| "backend is in a failed state".to_owned()),
            )),
            _ => Err(RuntimeError::ModelNotLoaded(model_id.clone())),
        }
    }

    async fn handle_unexpected_exit(
        &self,
        generation: u64,
        process: ProcessDescriptor,
        exit: ProcessExit,
    ) {
        let mut state = self.state.write().await;
        if state.generation != generation
            || state.lifecycle != BackendLifecycle::Running
            || state
                .active
                .as_ref()
                .is_none_or(|active| active.process.supervisor_id != process.supervisor_id)
        {
            return;
        }
        let detail = exit_detail(&exit);
        let adapter = state
            .active
            .as_ref()
            .map(|active| Arc::clone(&active.adapter));
        state.lifecycle = BackendLifecycle::Failed;
        state.failure = Some(detail.clone());
        state.active = None;
        state.runtime_id = None;
        push_notice(
            &mut state,
            RuntimeNoticeLevel::Error,
            format!("Backend exited unexpectedly: {detail}"),
        );
        drop(state);
        if let Some(adapter) = adapter {
            adapter
                .clear_launch_state(process.endpoint.as_deref())
                .await;
        }
    }

    fn status_from_state(&self, state: &ManagerState) -> ControlStatus {
        let engines = state.engines.values().cloned().collect::<Vec<_>>();
        let installed_engine_count = engines
            .iter()
            .filter(|status| {
                matches!(
                    status.probe.installation,
                    InstallationState::Installed { .. }
                )
            })
            .count();
        let active = state.active.as_ref();
        let process_id = active.map(|active| active.process.process_id).or_else(|| {
            state
                .loading_process
                .as_ref()
                .map(|process| process.process_id)
        });
        ControlStatus {
            public_endpoint: state.public_endpoint.clone(),
            available_engine_count: engines.len(),
            installed_engine_count,
            running_engine_count: usize::from(state.lifecycle == BackendLifecycle::Running),
            engines,
            backend: BackendStatus {
                lifecycle: state.lifecycle,
                model_id: state.model_id.clone(),
                engine_id: state.engine_id.clone(),
                runtime_id: state.runtime_id.clone(),
                runtime_version: state
                    .runtime_id
                    .as_ref()
                    .and(state.provenance.as_ref())
                    .map(|provenance| provenance.runtime.identity.version.clone()),
                runtime_variant: state
                    .runtime_id
                    .as_ref()
                    .and(state.provenance.as_ref())
                    .map(|provenance| provenance.runtime.identity.variant.clone()),
                runtime_executable_sha256: state
                    .runtime_id
                    .as_ref()
                    .and(state.provenance.as_ref())
                    .map(|provenance| provenance.runtime.entrypoint_sha256.clone()),
                process_id,
                private_endpoint: if state.lifecycle == BackendLifecycle::Stopped {
                    None
                } else {
                    active.map(|active| active.endpoint.clone()).or_else(|| {
                        state
                            .provenance
                            .as_ref()
                            .map(|provenance| provenance.private_backend_endpoint.clone())
                    })
                },
                failure: state.failure.clone(),
                provenance: state.provenance.clone(),
            },
            recent_events: state.notices.iter().cloned().collect(),
        }
    }
}

async fn cleanup_temporary_launch_files(paths: &[std::path::PathBuf]) {
    for path in paths {
        if let Err(error) = tokio::fs::remove_file(path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), %error, "could not remove temporary engine launch file");
        }
    }
}

async fn cleanup_pending_launch_files(attempts: &VecDeque<crate::LaunchSpec>) {
    for attempt in attempts {
        cleanup_temporary_launch_files(&attempt.temporary_files).await;
    }
}

fn spawn_exit_monitor(
    manager: Weak<RuntimeManager>,
    generation: u64,
    process: ProcessDescriptor,
    mut exit: tokio::sync::watch::Receiver<Option<ProcessExit>>,
) {
    tokio::spawn(async move {
        let observed = loop {
            if let Some(observed) = exit.borrow().clone() {
                break observed;
            }
            if exit.changed().await.is_err() {
                return;
            }
        };
        if observed.expected {
            return;
        }
        if let Some(manager) = manager.upgrade() {
            manager
                .handle_unexpected_exit(generation, process, observed)
                .await;
        }
    });
}

fn reserve_loopback_address() -> std::io::Result<SocketAddr> {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let address = listener.local_addr()?;
    drop(listener);
    Ok(address)
}

fn native_argument_provenance(arguments: Vec<String>) -> Vec<NativeArgumentProvenance> {
    let mut provenance = Vec::with_capacity(arguments.len());
    let mut previous_option = None;
    for (index, argument) in arguments.into_iter().enumerate() {
        if let Some((name, value)) = argument.split_once('=')
            && name.starts_with('-')
        {
            provenance.push(NativeArgumentProvenance::Redacted {
                argument: format!("{name}=<redacted>"),
                value_sha256: hex_digest(Sha256::digest(value.as_bytes())),
            });
            previous_option = None;
        } else if argument.starts_with('-') {
            previous_option = Some(argument.clone());
            provenance.push(NativeArgumentProvenance::Value(argument));
        } else {
            let owner = previous_option
                .take()
                .map(|option| format!("{option}=<redacted>"))
                .unwrap_or_else(|| format!("<positional:{index}>=<redacted>"));
            provenance.push(NativeArgumentProvenance::Redacted {
                argument: owner,
                value_sha256: hex_digest(Sha256::digest(argument.as_bytes())),
            });
        }
    }
    provenance
}

fn environment_provenance(
    environment: &BTreeMap<String, String>,
    environment_remove: &[OsString],
    inherits_parent_environment: bool,
) -> Vec<EnvironmentVariableProvenance> {
    let mut observed = environment
        .iter()
        .map(|(name, value)| (name.clone(), (value.clone(), false)))
        .collect::<BTreeMap<_, _>>();
    if inherits_parent_environment {
        for (name, value) in std::env::vars() {
            let removed = environment_remove
                .iter()
                .any(|removed| name.eq_ignore_ascii_case(&removed.to_string_lossy()));
            let overridden = observed.keys().any(|key| key.eq_ignore_ascii_case(&name));
            if !removed && !overridden {
                observed.insert(name, (value, true));
            }
        }
    }
    observed
        .into_iter()
        .map(|(name, (value, inherited))| EnvironmentVariableProvenance {
            name,
            inherited,
            value_sha256: Some(hex_digest(Sha256::digest(value.as_bytes()))),
        })
        .collect()
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn exit_detail(exit: &ProcessExit) -> String {
    if exit.stderr_tail.is_empty() {
        exit.detail.clone()
    } else {
        format!("{}; {}", exit.detail, exit.stderr_tail.join(" | "))
    }
}

fn map_inference_error(error: EngineError) -> RuntimeError {
    match error {
        EngineError::InvalidGenerationSettings(message) => {
            RuntimeError::InvalidGenerationSettings(message)
        }
        EngineError::TimedOut(message) => RuntimeError::InferenceTimedOut(message),
        EngineError::BackendUnavailable(message) => RuntimeError::InferenceUnavailable(message),
        error => RuntimeError::Inference(error.to_string()),
    }
}

fn push_notice(state: &mut ManagerState, level: RuntimeNoticeLevel, message: String) {
    if state.notices.len() == NOTICE_LIMIT {
        state.notices.pop_front();
    }
    state.notices.push_back(RuntimeNotice {
        timestamp_unix: unix_timestamp(),
        level,
        message,
    });
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}
