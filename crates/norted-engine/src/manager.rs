use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use norted_core::{
    ApplicationCore, BuilderRecommendationStatus, EnvironmentVariableProvenance, LoadSettingsPatch,
    LoadSettingsProvenance, ModelArtifact, ModelId, NativeArgumentProvenance, ProcessIdentity,
    RuntimeId, RuntimeProvenance, RuntimeSelection, ServeProfile, ServeProfileName,
    ServeProfileRuntimeIdentity, ServeProfilesState, ServeProfilesStore,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock, oneshot};

use crate::{
    EffectiveGenerationSettings, EngineAdapter, EngineError, EngineIdentity, EngineProbe,
    EngineRegistry, GenerationSettingsPatch, InferenceRequest, InstallationState, LaunchRequest,
    LoadProgressReporter, ProcessDescriptor, ProcessExit, ProcessSupervisor, RoutedInferenceOutput,
    RoutedInferenceStream, RuntimeLease, RuntimePackError, RuntimePackManager, StartupObservation,
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

impl BackendLifecycle {
    pub fn is_loading(self) -> bool {
        self == Self::Loading
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatus {
    pub identity: EngineIdentity,
    pub probe: EngineProbe,
}

/// Generic model-load phases. These refine `BackendLifecycle::Loading`; they
/// never replace the lifecycle itself.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendLoadPhase {
    Starting,
    SelectingRuntime,
    ResolvingSettings,
    AcquiringRuntime,
    PreparingModel,
    PreparingLaunch,
    SpawningBackend,
    LoadingModel,
    AllocatingContext,
    VerifyingStartup,
}

impl BackendLoadPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting model load",
            Self::SelectingRuntime => "Selecting runtime",
            Self::ResolvingSettings => "Resolving load settings",
            Self::AcquiringRuntime => "Acquiring runtime",
            Self::PreparingModel => "Preparing model input",
            Self::PreparingLaunch => "Preparing launch",
            Self::SpawningBackend => "Spawning backend",
            Self::LoadingModel => "Loading model",
            Self::AllocatingContext => "Allocating context",
            Self::VerifyingStartup => "Verifying startup",
        }
    }

    pub fn failure_label(self) -> &'static str {
        match self {
            Self::Starting => "starting the model load",
            Self::SelectingRuntime => "selecting a runtime",
            Self::ResolvingSettings => "resolving load settings",
            Self::AcquiringRuntime => "acquiring the runtime",
            Self::PreparingModel => "preparing the model input",
            Self::PreparingLaunch => "preparing the launch",
            Self::SpawningBackend => "spawning the backend",
            Self::LoadingModel => "loading the model",
            Self::AllocatingContext => "allocating context",
            Self::VerifyingStartup => "verifying startup",
        }
    }
}

/// Engine-neutral model-load progress. A determinate fraction is present only
/// when the exact runtime exposes trustworthy measurable progress; phase
/// transitions alone never invent a percentage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendLoadProgress {
    pub phase: BackendLoadPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl BackendLoadProgress {
    pub fn indeterminate(phase: BackendLoadPhase) -> Self {
        Self {
            phase,
            fraction: None,
            current: None,
            total: None,
            message: None,
        }
    }

    pub fn with_message(phase: BackendLoadPhase, message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            ..Self::indeterminate(phase)
        }
    }

    /// Rejects untrustworthy numeric evidence instead of presenting it: a
    /// non-finite or out-of-range fraction and inconsistent unit counts are
    /// dropped, degrading the observation to an indeterminate phase.
    pub fn sanitized(mut self) -> Self {
        if let (Some(current), Some(total)) = (self.current, self.total)
            && (total == 0 || current > total)
        {
            self.current = None;
            self.total = None;
            self.fraction = None;
        }
        if self.current.is_some() != self.total.is_some() {
            self.current = None;
            self.total = None;
        }
        if let Some(fraction) = self.fraction
            && !(fraction.is_finite() && (0.0..=1.0).contains(&fraction))
        {
            self.fraction = None;
        }
        if self.fraction.is_none()
            && let (Some(current), Some(total)) = (self.current, self.total)
        {
            #[allow(clippy::cast_precision_loss)]
            let fraction = current as f32 / total as f32;
            self.fraction = Some(fraction.clamp(0.0, 1.0));
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendStatus {
    /// Monotonic manager generation used by private control clients to
    /// correlate an admitted load with later status observations.
    #[serde(default)]
    pub generation: u64,
    pub lifecycle: BackendLifecycle,
    pub model_id: Option<ModelId>,
    pub engine_id: Option<String>,
    pub runtime_id: Option<RuntimeId>,
    pub runtime_version: Option<String>,
    pub runtime_variant: Option<String>,
    pub runtime_executable_sha256: Option<String>,
    pub process_id: Option<u32>,
    pub private_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_progress: Option<BackendLoadProgress>,
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
    #[error("the runtime is shutting down")]
    ShuttingDown,
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
    serve_profile: Option<ServeProfile>,
    _runtime_lease: RuntimeLease,
}

struct LoadAdmission {
    status: ControlStatus,
    completion: oneshot::Receiver<Result<ControlStatus, RuntimeError>>,
}

struct AdmittedLoad {
    model: norted_core::ModelArtifact,
    runtime_id: Option<RuntimeId>,
    profile: Option<ServeProfileName>,
    settings: LoadSettingsPatch,
    cancellation_epoch: u64,
    generation: u64,
}

struct ResolvedServeProfileSelection {
    profile: Option<ServeProfile>,
    local_load_profile: Option<ServeProfileName>,
    suppress_persisted_local_profile: bool,
}

async fn resolve_serve_profile(
    core: &ApplicationCore,
    model: &ModelArtifact,
    state: &ServeProfilesState,
    invocation: Option<&ServeProfileName>,
) -> Result<ResolvedServeProfileSelection, String> {
    let builder_profiles = core
        .snapshot()
        .await
        .models
        .into_iter()
        .filter_map(|candidate| {
            candidate
                .norted_package
                .and_then(|package| package.recommended_serve_profile)
        })
        .try_fold(
            BTreeMap::<String, ServeProfile>::new(),
            |mut profiles, profile| {
                if let Some(existing) = profiles.get(&profile.id)
                    && existing.content_hash() != profile.content_hash()
                {
                    return Err(format!(
                        "discovered Builder Serve Profiles reuse ID `{}` with different content",
                        profile.id
                    ));
                }
                profiles.entry(profile.id.clone()).or_insert(profile);
                Ok(profiles)
            },
        )?;

    let local = |name: &ServeProfileName| state.profiles.get(name).cloned();
    if let Some(name) = invocation {
        if name.as_str() == "none" {
            return Ok(ResolvedServeProfileSelection {
                profile: None,
                local_load_profile: None,
                suppress_persisted_local_profile: true,
            });
        }
        if let Some(profile) = local(name) {
            return Ok(ResolvedServeProfileSelection {
                profile: Some(profile),
                local_load_profile: Some(name.clone()),
                suppress_persisted_local_profile: true,
            });
        }
        if let Some(profile) = builder_profiles.get(name.as_str()) {
            return Ok(ResolvedServeProfileSelection {
                profile: Some(profile.clone()),
                local_load_profile: None,
                suppress_persisted_local_profile: true,
            });
        }
        return Err(format!("Serve Profile `{name}` does not exist"));
    }

    if state.raw_profile_models.contains(&model.id) {
        return Ok(ResolvedServeProfileSelection {
            profile: None,
            local_load_profile: None,
            suppress_persisted_local_profile: true,
        });
    }
    if let Some(name) = state.model_assignments.get(&model.id) {
        return Ok(ResolvedServeProfileSelection {
            profile: Some(
                local(name).ok_or_else(|| format!("Serve Profile `{name}` does not exist"))?,
            ),
            local_load_profile: None,
            suppress_persisted_local_profile: false,
        });
    }
    if let Some(profile_id) = state.builder_profile_assignments.get(&model.id) {
        return Ok(ResolvedServeProfileSelection {
            profile: Some(builder_profiles.get(profile_id).cloned().ok_or_else(|| {
                format!(
                    "assigned Builder Serve Profile `{profile_id}` is unavailable; its source package may have moved or been removed"
                )
            })?),
            local_load_profile: None,
            suppress_persisted_local_profile: true,
        });
    }
    Ok(ResolvedServeProfileSelection {
        profile: model
            .norted_package
            .as_ref()
            .and_then(|package| package.recommended_serve_profile.clone()),
        local_load_profile: None,
        suppress_persisted_local_profile: true,
    })
}

fn serve_profile_runtime_identity(
    selected: Option<&ServeProfile>,
    model: &norted_core::ModelRuntimeIdentity,
) -> ServeProfileRuntimeIdentity {
    let recommended = model
        .norted_package
        .as_ref()
        .and_then(|package| package.binding.recommended_serve_profile.as_ref());
    let status = match (recommended, selected) {
        (None, _) => BuilderRecommendationStatus::NotApplicable,
        (Some(_), None) => BuilderRecommendationStatus::Disabled,
        (Some(recommended), Some(selected))
            if recommended.id == selected.id
                && recommended.content_hash() == selected.content_hash() =>
        {
            BuilderRecommendationStatus::Canonical
        }
        (Some(_), Some(_)) => BuilderRecommendationStatus::Replaced,
    };
    let template = selected.and_then(|profile| profile.prompt.template.as_ref());
    ServeProfileRuntimeIdentity {
        profile_id: selected.map(|profile| profile.id.clone()),
        display_name: selected.map(|profile| profile.display_name.clone()),
        source: selected.map(|profile| profile.source),
        schema: selected.map(|profile| profile.schema.clone()),
        schema_version: selected.map(|profile| profile.schema_version),
        content_sha256: selected.map(|profile| {
            profile
                .source_profile_sha256
                .clone()
                .unwrap_or_else(|| profile.content_hash())
        }),
        builder_recommended_profile_id: recommended.map(|profile| profile.id.clone()),
        builder_recommendation_status: status,
        effective_template_identity: template.map(|template| template.identity.clone()),
        effective_template_sha256: template.map(|template| template.sha256.clone()),
    }
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
    load_progress: Option<BackendLoadProgress>,
    failure: Option<String>,
    provenance: Option<RuntimeProvenance>,
    generation: u64,
    notices: VecDeque<RuntimeNotice>,
}

impl ManagerState {
    /// Applies a load-progress update only when it still belongs to the
    /// current load generation; an observation from an older load can never
    /// overwrite a newer load operation.
    fn apply_load_progress(&mut self, generation: u64, progress: BackendLoadProgress) {
        if self.generation == generation && self.lifecycle == BackendLifecycle::Loading {
            self.load_progress = Some(progress.sanitized());
        }
    }
}

pub struct RuntimeManager {
    core: Arc<ApplicationCore>,
    registry: EngineRegistry,
    packs: Arc<RuntimePackManager>,
    supervisor: Arc<dyn ProcessSupervisor>,
    options: RuntimeManagerOptions,
    state: RwLock<ManagerState>,
    operation: Arc<Mutex<()>>,
    cancellation_epoch: AtomicU64,
    shutting_down: AtomicBool,
    load_profiles: ServeProfilesStore,
    #[cfg(test)]
    load_start_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
}

impl RuntimeManager {
    pub async fn initialize(
        core: Arc<ApplicationCore>,
        registry: EngineRegistry,
        packs: Arc<RuntimePackManager>,
        supervisor: Arc<dyn ProcessSupervisor>,
        options: RuntimeManagerOptions,
    ) -> Arc<Self> {
        let load_profiles = ServeProfilesStore::new(&core.paths);
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
                load_progress: None,
                failure: None,
                provenance: None,
                generation: 0,
                notices: VecDeque::new(),
            }),
            operation: Arc::new(Mutex::new(())),
            cancellation_epoch: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
            load_profiles,
            #[cfg(test)]
            load_start_gate: Mutex::new(None),
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
                generation: state.generation,
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
                load_progress: state.load_progress.clone(),
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
        profile: Option<ServeProfileName>,
        settings: LoadSettingsPatch,
    ) -> Result<ControlStatus, RuntimeError> {
        let admission = self
            .admit_load_with_settings(model_id, runtime_id, profile, settings)
            .await?;
        admission.completion.await.map_err(|_| {
            RuntimeError::Operation("server-owned load task ended without a result".to_owned())
        })?
    }

    pub async fn start_load(
        self: &Arc<Self>,
        model_id: ModelId,
    ) -> Result<ControlStatus, RuntimeError> {
        self.start_load_with_runtime(model_id, None).await
    }

    pub async fn start_load_with_runtime(
        self: &Arc<Self>,
        model_id: ModelId,
        runtime_id: Option<RuntimeId>,
    ) -> Result<ControlStatus, RuntimeError> {
        self.start_load_with_settings(model_id, runtime_id, None, LoadSettingsPatch::default())
            .await
    }

    /// Admits a load and returns after its generation has been authoritatively
    /// reserved as `Loading`. The load task remains owned by the manager after
    /// this future (or its caller) is dropped.
    pub async fn start_load_with_settings(
        self: &Arc<Self>,
        model_id: ModelId,
        runtime_id: Option<RuntimeId>,
        profile: Option<ServeProfileName>,
        settings: LoadSettingsPatch,
    ) -> Result<ControlStatus, RuntimeError> {
        let admission = self
            .admit_load_with_settings(model_id, runtime_id, profile, settings)
            .await?;
        Ok(admission.status)
    }

    async fn admit_load_with_settings(
        self: &Arc<Self>,
        model_id: ModelId,
        runtime_id: Option<RuntimeId>,
        profile: Option<ServeProfileName>,
        settings: LoadSettingsPatch,
    ) -> Result<LoadAdmission, RuntimeError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(RuntimeError::ShuttingDown);
        }
        // Capture before acquiring/validating so a concurrent unload that
        // begins anywhere during admission invalidates this reservation.
        let cancellation_epoch = self.cancellation_epoch.load(Ordering::Acquire);
        let operation = Arc::clone(&self.operation)
            .try_lock_owned()
            .map_err(|_| RuntimeError::Busy(self.current_lifecycle()))?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(RuntimeError::ShuttingDown);
        }
        self.ensure_idle_for_load().await?;
        let model = self
            .core
            .model(&model_id)
            .await
            .ok_or_else(|| RuntimeError::ModelNotFound(model_id.clone()))?;
        let generation = self.reserve_loading(&model_id, cancellation_epoch).await?;
        let status = self.status().await;
        let (completion_sender, completion) = oneshot::channel();
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let result = manager
                .load_admitted(
                    AdmittedLoad {
                        model,
                        runtime_id,
                        profile,
                        settings,
                        cancellation_epoch,
                        generation,
                    },
                    operation,
                )
                .await;
            let _ = completion_sender.send(result);
        });
        Ok(LoadAdmission { status, completion })
    }

    async fn reserve_loading(
        &self,
        model_id: &ModelId,
        cancellation_epoch: u64,
    ) -> Result<u64, RuntimeError> {
        if self.load_cancelled(cancellation_epoch) {
            return Err(if self.shutting_down.load(Ordering::Acquire) {
                RuntimeError::ShuttingDown
            } else {
                RuntimeError::Operation("model load was cancelled before admission".to_owned())
            });
        }
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
        state.cancel_loading = false;
        state.load_progress = Some(BackendLoadProgress::indeterminate(
            BackendLoadPhase::Starting,
        ));
        state.provenance = None;
        push_notice(
            &mut state,
            RuntimeNoticeLevel::Info,
            format!("Loading model {model_id}"),
        );
        Ok(state.generation)
    }

    fn current_lifecycle(&self) -> BackendLifecycle {
        self.state
            .try_read()
            .map_or(BackendLifecycle::Loading, |state| state.lifecycle)
    }

    async fn load_admitted(
        self: &Arc<Self>,
        admitted: AdmittedLoad,
        _operation: OwnedMutexGuard<()>,
    ) -> Result<ControlStatus, RuntimeError> {
        let AdmittedLoad {
            model,
            runtime_id,
            profile,
            settings: invocation_settings,
            cancellation_epoch,
            generation,
        } = admitted;
        let model_id = model.id.clone();
        #[cfg(test)]
        if let Some(gate) = self.load_start_gate.lock().await.clone() {
            gate.notified().await;
        }
        if self.load_cancelled(cancellation_epoch) {
            let detail = "model load was cancelled".to_owned();
            self.fail_loading(generation, detail.clone(), None).await;
            return Err(RuntimeError::Operation(detail));
        }

        self.set_load_progress(
            generation,
            BackendLoadProgress::indeterminate(BackendLoadPhase::SelectingRuntime),
        )
        .await;
        let profile_state = match self.load_profiles.read().await {
            Ok(state) => state,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::Operation(error.to_string()));
            }
        };
        let profile_selection =
            match resolve_serve_profile(&self.core, &model, &profile_state, profile.as_ref()).await
            {
                Ok(selection) => selection,
                Err(error) => {
                    self.fail_loading(generation, error.clone(), None).await;
                    return Err(RuntimeError::Operation(error));
                }
            };
        let serve_profile = profile_selection.profile;
        if let Some(profile) = &serve_profile
            && let Err(reason) = profile.basic_applicability(&model)
        {
            self.fail_loading(generation, reason.clone(), None).await;
            return Err(RuntimeError::Incompatible {
                model_id: model_id.clone(),
                reason,
            });
        }
        let (adapter, selection) = match self
            .select_runtime(&model, runtime_id.as_ref(), serve_profile.as_ref())
            .await
        {
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
        self.set_load_progress(
            generation,
            BackendLoadProgress::indeterminate(BackendLoadPhase::ResolvingSettings),
        )
        .await;
        let mut load_state = profile_state.clone();
        if profile_selection.suppress_persisted_local_profile {
            load_state.model_assignments.remove(&model_id);
        }
        let mut resolved_load_settings = match load_state.resolve(
            &model_id,
            &engine_id,
            profile_selection.local_load_profile.as_ref(),
            &invocation_settings,
            &self.core.paths.data_dir,
        ) {
            Ok(settings) => settings,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::Operation(error.to_string()));
            }
        };
        if let Err(error) = norted_core::apply_serve_profile_load_policy(
            serve_profile.as_ref(),
            &mut resolved_load_settings,
        ) {
            self.fail_loading(generation, error.clone(), None).await;
            return Err(RuntimeError::Operation(error));
        }
        let host = self.packs.host_capabilities().await;
        let load_settings_schema = match adapter
            .load_settings_schema(&selection.runtime, &model, &host, serve_profile.as_ref())
            .await
        {
            Ok(schema) => schema,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::StartupFailed(error.to_string()));
            }
        };
        let selected_runtime_id = selection.runtime.manifest.runtime_id.clone();
        self.set_load_progress(
            generation,
            BackendLoadProgress::indeterminate(BackendLoadPhase::AcquiringRuntime),
        )
        .await;
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
        self.set_load_progress(
            generation,
            BackendLoadProgress::indeterminate(BackendLoadPhase::PreparingModel),
        )
        .await;
        let (progress, progress_pump) = self.load_progress_reporter(generation);
        let prepared_model_result = adapter
            .prepare_model_input_with_progress(&model, Arc::clone(&progress))
            .await;
        drop(progress);
        let _ = progress_pump.await;
        let prepared_model = match prepared_model_result {
            Ok(model) => model,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::StartupFailed(error.to_string()));
            }
        };
        self.set_load_progress(
            generation,
            BackendLoadProgress::indeterminate(BackendLoadPhase::PreparingLaunch),
        )
        .await;
        let launch_attempts = match adapter
            .build_launch_attempts(LaunchRequest {
                model: prepared_model,
                runtime: selection.runtime.clone(),
                accelerator: selection.accelerator.clone(),
                backend_address,
                load_settings: resolved_load_settings,
                load_settings_schema,
                serve_profile: serve_profile.clone(),
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
        let (
            process,
            endpoint,
            exit,
            mut startup_observation,
            effective_generation_settings,
            selected_serve_profile,
        ) = loop {
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
            self.set_load_progress(
                generation,
                adapter
                    .prepare_launch_progress(&launch_spec)
                    .unwrap_or_else(|| {
                        BackendLoadProgress::indeterminate(BackendLoadPhase::PreparingLaunch)
                    }),
            )
            .await;
            let (progress, progress_pump) = self.load_progress_reporter(generation);
            let preparation_result = adapter
                .prepare_launch_attempt_with_progress(&launch_spec, Arc::clone(&progress))
                .await;
            drop(progress);
            let _ = progress_pump.await;
            if let Err(error) = preparation_result {
                cleanup_temporary_launch_files(&launch_spec.temporary_files).await;
                cleanup_pending_launch_files(&launch_attempts).await;
                adapter
                    .clear_launch_state(launch_spec.endpoint.as_deref())
                    .await;
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::StartupFailed(error.to_string()));
            }
            if self.load_cancelled(cancellation_epoch) {
                cleanup_temporary_launch_files(&launch_spec.temporary_files).await;
                cleanup_pending_launch_files(&launch_attempts).await;
                adapter
                    .clear_launch_state(launch_spec.endpoint.as_deref())
                    .await;
                let detail = "model load was cancelled".to_owned();
                self.fail_loading(generation, detail.clone(), None).await;
                return Err(RuntimeError::Operation(detail));
            }

            let installation = launch_spec.installation.clone();
            let selected_runtime = launch_spec.runtime.clone();
            let selected_accelerator = launch_spec.accelerator.clone();
            let model_identity = launch_spec.model.runtime_identity();
            let normalized_settings = launch_spec.normalized_settings.clone();
            let load_settings = launch_spec.load_settings.clone();
            let selected_serve_profile = launch_spec.serve_profile.clone();
            let native_arguments = launch_spec.native_arguments.clone();
            let inherits_parent_environment = launch_spec.inherits_parent_environment;
            let native_environment = environment_provenance(
                &launch_spec.environment,
                &launch_spec.environment_remove,
                inherits_parent_environment,
            );
            let temporary_files = launch_spec.temporary_files.clone();
            let launch_endpoint = launch_spec.endpoint.clone();
            self.set_load_progress(
                generation,
                BackendLoadProgress::indeterminate(BackendLoadPhase::SpawningBackend),
            )
            .await;
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
            let serve_profile_identity =
                serve_profile_runtime_identity(selected_serve_profile.as_ref(), &model_identity);
            let provenance = RuntimeProvenance {
                model: model_identity,
                runtime: selected_runtime.manifest.clone(),
                runtime_entrypoint: selected_runtime.entrypoint_path(),
                selection_source: selection.source,
                accelerator: selected_accelerator,
                installation,
                serve_profile: serve_profile_identity,
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
            self.set_load_progress(
                generation,
                BackendLoadProgress::indeterminate(BackendLoadPhase::LoadingModel),
            )
            .await;
            if let Err(error) = self
                .wait_for_readiness(adapter.as_ref(), &process, &mut exit, generation)
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
            self.set_load_progress(
                generation,
                BackendLoadProgress::indeterminate(BackendLoadPhase::VerifyingStartup),
            )
            .await;
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
                    break (
                        process,
                        endpoint,
                        exit,
                        observation,
                        settings,
                        selected_serve_profile,
                    );
                }
                StartupObservation::RetryContextCapacity {
                    kv_mode,
                    observed_context,
                    minimum_context,
                } => {
                    self.set_load_progress(
                        generation,
                        BackendLoadProgress::with_message(
                            BackendLoadPhase::AllocatingContext,
                            retry_context_capacity_message(
                                &kv_mode,
                                observed_context,
                                minimum_context,
                            ),
                        ),
                    )
                    .await;
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
                "serve_profile_kv_attempts".to_owned(),
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
            state.load_progress = None;
            if let Some(provenance) = state.provenance.as_mut() {
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
                serve_profile: selected_serve_profile,
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
        state.load_progress = None;
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
        let (adapter, endpoint, backend_generation_settings, serve_profile) =
            self.inference_target(&request.model_id).await?;
        validate_profile_generation_overrides(
            serve_profile.as_ref(),
            &request.generation_settings,
            adapter.identity().id.as_str(),
        )?;
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
        let (adapter, endpoint, backend_generation_settings, serve_profile) =
            self.inference_target(&request.model_id).await?;
        validate_profile_generation_overrides(
            serve_profile.as_ref(),
            &request.generation_settings,
            adapter.identity().id.as_str(),
        )?;
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

    async fn set_load_progress(&self, generation: u64, progress: BackendLoadProgress) {
        self.state
            .write()
            .await
            .apply_load_progress(generation, progress);
    }

    fn load_progress_reporter(
        self: &Arc<Self>,
        generation: u64,
    ) -> (LoadProgressReporter, tokio::task::JoinHandle<()>) {
        let (sender, mut receiver) = tokio::sync::watch::channel(None);
        let reporter: LoadProgressReporter = Arc::new(move |progress| {
            sender.send_replace(Some(progress));
        });
        let manager = Arc::downgrade(self);
        let pump = tokio::spawn(async move {
            loop {
                match receiver.changed().await {
                    Ok(()) => {}
                    Err(_) if !receiver.has_changed().unwrap_or(false) => break,
                    Err(_) => {}
                }
                let Some(progress) = receiver.borrow_and_update().clone() else {
                    continue;
                };
                let Some(manager) = manager.upgrade() else {
                    break;
                };
                manager.set_load_progress(generation, progress).await;
            }
        });
        (reporter, pump)
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
            state.load_progress = None;
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
        serve_profile: Option<&ServeProfile>,
    ) -> Result<(Arc<dyn EngineAdapter>, RuntimeSelection), RuntimeError> {
        let selection = self
            .packs
            .resolve_with_profile(model, explicit_runtime, serve_profile)
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
        generation: u64,
    ) -> Result<(), RuntimeError> {
        let deadline = tokio::time::Instant::now() + self.options.startup_timeout;
        let mut published_progress: Option<BackendLoadProgress> = None;
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
            // Progress reporting is UX evidence only: an unreadable tail or an
            // unrecognized output format never fails a valid startup.
            if let Ok(stderr_tail) = self.supervisor.stderr_tail(process).await
                && let Some(observed) = adapter.startup_progress(&stderr_tail)
            {
                let observed = observed.sanitized();
                if published_progress.as_ref() != Some(&observed) {
                    published_progress = Some(observed.clone());
                    self.set_load_progress(generation, observed).await;
                }
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
        let failed_phase = state
            .load_progress
            .take()
            .map(|progress| progress.phase)
            .filter(|_| {
                !state.cancel_loading
                    && state.lifecycle == BackendLifecycle::Loading
                    && detail != "model load was cancelled"
            });
        let detail = match failed_phase {
            Some(phase) => format!("failed while {}: {detail}", phase.failure_label()),
            None => detail,
        };
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
    ) -> Result<
        (
            Arc<dyn EngineAdapter>,
            String,
            EffectiveGenerationSettings,
            Option<ServeProfile>,
        ),
        RuntimeError,
    > {
        if self.core.model(model_id).await.is_none() {
            return Err(RuntimeError::ModelNotFound(model_id.clone()));
        }
        let state = self.state.read().await;
        match (&state.lifecycle, &state.active) {
            (BackendLifecycle::Running, Some(active)) if &active.model_id == model_id => Ok((
                Arc::clone(&active.adapter),
                active.endpoint.clone(),
                active.effective_generation_settings,
                active.serve_profile.clone(),
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
        state.load_progress = None;
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
                generation: state.generation,
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
                load_progress: state.load_progress.clone(),
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

fn validate_profile_generation_overrides(
    profile: Option<&ServeProfile>,
    settings: &GenerationSettingsPatch,
    engine_id: &str,
) -> Result<(), RuntimeError> {
    if settings.reasoning_effort.is_some()
        && (engine_id != "q27"
            || !profile.is_some_and(|profile| {
                profile.prompt.mode == norted_core::PromptMode::ExternalTemplate
                    && profile.prompt.delivery == norted_core::PromptDelivery::RawCompletions
            }))
    {
        return Err(RuntimeError::InvalidGenerationSettings(
            "request-time reasoning_effort requires a selected q27 external-template Serve Profile"
                .to_owned(),
        ));
    }
    let Some(profile) = profile else {
        return Ok(());
    };
    for (name, present) in [
        ("temperature", settings.temperature.is_some()),
        ("top_p", settings.top_p.is_some()),
        ("reasoning_effort", settings.reasoning_effort.is_some()),
    ] {
        if present && !profile.generation.allows_override(name) {
            return Err(RuntimeError::InvalidGenerationSettings(format!(
                "Serve Profile `{}` does not allow request-time `{name}` overrides",
                profile.display_name
            )));
        }
    }
    Ok(())
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

fn retry_context_capacity_message(
    kv_mode: &str,
    observed_context: u64,
    minimum_context: u64,
) -> String {
    format!(
        "KV mode {kv_mode} served {observed_context} of the required {minimum_context} context tokens; retrying with the next policy KV mode"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ManagerFixture {
        _temporary: tempfile::TempDir,
        manager: Arc<RuntimeManager>,
        model_id: ModelId,
    }

    async fn manager_fixture() -> ManagerFixture {
        let temporary = tempfile::tempdir().expect("temporary manager fixture");
        let root = temporary.path();
        let model_dir = root.join("models");
        std::fs::create_dir_all(&model_dir).expect("model directory");
        std::fs::write(model_dir.join("fixture.gguf"), b"fixture").expect("model fixture");
        let paths = norted_core::AppPaths {
            config_dir: root.join("config"),
            config_file: root.join("config/config.toml"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
            runtimes_dir: root.join("data/runtimes"),
            runtime_cache_dir: root.join("cache/runtime-packs"),
            runtime_selections_file: root.join("data/runtime-selections.json"),
            serve_profiles_file: root.join("data/serve-profiles.json"),
            serve_profiles_lock_file: root.join("data/.serve-profiles.lock"),
        };
        paths.ensure_required().expect("fixture paths");
        std::fs::write(
            &paths.config_file,
            format!(
                "version = 1\n\n[models]\npaths = [{}]\n",
                serde_json::to_string(&model_dir).expect("model path string")
            ),
        )
        .expect("fixture config");
        let core = ApplicationCore::load_from_paths(paths.clone())
            .await
            .expect("fixture core");
        core.refresh_models().await.expect("model discovery");
        let model_id = core
            .snapshot()
            .await
            .models
            .into_iter()
            .next()
            .expect("discovered model")
            .id;
        let registry = EngineRegistry::default();
        let packs = RuntimePackManager::new(
            &paths,
            registry.clone(),
            Vec::<Arc<dyn crate::RuntimeCatalogProvider>>::new(),
        )
        .expect("runtime packs");
        let manager = RuntimeManager::initialize(
            core,
            registry,
            packs,
            Arc::new(crate::TokioProcessSupervisor::default()),
            RuntimeManagerOptions::default(),
        )
        .await;
        ManagerFixture {
            _temporary: temporary,
            manager,
            model_id,
        }
    }

    async fn wait_for_lifecycle(
        manager: &RuntimeManager,
        expected: BackendLifecycle,
    ) -> ControlStatus {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status = manager.status().await;
                if status.backend.lifecycle == expected {
                    return status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("lifecycle transition")
    }

    fn empty_state() -> ManagerState {
        ManagerState {
            public_endpoint: None,
            engines: BTreeMap::new(),
            lifecycle: BackendLifecycle::Loading,
            model_id: None,
            engine_id: None,
            runtime_id: None,
            active: None,
            loading_process: None,
            loading_runtime_lease: None,
            cancel_loading: false,
            load_progress: None,
            failure: None,
            provenance: None,
            generation: 7,
            notices: VecDeque::new(),
        }
    }

    #[tokio::test]
    async fn admission_returns_after_loading_generation_is_reserved() {
        let fixture = manager_fixture().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        *fixture.manager.load_start_gate.lock().await = Some(Arc::clone(&gate));

        let admitted = fixture
            .manager
            .start_load(fixture.model_id.clone())
            .await
            .expect("load admission");
        assert_eq!(admitted.backend.lifecycle, BackendLifecycle::Loading);
        assert_eq!(admitted.backend.model_id.as_ref(), Some(&fixture.model_id));
        assert_eq!(
            fixture.manager.status().await.backend.generation,
            admitted.backend.generation
        );

        gate.notify_one();
        let _ = wait_for_lifecycle(&fixture.manager, BackendLifecycle::Failed).await;
    }

    #[tokio::test]
    async fn simultaneous_second_admission_is_busy_instead_of_queued() {
        let fixture = manager_fixture().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        *fixture.manager.load_start_gate.lock().await = Some(Arc::clone(&gate));
        fixture
            .manager
            .start_load(fixture.model_id.clone())
            .await
            .expect("first admission");

        let error = fixture
            .manager
            .start_load(fixture.model_id.clone())
            .await
            .expect_err("second admission must be rejected");
        assert!(matches!(
            error,
            RuntimeError::Busy(BackendLifecycle::Loading)
        ));

        gate.notify_one();
        let _ = wait_for_lifecycle(&fixture.manager, BackendLifecycle::Failed).await;
    }

    #[tokio::test]
    async fn unload_epoch_change_prevents_a_stale_admission_reservation() {
        let fixture = manager_fixture().await;
        let stale_epoch = fixture.manager.cancellation_epoch.load(Ordering::Acquire);
        fixture
            .manager
            .cancellation_epoch
            .fetch_add(1, Ordering::AcqRel);

        let error = fixture
            .manager
            .reserve_loading(&fixture.model_id, stale_epoch)
            .await
            .expect_err("stale admission epoch");
        assert!(error.to_string().contains("cancelled before admission"));
        assert_eq!(
            fixture.manager.status().await.backend.lifecycle,
            BackendLifecycle::Stopped
        );
    }

    #[tokio::test]
    async fn admitted_background_failure_is_authoritative() {
        let fixture = manager_fixture().await;
        let admitted = fixture
            .manager
            .start_load(fixture.model_id.clone())
            .await
            .expect("load admission");
        let failed = wait_for_lifecycle(&fixture.manager, BackendLifecycle::Failed).await;

        assert_eq!(failed.backend.generation, admitted.backend.generation);
        assert!(
            failed
                .backend
                .failure
                .as_deref()
                .is_some_and(|failure| failure.contains("compatible installed engine"))
        );
    }

    #[tokio::test]
    async fn unload_cancels_an_admitted_background_load() {
        let fixture = manager_fixture().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        *fixture.manager.load_start_gate.lock().await = Some(Arc::clone(&gate));
        fixture
            .manager
            .start_load(fixture.model_id.clone())
            .await
            .expect("load admission");

        let manager = Arc::clone(&fixture.manager);
        let unload = tokio::spawn(async move { manager.unload().await });
        let _ = wait_for_lifecycle(&fixture.manager, BackendLifecycle::Stopping).await;
        gate.notify_one();
        let stopped = unload.await.expect("unload task").expect("unload");
        assert_eq!(stopped.backend.lifecycle, BackendLifecycle::Stopped);
        assert!(stopped.backend.model_id.is_none());
    }

    #[tokio::test]
    async fn shutdown_cleans_up_an_admitted_background_load() {
        let fixture = manager_fixture().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        *fixture.manager.load_start_gate.lock().await = Some(Arc::clone(&gate));
        fixture
            .manager
            .start_load(fixture.model_id.clone())
            .await
            .expect("load admission");

        let manager = Arc::clone(&fixture.manager);
        let shutdown = tokio::spawn(async move { manager.shutdown().await });
        let _ = wait_for_lifecycle(&fixture.manager, BackendLifecycle::Stopping).await;
        gate.notify_one();
        shutdown.await.expect("shutdown task");
        assert_eq!(
            fixture.manager.status().await.backend.lifecycle,
            BackendLifecycle::Stopped
        );
    }

    #[test]
    fn apply_load_progress_updates_the_current_generation() {
        let mut state = empty_state();
        state.apply_load_progress(
            7,
            BackendLoadProgress::indeterminate(BackendLoadPhase::SelectingRuntime),
        );
        let progress = state.load_progress.expect("progress was set");
        assert_eq!(progress.phase, BackendLoadPhase::SelectingRuntime);
        assert_eq!(progress.fraction, None);
    }

    #[test]
    fn an_old_load_generation_cannot_overwrite_newer_load_progress() {
        let mut state = empty_state();
        state.apply_load_progress(
            7,
            BackendLoadProgress::indeterminate(BackendLoadPhase::LoadingModel),
        );
        // A newer load started under generation 8.
        state.generation = 8;
        state.apply_load_progress(
            7,
            BackendLoadProgress::indeterminate(BackendLoadPhase::SpawningBackend),
        );
        let progress = state.load_progress.expect("progress is unchanged");
        assert_eq!(
            progress.phase,
            BackendLoadPhase::LoadingModel,
            "stale generation must not overwrite newer progress"
        );
    }

    #[tokio::test]
    async fn coalesced_hash_progress_from_an_old_generation_is_ignored() {
        let fixture = manager_fixture().await;
        {
            let mut state = fixture.manager.state.write().await;
            state.generation = 8;
            state.lifecycle = BackendLifecycle::Loading;
            state.load_progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::SelectingRuntime,
                "newer load",
            ));
        }
        let (reporter, pump) = fixture.manager.load_progress_reporter(7);
        reporter(BackendLoadProgress {
            phase: BackendLoadPhase::PreparingModel,
            fraction: None,
            current: Some(64),
            total: Some(128),
            message: Some("stale package hash".to_owned()),
        });
        drop(reporter);
        pump.await.expect("progress pump");

        let state = fixture.manager.state.read().await;
        let progress = state.load_progress.as_ref().expect("new load progress");
        assert_eq!(progress.phase, BackendLoadPhase::SelectingRuntime);
        assert_eq!(progress.message.as_deref(), Some("newer load"));
    }

    #[test]
    fn apply_load_progress_is_ignored_once_loading_is_no_longer_active() {
        let mut state = empty_state();
        state.lifecycle = BackendLifecycle::Running;
        state.apply_load_progress(
            7,
            BackendLoadProgress::indeterminate(BackendLoadPhase::VerifyingStartup),
        );
        assert!(
            state.load_progress.is_none(),
            "progress is not attached once the backend is Running"
        );

        let mut state = empty_state();
        state.lifecycle = BackendLifecycle::Failed;
        state.apply_load_progress(
            7,
            BackendLoadProgress::indeterminate(BackendLoadPhase::VerifyingStartup),
        );
        assert!(
            state.load_progress.is_none(),
            "progress is not attached once the backend is Failed"
        );
    }

    #[test]
    fn apply_load_progress_sanitizes_untrustworthy_adapter_values() {
        let mut state = empty_state();
        state.apply_load_progress(
            7,
            BackendLoadProgress {
                phase: BackendLoadPhase::LoadingModel,
                fraction: Some(1.5),
                current: Some(300),
                total: Some(200),
                message: None,
            },
        );
        let progress = state.load_progress.expect("progress was sanitized");
        assert_eq!(progress.fraction, None);
        assert_eq!(progress.current, None);
        assert_eq!(progress.total, None);
    }

    #[test]
    fn pre_launch_preparation_precedes_the_spawning_phase() {
        let source = include_str!("manager.rs");
        let launch_loop = source
            .split_once("let mut context_attempts = Vec::new();")
            .expect("launch loop")
            .1
            .split_once("let process = match self")
            .expect("supervisor spawn boundary")
            .0;
        let preparation = launch_loop
            .find(".prepare_launch_attempt_with_progress(&launch_spec")
            .expect("adapter preparation");
        let spawning = launch_loop
            .find("BackendLoadPhase::SpawningBackend")
            .expect("spawning phase");
        assert!(
            preparation < spawning,
            "SpawningBackend must not be published during adapter preparation"
        );
        assert!(
            launch_loop[preparation..spawning].contains("self.load_cancelled(cancellation_epoch)"),
            "cancellation during final preparation must prevent process creation"
        );
    }

    #[test]
    fn context_capacity_retry_names_the_attempted_kv_mode() {
        let message = retry_context_capacity_message("fp8", 180_000, 200_000);
        assert_eq!(
            message,
            "KV mode fp8 served 180000 of the required 200000 context tokens; retrying with the next policy KV mode"
        );
        assert!(!message.contains("next KV mode: fp8"));
    }
}
