use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::Stream;
use norted_core::{
    ApplicationCore, EnvironmentVariableProvenance, ModelId, ModelProfile, ModelProfileId,
    ModelProfileRuntimeIdentity, ModelProfilesStore, ModelRole, NativeArgumentProvenance,
    ProcessIdentity, RuntimeId, RuntimeProvenance, RuntimeSelection, SettingsPatch,
    SettingsProvenance, SettingsStore,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Notify, OwnedMutexGuard, RwLock, oneshot};

use crate::{
    EffectiveGenerationSettings, EngineAdapter, EngineError, EngineIdentity, EngineProbe,
    EngineRegistry, InferenceRequest, InstallationState, LaunchRequest, LoadProgressReporter,
    ProcessDescriptor, ProcessExit, ProcessSupervisor, RoutedInferenceOutput,
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
            Self::ResolvingSettings => "Resolving settings",
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
            Self::ResolvingSettings => "resolving settings",
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
    pub model_profile_id: ModelProfileId,
    pub model_id: ModelId,
    /// Persistent/default role from the Model Profile, never a request override.
    pub role: ModelRole,
    pub residency: BackendResidency,
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
    pub active_request_count: usize,
    pub primary_lease_count: usize,
    pub last_used_unix: i64,
    pub retiring: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendResidency {
    Jit,
    Pinned,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct InferenceRoutingContext {
    pub session_id: String,
    pub role: Option<ModelRole>,
}

impl Default for InferenceRoutingContext {
    fn default() -> Self {
        Self {
            session_id: "default".to_owned(),
            role: None,
        }
    }
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
    pub running_backend_count: usize,
    pub engines: Vec<EngineStatus>,
    pub backends: Vec<BackendStatus>,
    pub recent_events: Vec<RuntimeNotice>,
}

impl ControlStatus {
    pub fn backend(&self, profile_id: &ModelProfileId) -> Option<&BackendStatus> {
        self.backends
            .iter()
            .find(|backend| &backend.model_profile_id == profile_id)
    }

    pub fn newest_backend(&self) -> Option<&BackendStatus> {
        self.backends
            .iter()
            .max_by_key(|backend| backend.generation)
    }

    pub fn loading_backend(&self) -> Option<&BackendStatus> {
        self.backends
            .iter()
            .filter(|backend| backend.lifecycle.is_loading())
            .max_by_key(|backend| backend.generation)
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeManagerOptions {
    pub startup_timeout: Duration,
    pub health_poll_interval: Duration,
    pub jit_enabled: bool,
    pub primary_idle_ttl: Duration,
    pub auxiliary_idle_ttl: Duration,
    pub max_idle_auxiliary_backends: usize,
}

impl Default for RuntimeManagerOptions {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(5 * 60),
            health_poll_interval: Duration::from_millis(250),
            jit_enabled: true,
            primary_idle_ttl: Duration::from_secs(3600),
            auxiliary_idle_ttl: Duration::from_secs(300),
            max_idle_auxiliary_backends: 2,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("Model Profile `{0}` does not exist")]
    ModelProfileNotFound(ModelProfileId),
    #[error(
        "Model Profile `{profile_id}` is unavailable because bound model `{model_id}` is missing"
    )]
    BoundModelMissing {
        profile_id: ModelProfileId,
        model_id: ModelId,
    },
    #[error("model `{0}` does not exist in the discovered registry")]
    ModelNotFound(ModelId),
    #[error("Model Profile `{0}` is not loaded")]
    ModelProfileNotLoaded(ModelProfileId),
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
    #[error(
        "auxiliary Model Profile `{profile_id}` could not be loaded without evicting a leased primary: {reason}"
    )]
    AuxiliaryLoadFailed {
        profile_id: ModelProfileId,
        reason: String,
    },
    #[error("runtime operation failed: {0}")]
    Operation(String),
}

struct RunningBackend {
    adapter: Arc<dyn EngineAdapter>,
    process: ProcessDescriptor,
    endpoint: String,
    effective_generation_settings: EffectiveGenerationSettings,
    settings: norted_core::ResolvedSettings,
    settings_schema: norted_core::SettingsSchema,
    _runtime_lease: RuntimeLease,
}

struct BackendActivity {
    active_requests: AtomicUsize,
    last_used_unix: AtomicI64,
    drained: Notify,
}

impl BackendActivity {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active_requests: AtomicUsize::new(0),
            last_used_unix: AtomicI64::new(unix_timestamp()),
            drained: Notify::new(),
        })
    }
}

struct InferenceLease {
    activity: Arc<BackendActivity>,
}

struct InferenceTarget {
    adapter: Arc<dyn EngineAdapter>,
    endpoint: String,
    generation_settings: EffectiveGenerationSettings,
    settings: norted_core::ResolvedSettings,
    settings_schema: norted_core::SettingsSchema,
    lease: InferenceLease,
}

impl InferenceLease {
    fn acquire(activity: Arc<BackendActivity>) -> Self {
        activity.active_requests.fetch_add(1, Ordering::AcqRel);
        activity
            .last_used_unix
            .store(unix_timestamp(), Ordering::Release);
        Self { activity }
    }
}

impl Drop for InferenceLease {
    fn drop(&mut self) {
        self.activity
            .last_used_unix
            .store(unix_timestamp(), Ordering::Release);
        if self.activity.active_requests.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.activity.drained.notify_one();
        }
    }
}

struct LeasedInferenceStream {
    inner: crate::InferenceStream,
    lease: Option<InferenceLease>,
}

impl Stream for LeasedInferenceStream {
    type Item = Result<crate::InferenceEvent, EngineError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let result = this.inner.as_mut().poll_next(cx);
        if matches!(&result, std::task::Poll::Ready(None | Some(Err(_)))) {
            this.lease.take();
        }
        result
    }
}

struct LoadAdmission {
    status: ControlStatus,
    completion: oneshot::Receiver<Result<ControlStatus, RuntimeError>>,
}

struct AdmittedLoad {
    model_profile: ModelProfile,
    model: norted_core::ModelArtifact,
    runtime_id: Option<RuntimeId>,
    settings: SettingsPatch,
    cancellation_epoch: u64,
    generation: u64,
    profile_role: ModelRole,
    residency: BackendResidency,
}

#[derive(Clone, Copy)]
enum LoadIntent {
    Manual,
}

struct ManagedBackend {
    lifecycle: BackendLifecycle,
    model_profile_id: ModelProfileId,
    model_id: ModelId,
    profile_role: ModelRole,
    residency: BackendResidency,
    engine_id: Option<String>,
    runtime_id: Option<RuntimeId>,
    running: Option<RunningBackend>,
    loading_process: Option<ProcessDescriptor>,
    loading_runtime_lease: Option<RuntimeLease>,
    cancel_loading: bool,
    load_progress: Option<BackendLoadProgress>,
    failure: Option<String>,
    provenance: Option<RuntimeProvenance>,
    generation: u64,
    activity: Arc<BackendActivity>,
    retiring: bool,
}

impl ManagedBackend {
    fn apply_load_progress(&mut self, generation: u64, progress: BackendLoadProgress) {
        if self.generation == generation && self.lifecycle == BackendLifecycle::Loading {
            self.load_progress = Some(progress.sanitized());
        }
    }
}

struct SessionState {
    primary: Option<ModelProfileId>,
    last_used_unix: i64,
}

struct ManagerState {
    public_endpoint: Option<String>,
    engines: BTreeMap<String, EngineStatus>,
    backends: BTreeMap<ModelProfileId, ManagedBackend>,
    sessions: BTreeMap<String, SessionState>,
    next_generation: u64,
    notices: VecDeque<RuntimeNotice>,
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
    reaper: Mutex<Option<tokio::task::JoinHandle<()>>>,
    settings: SettingsStore,
    model_profiles: ModelProfilesStore,
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
        let settings = SettingsStore::new(&core.paths);
        let model_profiles = ModelProfilesStore::new(&core.paths);
        let manager = Arc::new(Self {
            core,
            registry,
            packs,
            supervisor,
            options,
            state: RwLock::new(ManagerState {
                public_endpoint: None,
                engines: BTreeMap::new(),
                backends: BTreeMap::new(),
                sessions: BTreeMap::new(),
                next_generation: 0,
                notices: VecDeque::new(),
            }),
            operation: Arc::new(Mutex::new(())),
            cancellation_epoch: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
            reaper: Mutex::new(None),
            settings,
            model_profiles,
            #[cfg(test)]
            load_start_gate: Mutex::new(None),
        });
        manager.refresh_engine_probes().await;
        let weak = Arc::downgrade(&manager);
        *manager.reaper.lock().await = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            loop {
                interval.tick().await;
                let Some(manager) = weak.upgrade() else { break };
                if manager.shutting_down.load(Ordering::Acquire) {
                    break;
                }
                manager.reap_idle().await;
            }
        }));
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
        self.status_from_state(&state)
    }

    pub async fn load(
        self: &Arc<Self>,
        profile_id: ModelProfileId,
    ) -> Result<ControlStatus, RuntimeError> {
        self.load_with_runtime(profile_id, None).await
    }

    pub async fn load_with_runtime(
        self: &Arc<Self>,
        profile_id: ModelProfileId,
        runtime_id: Option<RuntimeId>,
    ) -> Result<ControlStatus, RuntimeError> {
        self.load_with_settings(profile_id, runtime_id, SettingsPatch::default())
            .await
    }

    pub async fn load_with_settings(
        self: &Arc<Self>,
        profile_id: ModelProfileId,
        runtime_id: Option<RuntimeId>,
        settings: SettingsPatch,
    ) -> Result<ControlStatus, RuntimeError> {
        let admission = self
            .admit_load_with_settings(profile_id, runtime_id, settings, LoadIntent::Manual)
            .await?;
        admission.completion.await.map_err(|_| {
            RuntimeError::Operation("server-owned load task ended without a result".to_owned())
        })?
    }

    pub async fn start_load(
        self: &Arc<Self>,
        profile_id: ModelProfileId,
    ) -> Result<ControlStatus, RuntimeError> {
        self.start_load_with_runtime(profile_id, None).await
    }

    pub async fn start_load_with_runtime(
        self: &Arc<Self>,
        profile_id: ModelProfileId,
        runtime_id: Option<RuntimeId>,
    ) -> Result<ControlStatus, RuntimeError> {
        self.start_load_with_settings(profile_id, runtime_id, SettingsPatch::default())
            .await
    }

    /// Admits a load and returns after its generation has been authoritatively
    /// reserved as `Loading`. The load task remains owned by the manager after
    /// this future (or its caller) is dropped.
    pub async fn start_load_with_settings(
        self: &Arc<Self>,
        profile_id: ModelProfileId,
        runtime_id: Option<RuntimeId>,
        settings: SettingsPatch,
    ) -> Result<ControlStatus, RuntimeError> {
        let admission = self
            .admit_load_with_settings(profile_id, runtime_id, settings, LoadIntent::Manual)
            .await?;
        Ok(admission.status)
    }

    async fn admit_load_with_settings(
        self: &Arc<Self>,
        profile_id: ModelProfileId,
        runtime_id: Option<RuntimeId>,
        settings: SettingsPatch,
        intent: LoadIntent,
    ) -> Result<LoadAdmission, RuntimeError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(RuntimeError::ShuttingDown);
        }
        let mut operation = Arc::clone(&self.operation)
            .try_lock_owned()
            .map_err(|_| RuntimeError::Busy(self.current_lifecycle()))?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(RuntimeError::ShuttingDown);
        }
        let profiles = self
            .model_profiles
            .read()
            .await
            .map_err(|error| RuntimeError::Operation(error.to_string()))?;
        let model_profile = profiles
            .profiles
            .get(&profile_id)
            .cloned()
            .ok_or_else(|| RuntimeError::ModelProfileNotFound(profile_id.clone()))?;
        let model = self
            .core
            .model(&model_profile.model_id)
            .await
            .ok_or_else(|| RuntimeError::BoundModelMissing {
                profile_id: profile_id.clone(),
                model_id: model_profile.model_id.clone(),
            })?;
        let expected_profile_hash = model_profile.content_hash();
        let failed_backend = {
            let mut state = self.state.write().await;
            if let Some(backend) = state.backends.get_mut(&profile_id) {
                match backend.lifecycle {
                    BackendLifecycle::Running => {
                        let reusable = settings.is_empty()
                            && runtime_id
                                .as_ref()
                                .is_none_or(|runtime| backend.runtime_id.as_ref() == Some(runtime))
                            && backend.provenance.as_ref().is_some_and(|provenance| {
                                provenance.model_profile.content_sha256 == expected_profile_hash
                            });
                        if reusable && matches!(intent, LoadIntent::Manual) {
                            backend.residency = BackendResidency::Pinned;
                        }
                        if reusable {
                            let status = self.status_from_state(&state);
                            let (sender, completion) = oneshot::channel();
                            let _ = sender.send(Ok(status.clone()));
                            return Ok(LoadAdmission { status, completion });
                        }
                        true
                    }
                    BackendLifecycle::Failed => true,
                    lifecycle => return Err(RuntimeError::Busy(lifecycle)),
                }
            } else {
                false
            }
        };
        if failed_backend {
            let (returned_operation, result) = self
                .stop_backend_with_operation_held(&profile_id, operation)
                .await;
            operation = returned_operation;
            result?;
        }
        let cancellation_epoch = self.cancellation_epoch.load(Ordering::Acquire);
        let (role, residency) = match intent {
            LoadIntent::Manual => (model_profile.role, BackendResidency::Pinned),
        };
        let generation = self
            .reserve_loading(&profile_id, &model.id, role, residency, cancellation_epoch)
            .await?;
        let status = self.status().await;
        let (completion_sender, completion) = oneshot::channel();
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let result = manager
                .load_admitted(
                    AdmittedLoad {
                        model_profile,
                        model,
                        runtime_id,
                        settings,
                        cancellation_epoch,
                        generation,
                        profile_role: role,
                        residency,
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
        profile_id: &ModelProfileId,
        model_id: &ModelId,
        profile_role: ModelRole,
        residency: BackendResidency,
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
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        state.backends.insert(
            profile_id.clone(),
            ManagedBackend {
                lifecycle: BackendLifecycle::Loading,
                model_profile_id: profile_id.clone(),
                model_id: model_id.clone(),
                profile_role,
                residency,
                engine_id: None,
                runtime_id: None,
                running: None,
                loading_process: None,
                loading_runtime_lease: None,
                cancel_loading: false,
                load_progress: Some(BackendLoadProgress::indeterminate(
                    BackendLoadPhase::Starting,
                )),
                failure: None,
                provenance: None,
                generation,
                activity: BackendActivity::new(),
                retiring: false,
            },
        );
        push_notice(
            &mut state,
            RuntimeNoticeLevel::Info,
            format!("Loading Model Profile {profile_id} (artifact {model_id})"),
        );
        Ok(generation)
    }

    fn current_lifecycle(&self) -> BackendLifecycle {
        self.state
            .try_read()
            .ok()
            .and_then(|state| {
                state
                    .backends
                    .values()
                    .find(|backend| {
                        matches!(
                            backend.lifecycle,
                            BackendLifecycle::Loading | BackendLifecycle::Stopping
                        )
                    })
                    .map(|backend| backend.lifecycle)
            })
            .unwrap_or(BackendLifecycle::Stopped)
    }

    async fn load_admitted(
        self: &Arc<Self>,
        admitted: AdmittedLoad,
        _operation: OwnedMutexGuard<()>,
    ) -> Result<ControlStatus, RuntimeError> {
        let AdmittedLoad {
            model_profile,
            model,
            runtime_id,
            settings: invocation_settings,
            cancellation_epoch,
            generation,
            profile_role,
            residency,
        } = admitted;
        let profile_id = model_profile.id.clone();
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
            BackendLoadProgress::indeterminate(BackendLoadPhase::ResolvingSettings),
        )
        .await;
        let settings_state = match self.settings.read().await {
            Ok(state) => state,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::Operation(error.to_string()));
            }
        };
        let engine_id = model_profile.engine_id.to_string();
        let adapter = match self.registry.get(&engine_id) {
            Some(adapter) => adapter,
            None => {
                let error = RuntimeError::EngineUnavailable(format!(
                    "Model Profile `{profile_id}` binds unregistered engine `{engine_id}`"
                ));
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(error);
            }
        };
        if let crate::CompatibilityDecision::Unsupported { reason } = adapter.compatibility(&model)
        {
            self.fail_loading(generation, reason.clone(), None).await;
            return Err(RuntimeError::Incompatible {
                model_id: model_id.clone(),
                reason,
            });
        }
        if let Some(id) = invocation_settings
            .0
            .keys()
            .find(|id| !id.applies_to_engine(&engine_id))
        {
            let error = RuntimeError::Operation(format!(
                "invocation setting `{id}` does not apply to selected engine `{engine_id}`"
            ));
            self.fail_loading(generation, error.to_string(), None).await;
            return Err(error);
        }
        let mut resolved_settings = match settings_state.resolve(
            &profile_id,
            &engine_id,
            &model_profile.overrides,
            &invocation_settings,
            &self.core.paths.data_dir,
        ) {
            Ok(settings) => settings,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::Operation(error.to_string()));
            }
        };
        self.set_load_progress(
            generation,
            BackendLoadProgress::indeterminate(BackendLoadPhase::SelectingRuntime),
        )
        .await;
        let (_, selection) = match self
            .select_runtime(
                &model,
                &engine_id,
                runtime_id.as_ref(),
                Some(&resolved_settings),
            )
            .await
        {
            Ok(selection) => selection,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(error);
            }
        };
        let host = self.packs.host_capabilities().await;
        let settings_schema = match adapter
            .settings_schema(&selection.runtime, &model, &host, Some(&resolved_settings))
            .await
        {
            Ok(schema) => schema,
            Err(error) => {
                self.fail_loading(generation, error.to_string(), None).await;
                return Err(RuntimeError::StartupFailed(error.to_string()));
            }
        };
        if let Err(error) = settings_schema
            .materialize_runtime_configuration(&mut resolved_settings)
            .and_then(|()| settings_schema.validate(&resolved_settings))
            .and_then(|()| settings_schema.materialize_effective(&mut resolved_settings))
        {
            self.fail_loading(generation, error.to_string(), None).await;
            return Err(RuntimeError::StartupFailed(error.to_string()));
        }
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
            let backend = state.backends.get_mut(&profile_id);
            if backend.as_ref().is_none_or(|backend| {
                backend.generation != generation
                    || backend.lifecycle != BackendLifecycle::Loading
                    || backend.cancel_loading
            }) || self.load_cancelled(cancellation_epoch)
            {
                true
            } else {
                let backend = backend.expect("checked above");
                backend.engine_id = Some(engine_id.clone());
                backend.runtime_id = Some(selected_runtime_id.clone());
                backend.loading_runtime_lease = Some(runtime_lease);
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
                settings: resolved_settings.clone(),
                settings_schema: settings_schema.clone(),
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
            let settings = launch_spec.settings.clone();
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
                let backend = state.backends.get_mut(&profile_id);
                if backend.as_ref().is_none_or(|backend| {
                    backend.generation != generation
                        || backend.lifecycle != BackendLifecycle::Loading
                        || backend.cancel_loading
                }) || self.load_cancelled(cancellation_epoch)
                {
                    true
                } else {
                    backend.expect("checked above").loading_process = Some(process.clone());
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
                model_profile: ModelProfileRuntimeIdentity {
                    model_profile_id: model_profile.id.clone(),
                    display_name: model_profile.display_name.clone(),
                    content_sha256: model_profile.content_hash(),
                    bound_model_id: model_profile.model_id.clone(),
                    bound_engine_id: model_profile.engine_id.clone(),
                    role: model_profile.role,
                },
                settings: SettingsProvenance {
                    effective: settings.effective,
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
            if let Some(backend) = self.state.write().await.backends.get_mut(&profile_id) {
                backend.provenance = Some(provenance);
            }
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
                    let detail = format!(
                        "engine startup did not prove the effective settings/startup contract: {error}"
                    );
                    let retained = self.terminate_or_retain(&process).await;
                    adapter.clear_launch_state(Some(&endpoint)).await;
                    self.fail_loading(generation, detail.clone(), retained.as_ref())
                        .await;
                    return Err(RuntimeError::StartupFailed(detail));
                }
            };
            match observation {
                StartupObservation::Ready(mut observation) => {
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
                    if let Err(error) =
                        merge_effective_generation_settings(&mut observation, settings)
                    {
                        cleanup_pending_launch_files(&launch_attempts).await;
                        let detail = format!(
                            "could not record effective generation settings from the engine: {error}"
                        );
                        let retained = self.terminate_or_retain(&process).await;
                        adapter.clear_launch_state(Some(&endpoint)).await;
                        self.fail_loading(generation, detail.clone(), retained.as_ref())
                            .await;
                        return Err(RuntimeError::StartupFailed(detail));
                    }
                    break (process, endpoint, exit, observation, settings);
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
                    if let Some(backend) = self.state.write().await.backends.get_mut(&profile_id) {
                        backend.loading_process = None;
                    }
                    if launch_attempts.is_empty() {
                        let attempts = context_attempts
                            .iter()
                            .map(|(mode, context)| format!("{mode}={context}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let detail = format!(
                            "no exact-runtime-supported automatic KV mode met the {minimum_context}-token minimum; attempted modes and observed contexts: {attempts}"
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
                "automatic_kv_attempts".to_owned(),
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
            let invalid = state.backends.get(&profile_id).is_none_or(|backend| {
                backend.generation != generation || backend.lifecycle != BackendLifecycle::Loading
            }) || self.load_cancelled(cancellation_epoch);
            if invalid {
                let detail = if state.backends.get(&profile_id).is_some_and(|backend| {
                    backend.cancel_loading || backend.lifecycle == BackendLifecycle::Stopping
                }) || self.load_cancelled(cancellation_epoch)
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
            let backend = state.backends.get_mut(&profile_id).expect("checked above");
            backend.lifecycle = BackendLifecycle::Running;
            backend.loading_process = None;
            backend.cancel_loading = false;
            backend.load_progress = None;
            backend.profile_role = profile_role;
            backend.residency = residency;
            if let Some(provenance) = backend.provenance.as_mut() {
                provenance.normalized_settings.extend(startup_observation);
                provenance.normalized_settings.insert(
                    "temperature".to_owned(),
                    serde_json::json!(effective_generation_settings.temperature),
                );
                provenance.normalized_settings.insert(
                    "top_p".to_owned(),
                    serde_json::json!(effective_generation_settings.top_p),
                );
                reconcile_effective_settings_from_runtime(provenance);
            }
            let runtime_lease = backend
                .loading_runtime_lease
                .take()
                .expect("a loading runtime must retain its store lease");
            backend.running = Some(RunningBackend {
                adapter,
                process: process.clone(),
                endpoint,
                effective_generation_settings,
                settings: resolved_settings.clone(),
                settings_schema,
                _runtime_lease: runtime_lease,
            });
            push_notice(
                &mut state,
                RuntimeNoticeLevel::Info,
                format!("Model Profile {profile_id} is ready (artifact {model_id})"),
            );
        }
        spawn_exit_monitor(Arc::downgrade(self), profile_id, generation, process, exit);
        Ok(self.status().await)
    }

    pub async fn unload(
        self: &Arc<Self>,
        profile_id: ModelProfileId,
    ) -> Result<ControlStatus, RuntimeError> {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            manager.cancel_loading(&profile_id).await;
            let operation = Arc::clone(&manager.operation).lock_owned().await;
            let (_operation, result) = manager
                .stop_backend_with_operation_held(&profile_id, operation)
                .await;
            result
        })
        .await
        .map_err(|error| RuntimeError::Operation(format!("unload task failed: {error}")))?
    }

    async fn wait_until_drained(activity: &BackendActivity) {
        while activity.active_requests.load(Ordering::Acquire) != 0 {
            activity.drained.notified().await;
        }
    }

    async fn stop_backend_with_operation_held(
        self: &Arc<Self>,
        profile_id: &ModelProfileId,
        operation: OwnedMutexGuard<()>,
    ) -> (OwnedMutexGuard<()>, Result<ControlStatus, RuntimeError>) {
        let (process, adapter, activity) = {
            let mut state = self.state.write().await;
            let Some(backend) = state.backends.get_mut(profile_id) else {
                return (operation, Ok(self.status_from_state(&state)));
            };
            backend.retiring = true;
            backend.lifecycle = BackendLifecycle::Stopping;
            let process = backend
                .running
                .as_ref()
                .map(|running| running.process.clone())
                .or_else(|| backend.loading_process.clone());
            let adapter = backend
                .running
                .as_ref()
                .map(|running| Arc::clone(&running.adapter))
                .or_else(|| {
                    backend
                        .engine_id
                        .as_deref()
                        .and_then(|id| self.registry.get(id))
                });
            let activity = Arc::clone(&backend.activity);
            push_notice(
                &mut state,
                RuntimeNoticeLevel::Info,
                format!("Draining Model Profile {profile_id} before unload"),
            );
            (process, adapter, activity)
        };
        let manager = Arc::clone(self);
        let profile_id = profile_id.clone();
        let (completion_sender, completion) = oneshot::channel();
        tokio::spawn(async move {
            Self::wait_until_drained(&activity).await;
            let result = manager
                .finish_backend_stop_with_parts(&profile_id, process, adapter)
                .await;
            if let Err(error) = &result {
                tracing::warn!(profile = %profile_id, %error, "backend retirement failed");
            }
            let _ = completion_sender.send((operation, result));
        });
        match completion.await {
            Ok(completion) => completion,
            Err(error) => {
                let operation = Arc::clone(&self.operation).lock_owned().await;
                (
                    operation,
                    Err(RuntimeError::Operation(format!(
                        "backend retirement task failed: {error}"
                    ))),
                )
            }
        }
    }

    async fn finish_backend_stop_with_parts(
        &self,
        profile_id: &ModelProfileId,
        process: Option<ProcessDescriptor>,
        adapter: Option<Arc<dyn EngineAdapter>>,
    ) -> Result<ControlStatus, RuntimeError> {
        if let Some(process) = process.as_ref()
            && let Err(error) = self.supervisor.terminate(process).await
        {
            let mut state = self.state.write().await;
            if let Some(backend) = state.backends.get_mut(profile_id) {
                backend.lifecycle = BackendLifecycle::Failed;
                backend.failure = Some(error.to_string());
            }
            push_notice(
                &mut state,
                RuntimeNoticeLevel::Error,
                format!("Backend {profile_id} termination failed: {error}"),
            );
            return Err(RuntimeError::Operation(error.to_string()));
        }
        if let Some(adapter) = adapter {
            adapter
                .clear_launch_state(process.as_ref().and_then(|p| p.endpoint.as_deref()))
                .await;
        }
        let mut state = self.state.write().await;
        state.backends.remove(profile_id);
        for session in state.sessions.values_mut() {
            if session.primary.as_ref() == Some(profile_id) {
                session.primary = None;
            }
        }
        push_notice(
            &mut state,
            RuntimeNoticeLevel::Info,
            format!("Model Profile {profile_id} unloaded"),
        );
        Ok(self.status_from_state(&state))
    }

    pub async fn infer(
        self: &Arc<Self>,
        request: InferenceRequest,
    ) -> Result<RoutedInferenceOutput, RuntimeError> {
        self.infer_routed(request, InferenceRoutingContext::default())
            .await
    }

    pub async fn infer_routed(
        self: &Arc<Self>,
        mut request: InferenceRequest,
        routing: InferenceRoutingContext,
    ) -> Result<RoutedInferenceOutput, RuntimeError> {
        let target = self
            .inference_target(&request.model_profile_id, routing)
            .await?;
        prepare_inference_request(
            &target.adapter,
            &target.endpoint,
            &target.settings,
            &mut request,
        )
        .await
        .map_err(map_inference_error)?;
        target
            .adapter
            .validate_inference_request(
                &request,
                &target.generation_settings,
                &target.settings_schema,
            )
            .map_err(map_inference_error)?;
        let effective_generation_settings = target
            .generation_settings
            .merged(&request.generation_settings);
        let effective_output_format = request.output_format.clone();
        let output = target
            .adapter
            .infer(&target.endpoint, request)
            .await
            .map_err(map_inference_error)?;
        drop(target.lease);
        Ok(RoutedInferenceOutput {
            output,
            effective_generation_settings,
            effective_output_format,
        })
    }

    pub async fn infer_stream(
        self: &Arc<Self>,
        request: InferenceRequest,
    ) -> Result<RoutedInferenceStream, RuntimeError> {
        self.infer_stream_routed(request, InferenceRoutingContext::default())
            .await
    }

    pub async fn infer_stream_routed(
        self: &Arc<Self>,
        mut request: InferenceRequest,
        routing: InferenceRoutingContext,
    ) -> Result<RoutedInferenceStream, RuntimeError> {
        let target = self
            .inference_target(&request.model_profile_id, routing)
            .await?;
        prepare_inference_request(
            &target.adapter,
            &target.endpoint,
            &target.settings,
            &mut request,
        )
        .await
        .map_err(map_inference_error)?;
        target
            .adapter
            .validate_inference_request(
                &request,
                &target.generation_settings,
                &target.settings_schema,
            )
            .map_err(map_inference_error)?;
        let effective_generation_settings = target
            .generation_settings
            .merged(&request.generation_settings);
        let effective_output_format = request.output_format.clone();
        let stream = target
            .adapter
            .infer_stream(&target.endpoint, request)
            .await
            .map_err(map_inference_error)?;
        let stream = Box::pin(LeasedInferenceStream {
            inner: stream,
            lease: Some(target.lease),
        });
        Ok(RoutedInferenceStream {
            stream,
            effective_generation_settings,
            effective_output_format,
        })
    }

    pub async fn shutdown(self: &Arc<Self>) {
        self.shutting_down.store(true, Ordering::Release);
        self.cancellation_epoch.fetch_add(1, Ordering::AcqRel);
        if let Some(reaper) = self.reaper.lock().await.take() {
            reaper.abort();
            let _ = reaper.await;
        }
        let profiles = self
            .state
            .read()
            .await
            .backends
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for profile in &profiles {
            self.cancel_loading(profile).await;
        }
        let mut operation = Arc::clone(&self.operation).lock_owned().await;
        for profile in profiles {
            let (returned_operation, result) = self
                .stop_backend_with_operation_held(&profile, operation)
                .await;
            operation = returned_operation;
            if let Err(error) = result {
                tracing::warn!(%profile, %error, "could not unload backend during shutdown");
            }
        }
        self.supervisor.shutdown().await;
    }

    async fn set_load_progress(&self, generation: u64, progress: BackendLoadProgress) {
        let mut state = self.state.write().await;
        if let Some(backend) = state
            .backends
            .values_mut()
            .find(|b| b.generation == generation)
        {
            backend.apply_load_progress(generation, progress);
        }
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

    async fn cancel_loading(&self, profile_id: &ModelProfileId) {
        let (process, adapter) = {
            let mut state = self.state.write().await;
            let Some(backend) = state.backends.get_mut(profile_id) else {
                return;
            };
            if backend.lifecycle != BackendLifecycle::Loading {
                return;
            }
            backend.lifecycle = BackendLifecycle::Stopping;
            backend.cancel_loading = true;
            backend.load_progress = None;
            let process = backend.loading_process.clone();
            let adapter = backend
                .engine_id
                .as_deref()
                .and_then(|id| self.registry.get(id));
            push_notice(
                &mut state,
                RuntimeNoticeLevel::Info,
                format!("Cancelling Model Profile {profile_id} load"),
            );
            (process, adapter)
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

    async fn select_runtime(
        &self,
        model: &norted_core::ModelArtifact,
        engine_id: &str,
        explicit_runtime: Option<&RuntimeId>,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<(Arc<dyn EngineAdapter>, RuntimeSelection), RuntimeError> {
        let selection = self
            .packs
            .resolve_for_engine_with_settings(model, engine_id, explicit_runtime, settings)
            .await
            .map_err(|error| match error {
                RuntimePackError::Incompatible { reason, .. } => RuntimeError::Incompatible {
                    model_id: model.id.clone(),
                    reason,
                },
                error => RuntimeError::EngineUnavailable(error.to_string()),
            })?;
        let selected_engine_id = &selection.runtime.manifest.identity.engine_id;
        let adapter = self.registry.get(selected_engine_id).ok_or_else(|| {
            RuntimeError::EngineUnavailable(format!(
                "selected runtime uses unregistered engine `{selected_engine_id}`"
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
        let Some(backend) = state
            .backends
            .values_mut()
            .find(|backend| backend.generation == generation)
        else {
            return;
        };
        let profile_id = backend.model_profile_id.clone();
        let failed_phase = backend
            .load_progress
            .take()
            .map(|progress| progress.phase)
            .filter(|_| {
                !backend.cancel_loading
                    && backend.lifecycle == BackendLifecycle::Loading
                    && detail != "model load was cancelled"
            });
        let detail = match failed_phase {
            Some(phase) => format!("failed while {}: {detail}", phase.failure_label()),
            None => detail,
        };
        backend.lifecycle = BackendLifecycle::Failed;
        backend.failure = Some(detail.clone());
        backend.running = None;
        backend.loading_process = process.cloned();
        if process.is_none() {
            backend.runtime_id = None;
            backend.loading_runtime_lease = None;
        }
        backend.cancel_loading = false;
        if let Some(process) = process
            && let Some(provenance) = &mut backend.provenance
        {
            provenance.process.process_id = process.process_id;
        }
        push_notice(
            &mut state,
            RuntimeNoticeLevel::Error,
            format!("Model Profile {profile_id} startup failed: {detail}"),
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
        self: &Arc<Self>,
        requested: &ModelProfileId,
        routing: InferenceRoutingContext,
    ) -> Result<InferenceTarget, RuntimeError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(RuntimeError::ShuttingDown);
        }
        let profiles = self
            .model_profiles
            .read()
            .await
            .map_err(|error| RuntimeError::Operation(error.to_string()))?;
        let profile = profiles
            .profiles
            .get(requested)
            .cloned()
            .ok_or_else(|| RuntimeError::ModelProfileNotFound(requested.clone()))?;
        let role = routing.role.unwrap_or(profile.role);
        let profile_role = profile.role;
        let profile_hash = profile.content_hash();
        {
            let mut state = self.state.write().await;
            if let Some(session) = state.sessions.get_mut(&routing.session_id) {
                session.last_used_unix = unix_timestamp();
            }
        }
        if let Some(target) = self
            .try_fast_target(requested, &profile_hash, role, &routing.session_id)
            .await
        {
            return Ok(target);
        }
        if !self.options.jit_enabled && !self.state.read().await.backends.contains_key(requested) {
            return Err(RuntimeError::ModelProfileNotLoaded(requested.clone()));
        }
        let mut operation = Arc::clone(&self.operation).lock_owned().await;

        if role == ModelRole::Primary {
            let previous = {
                let mut state = self.state.write().await;
                let now = unix_timestamp();
                let session =
                    state
                        .sessions
                        .entry(routing.session_id.clone())
                        .or_insert(SessionState {
                            primary: None,
                            last_used_unix: now,
                        });
                session.last_used_unix = now;
                let previous = session.primary.replace(requested.clone());
                previous.filter(|old| old != requested)
            };
            if let Some(previous) = previous {
                let eligible = {
                    let state = self.state.read().await;
                    state.backends.get(&previous).is_some_and(|backend| {
                        backend.residency == BackendResidency::Jit
                            && !state
                                .sessions
                                .values()
                                .any(|session| session.primary.as_ref() == Some(&previous))
                    })
                };
                if eligible {
                    let (returned_operation, result) = self
                        .stop_backend_with_operation_held(&previous, operation)
                        .await;
                    operation = returned_operation;
                    result?;
                }
            }
        } else {
            operation = self
                .reclaim_idle_with_operation_held(Some(requested), operation)
                .await;
        }

        let existing = self
            .state
            .read()
            .await
            .backends
            .get(requested)
            .map(|backend| {
                let compatible = backend.provenance.as_ref().is_some_and(|provenance| {
                    provenance.model_profile.content_sha256 == profile_hash
                });
                (backend.lifecycle, backend.residency, compatible)
            });
        match existing {
            Some((BackendLifecycle::Running, _, true)) => {
                let target = self.acquire_target(requested).await;
                drop(operation);
                return target;
            }
            Some((BackendLifecycle::Running, BackendResidency::Jit, false))
            | Some((BackendLifecycle::Failed, _, _)) => {
                let (returned_operation, result) = self
                    .stop_backend_with_operation_held(requested, operation)
                    .await;
                operation = returned_operation;
                result?;
            }
            Some((BackendLifecycle::Running, BackendResidency::Pinned, false)) => {
                return Err(RuntimeError::Operation(format!(
                    "pinned Model Profile `{requested}` was launched from an older profile configuration; unload it before inference"
                )));
            }
            Some((lifecycle, _, _)) => return Err(RuntimeError::Busy(lifecycle)),
            None => {}
        }
        if !self.options.jit_enabled {
            return Err(RuntimeError::ModelProfileNotLoaded(requested.clone()));
        }
        let model = self.core.model(&profile.model_id).await.ok_or_else(|| {
            RuntimeError::BoundModelMissing {
                profile_id: requested.clone(),
                model_id: profile.model_id.clone(),
            }
        })?;
        let cancellation_epoch = self.cancellation_epoch.load(Ordering::Acquire);
        let generation = self
            .reserve_loading(
                requested,
                &model.id,
                profile_role,
                BackendResidency::Jit,
                cancellation_epoch,
            )
            .await?;
        self.load_admitted(
            AdmittedLoad {
                model_profile: profile,
                model,
                runtime_id: None,
                settings: SettingsPatch::default(),
                cancellation_epoch,
                generation,
                profile_role,
                residency: BackendResidency::Jit,
            },
            operation,
        )
        .await
        .map_err(|error| {
            if role == ModelRole::Auxiliary {
                RuntimeError::AuxiliaryLoadFailed {
                    profile_id: requested.clone(),
                    reason: error.to_string(),
                }
            } else {
                error
            }
        })?;
        let operation = Arc::clone(&self.operation).lock_owned().await;
        let target = self.acquire_target(requested).await;
        drop(operation);
        target
    }

    async fn acquire_target(
        &self,
        requested: &ModelProfileId,
    ) -> Result<InferenceTarget, RuntimeError> {
        let state = self.state.read().await;
        let backend = state
            .backends
            .get(requested)
            .ok_or_else(|| RuntimeError::ModelProfileNotLoaded(requested.clone()))?;
        if backend.lifecycle == BackendLifecycle::Failed {
            return Err(RuntimeError::BackendCrashed(
                backend
                    .failure
                    .clone()
                    .unwrap_or_else(|| "backend is in a failed state".to_owned()),
            ));
        }
        let active = backend
            .running
            .as_ref()
            .filter(|_| backend.lifecycle == BackendLifecycle::Running && !backend.retiring)
            .ok_or_else(|| RuntimeError::ModelProfileNotLoaded(requested.clone()))?;
        let lease = InferenceLease::acquire(Arc::clone(&backend.activity));
        Ok(InferenceTarget {
            adapter: Arc::clone(&active.adapter),
            endpoint: active.endpoint.clone(),
            generation_settings: active.effective_generation_settings,
            settings: active.settings.clone(),
            settings_schema: active.settings_schema.clone(),
            lease,
        })
    }

    async fn try_fast_target(
        &self,
        requested: &ModelProfileId,
        profile_hash: &str,
        role: ModelRole,
        session_id: &str,
    ) -> Option<InferenceTarget> {
        let mut state = self.state.write().await;
        let can_route = state.backends.get(requested).is_some_and(|backend| {
            backend.lifecycle == BackendLifecycle::Running
                && !backend.retiring
                && backend.provenance.as_ref().is_some_and(|provenance| {
                    provenance.model_profile.content_sha256 == profile_hash
                })
        });
        if !can_route {
            return None;
        }
        if role == ModelRole::Primary {
            if state
                .sessions
                .get(session_id)
                .is_none_or(|session| session.primary.as_ref() != Some(requested))
            {
                return None;
            }
            let now = unix_timestamp();
            let session = state.sessions.get_mut(session_id).expect("checked above");
            session.last_used_unix = now;
        }
        let backend = state.backends.get(requested).expect("checked above");
        let active = backend
            .running
            .as_ref()
            .expect("running backend has process state");
        let lease = InferenceLease::acquire(Arc::clone(&backend.activity));
        Some(InferenceTarget {
            adapter: Arc::clone(&active.adapter),
            endpoint: active.endpoint.clone(),
            generation_settings: active.effective_generation_settings,
            settings: active.settings.clone(),
            settings_schema: active.settings_schema.clone(),
            lease,
        })
    }

    async fn reap_idle(self: &Arc<Self>) {
        let Ok(operation) = Arc::clone(&self.operation).try_lock_owned() else {
            return;
        };
        self.reclaim_idle_with_operation_held(None, operation).await;
    }

    async fn reclaim_idle_with_operation_held(
        self: &Arc<Self>,
        requested_profile: Option<&ModelProfileId>,
        mut operation: OwnedMutexGuard<()>,
    ) -> OwnedMutexGuard<()> {
        let now = unix_timestamp();
        let leased_primaries = {
            let mut state = self.state.write().await;
            let session_ttl = duration_seconds_i64(self.options.primary_idle_ttl);
            state
                .sessions
                .retain(|_, session| now.saturating_sub(session.last_used_unix) < session_ttl);
            state
                .sessions
                .values()
                .filter_map(|session| session.primary.clone())
                .collect::<BTreeSet<_>>()
        };
        let candidates = {
            let state = self.state.read().await;
            let auxiliary_ttl = duration_seconds_i64(self.options.auxiliary_idle_ttl);
            let primary_ttl = duration_seconds_i64(self.options.primary_idle_ttl);
            let mut idle_auxiliary = state
                .backends
                .values()
                .filter(|backend| {
                    backend.residency == BackendResidency::Jit
                        && backend.profile_role == ModelRole::Auxiliary
                        && backend.lifecycle == BackendLifecycle::Running
                        && backend.activity.active_requests.load(Ordering::Acquire) == 0
                        && Some(&backend.model_profile_id) != requested_profile
                        && !leased_primaries.contains(&backend.model_profile_id)
                })
                .map(|backend| {
                    (
                        backend.model_profile_id.clone(),
                        backend.activity.last_used_unix.load(Ordering::Acquire),
                    )
                })
                .collect::<Vec<_>>();
            idle_auxiliary.sort_by_key(|(_, last_used)| *last_used);
            let incoming_auxiliary = usize::from(requested_profile.is_some());
            let excess = idle_auxiliary
                .len()
                .saturating_add(incoming_auxiliary)
                .saturating_sub(self.options.max_idle_auxiliary_backends);
            let mut selected = idle_auxiliary
                .iter()
                .enumerate()
                .filter_map(|(index, (id, last_used))| {
                    (now.saturating_sub(*last_used) >= auxiliary_ttl || index < excess)
                        .then_some(id.clone())
                })
                .collect::<Vec<_>>();
            selected.extend(state.backends.values().filter_map(|backend| {
                (backend.residency == BackendResidency::Jit
                    && backend.profile_role == ModelRole::Primary
                    && backend.lifecycle == BackendLifecycle::Running
                    && backend.activity.active_requests.load(Ordering::Acquire) == 0
                    && !leased_primaries.contains(&backend.model_profile_id)
                    && Some(&backend.model_profile_id) != requested_profile
                    && now.saturating_sub(backend.activity.last_used_unix.load(Ordering::Acquire))
                        >= primary_ttl)
                    .then_some(backend.model_profile_id.clone())
            }));
            selected.extend(state.backends.values().filter_map(|backend| {
                let ttl = match backend.profile_role {
                    ModelRole::Primary => primary_ttl,
                    ModelRole::Auxiliary => auxiliary_ttl,
                };
                (backend.residency == BackendResidency::Jit
                    && backend.lifecycle == BackendLifecycle::Failed
                    && backend.activity.active_requests.load(Ordering::Acquire) == 0
                    && !leased_primaries.contains(&backend.model_profile_id)
                    && now.saturating_sub(backend.activity.last_used_unix.load(Ordering::Acquire))
                        >= ttl)
                    .then_some(backend.model_profile_id.clone())
            }));
            selected
        };
        for profile in candidates {
            let (returned_operation, result) = self
                .stop_backend_with_operation_held(&profile, operation)
                .await;
            operation = returned_operation;
            if let Err(error) = result {
                tracing::warn!(%profile, %error, "could not reap idle JIT backend");
            }
        }
        operation
    }

    async fn handle_unexpected_exit(
        &self,
        profile_id: ModelProfileId,
        generation: u64,
        process: ProcessDescriptor,
        exit: ProcessExit,
    ) {
        let mut state = self.state.write().await;
        let Some(backend) = state.backends.get_mut(&profile_id) else {
            return;
        };
        if backend.generation != generation
            || backend.lifecycle != BackendLifecycle::Running
            || backend
                .running
                .as_ref()
                .is_none_or(|active| active.process.supervisor_id != process.supervisor_id)
        {
            return;
        }
        let detail = exit_detail(&exit);
        let adapter = backend
            .running
            .as_ref()
            .map(|active| Arc::clone(&active.adapter));
        backend.lifecycle = BackendLifecycle::Failed;
        backend.failure = Some(detail.clone());
        backend.running = None;
        backend.runtime_id = None;
        backend.load_progress = None;
        push_notice(
            &mut state,
            RuntimeNoticeLevel::Error,
            format!("Model Profile {profile_id} exited unexpectedly: {detail}"),
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
        let backends = state
            .backends
            .values()
            .map(|backend| {
                let active = backend.running.as_ref();
                let process_id = active.map(|active| active.process.process_id).or_else(|| {
                    backend
                        .loading_process
                        .as_ref()
                        .map(|process| process.process_id)
                });
                let primary_lease_count = state
                    .sessions
                    .values()
                    .filter(|session| session.primary.as_ref() == Some(&backend.model_profile_id))
                    .count();
                BackendStatus {
                    generation: backend.generation,
                    lifecycle: backend.lifecycle,
                    model_profile_id: backend.model_profile_id.clone(),
                    model_id: backend.model_id.clone(),
                    role: backend.profile_role,
                    residency: backend.residency,
                    engine_id: backend.engine_id.clone(),
                    runtime_id: backend.runtime_id.clone(),
                    runtime_version: backend
                        .provenance
                        .as_ref()
                        .map(|p| p.runtime.identity.version.clone()),
                    runtime_variant: backend
                        .provenance
                        .as_ref()
                        .map(|p| p.runtime.identity.variant.clone()),
                    runtime_executable_sha256: backend
                        .provenance
                        .as_ref()
                        .map(|p| p.runtime.entrypoint_sha256.clone()),
                    process_id,
                    private_endpoint: active.map(|active| active.endpoint.clone()).or_else(|| {
                        backend
                            .provenance
                            .as_ref()
                            .map(|p| p.private_backend_endpoint.clone())
                    }),
                    load_progress: backend.load_progress.clone(),
                    failure: backend.failure.clone(),
                    provenance: backend.provenance.clone(),
                    active_request_count: backend.activity.active_requests.load(Ordering::Acquire),
                    primary_lease_count,
                    last_used_unix: backend.activity.last_used_unix.load(Ordering::Acquire),
                    retiring: backend.retiring,
                }
            })
            .collect::<Vec<_>>();
        ControlStatus {
            public_endpoint: state.public_endpoint.clone(),
            available_engine_count: engines.len(),
            installed_engine_count,
            running_backend_count: backends
                .iter()
                .filter(|backend| backend.lifecycle == BackendLifecycle::Running)
                .count(),
            engines,
            backends,
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

async fn prepare_inference_request(
    adapter: &Arc<dyn EngineAdapter>,
    endpoint: &str,
    settings: &norted_core::ResolvedSettings,
    request: &mut InferenceRequest,
) -> Result<(), EngineError> {
    if request.generation_settings.top_k.is_none()
        && let Some(norted_core::SettingValue::UnsignedInteger(value)) = settings.value("top_k")
    {
        request.generation_settings.top_k = Some(*value);
    }
    if request.generation_settings.min_p.is_none()
        && let Some(norted_core::SettingValue::Float(value)) = settings.value("min_p")
    {
        request.generation_settings.min_p = Some(*value);
    }
    if request.generation_settings.seed.is_none()
        && let Some(norted_core::SettingValue::UnsignedIntegerOrChoice(
            norted_core::UnsignedIntegerOrChoiceValue::UnsignedInteger(value),
        )) = settings.value("seed")
    {
        request.generation_settings.seed = Some(*value);
    }
    if request.generation_settings.repeat_penalty.is_none()
        && let Some(norted_core::SettingValue::Float(value)) = settings.value("repeat_penalty")
    {
        request.generation_settings.repeat_penalty = Some(*value);
    }
    if request.generation_settings.presence_penalty.is_none()
        && let Some(norted_core::SettingValue::Float(value)) = settings.value("presence_penalty")
    {
        request.generation_settings.presence_penalty = Some(*value);
    }
    if request.generation_settings.frequency_penalty.is_none()
        && let Some(norted_core::SettingValue::Float(value)) = settings.value("frequency_penalty")
    {
        request.generation_settings.frequency_penalty = Some(*value);
    }
    if request.generation_settings.stop.is_none()
        && let Some(norted_core::SettingValue::StringList(value)) = settings.value("stop_strings")
    {
        request.generation_settings.stop = Some(value.clone());
    }
    if request.generation_settings.reasoning_enabled.is_none()
        && let Some(norted_core::SettingValue::Choice(value)) = settings.value("reasoning")
    {
        request.generation_settings.reasoning_enabled = match value.as_str() {
            "on" => Some(true),
            "off" => Some(false),
            "auto" => None,
            _ => None,
        };
    }
    if adapter.uses_setting_as_request_default("reasoning_budget")
        && request.generation_settings.reasoning_budget.is_none()
        && let Some(norted_core::SettingValue::Integer(value)) = settings.value("reasoning_budget")
    {
        request.generation_settings.reasoning_budget = Some(*value);
    }
    if request.generation_settings.reasoning_effort.is_none()
        && let Some(norted_core::SettingValue::Choice(value)) = settings.value("reasoning_effort")
    {
        request.generation_settings.reasoning_effort = match value.as_str() {
            "none" => Some(crate::ReasoningEffort::None),
            "minimal" => Some(crate::ReasoningEffort::Minimal),
            "low" => Some(crate::ReasoningEffort::Low),
            "medium" => Some(crate::ReasoningEffort::Medium),
            "high" => Some(crate::ReasoningEffort::High),
            "xhigh" => Some(crate::ReasoningEffort::Xhigh),
            "max" => Some(crate::ReasoningEffort::Max),
            _ => None,
        };
    }
    if request.max_output_tokens.is_none()
        && let Some(norted_core::SettingValue::UnsignedInteger(value)) =
            settings.value("max_output_tokens")
    {
        request.max_output_tokens = u32::try_from(*value).ok();
    }
    if !request.messages.iter().any(|message| {
        matches!(
            message.role,
            crate::InferenceRole::System | crate::InferenceRole::Developer
        )
    }) && let Some(norted_core::SettingValue::String(prompt)) = settings.value("system_prompt")
    {
        request.messages.insert(
            0,
            crate::InferenceMessage::text(crate::InferenceRole::System, prompt.clone()),
        );
    }
    if request.output_format.is_none()
        && let Some(norted_core::SettingValue::Json(schema)) =
            settings.value("structured_output_schema")
    {
        request.output_format = Some(crate::OutputFormat::JsonSchema {
            name: None,
            description: None,
            schema: schema.clone(),
            strict: None,
        });
    }

    if !matches!(
        settings.value("context_overflow"),
        Some(norted_core::SettingValue::Choice(policy)) if policy == "truncate_middle"
    ) {
        return Ok(());
    }
    let output_allowance = request.max_output_tokens.ok_or_else(|| {
        EngineError::InvalidGenerationSettings(
            "truncate_middle requires an explicit request or Model Profile maximum output-token limit"
                .to_owned(),
        )
    })?;
    let capacity = adapter.context_capacity(endpoint).await?.ok_or_else(|| {
        EngineError::InvalidGenerationSettings(
            "the selected engine/runtime cannot prove an exact context capacity for truncate_middle"
                .to_owned(),
        )
    })?;
    let input_budget = capacity.checked_sub(u64::from(output_allowance)).ok_or_else(|| {
        EngineError::InvalidGenerationSettings(format!(
            "maximum output allowance {output_allowance} leaves no input capacity in the exact {capacity}-token context"
        ))
    })?;

    loop {
        let count = adapter
            .count_input_tokens(endpoint, request)
            .await?
            .ok_or_else(|| {
                EngineError::InvalidGenerationSettings(
                    "the selected engine/runtime has no exact tokenizer path for truncate_middle"
                        .to_owned(),
                )
            })?;
        if count <= input_budget {
            return Ok(());
        }
        let newest = request.messages.len().saturating_sub(1);
        let removable = request
            .messages
            .iter()
            .enumerate()
            .find_map(|(index, message)| {
                (index < newest
                    && !matches!(
                        message.role,
                        crate::InferenceRole::System | crate::InferenceRole::Developer
                    ))
                .then_some(index)
            });
        let Some(index) = removable else {
            return Err(EngineError::InvalidGenerationSettings(format!(
                "exact rendered input uses {count} tokens but only {input_budget} are available; required instructions and the newest conversational tail cannot be truncated"
            )));
        };
        request.messages.remove(index);
    }
}

fn spawn_exit_monitor(
    manager: Weak<RuntimeManager>,
    profile_id: ModelProfileId,
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
                .handle_unexpected_exit(profile_id, generation, process, observed)
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

fn merge_effective_generation_settings(
    normalized_settings: &mut BTreeMap<String, serde_json::Value>,
    effective: EffectiveGenerationSettings,
) -> Result<(), EngineError> {
    let resolved_settings = normalized_settings
        .entry("resolved_settings".to_owned())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            EngineError::Operation(
                "startup observation `resolved_settings` must be a JSON object".to_owned(),
            )
        })?;
    resolved_settings.insert(
        "temperature".to_owned(),
        serde_json::json!(effective.temperature),
    );
    resolved_settings.insert("top_p".to_owned(), serde_json::json!(effective.top_p));
    Ok(())
}

fn reconcile_effective_settings_from_runtime(provenance: &mut RuntimeProvenance) {
    let Some(resolved) = provenance
        .normalized_settings
        .get("resolved_settings")
        .and_then(serde_json::Value::as_object)
    else {
        return;
    };
    for (raw_id, value) in resolved {
        let Ok(id) = norted_core::SettingId::new(raw_id.clone()) else {
            continue;
        };
        let display = value
            .as_str()
            .map_or_else(|| value.to_string(), ToOwned::to_owned);
        match provenance.settings.effective.get_mut(&id) {
            Some(setting) => {
                let previous = setting.value.clone();
                setting.value = display;
                if previous != setting.value {
                    let resolution = format!(
                        "The launched runtime resolved the pre-startup `{previous}` policy/value"
                    );
                    setting.detail = Some(setting.detail.as_ref().map_or_else(
                        || resolution.clone(),
                        |detail| format!("{detail}; {resolution}"),
                    ));
                } else if setting.detail.is_none() {
                    setting.detail = Some("Confirmed by the launched runtime".to_owned());
                }
            }
            None => {
                provenance.settings.effective.insert(
                    id,
                    norted_core::EffectiveSetting {
                        value: display,
                        source: norted_core::SettingSource::RuntimeDefault,
                        detail: Some("Reported by the launched runtime".to_owned()),
                    },
                );
            }
        }
    }
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

fn duration_seconds_i64(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

fn retry_context_capacity_message(
    kv_mode: &str,
    observed_context: u64,
    minimum_context: u64,
) -> String {
    format!(
        "KV mode {kv_mode} served {observed_context} of the required {minimum_context} context tokens; retrying with the next configured automatic KV mode"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ManagerFixture {
        _temporary: tempfile::TempDir,
        manager: Arc<RuntimeManager>,
        profile_id: ModelProfileId,
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
            settings_file: root.join("data/settings.json"),
            settings_lock_file: root.join("data/.settings.lock"),
            model_profiles_file: root.join("data/model-profiles.json"),
            model_profiles_lock_file: root.join("data/.model-profiles.lock"),
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
        let profile_id = ModelProfileId::new("fixture").expect("profile ID");
        norted_core::ModelProfilesStore::new(&paths)
            .update({
                let profile_id = profile_id.clone();
                let model_id = model_id.clone();
                move |state| {
                    state.create(
                        profile_id.clone(),
                        "Fixture",
                        model_id,
                        norted_core::EngineId::new("llama.cpp").expect("engine ID"),
                    )?;
                    Ok(())
                }
            })
            .await
            .expect("model profile fixture");
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
            profile_id,
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
                if status
                    .newest_backend()
                    .is_some_and(|backend| backend.lifecycle == expected)
                {
                    return status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("lifecycle transition")
    }

    fn empty_backend() -> ManagedBackend {
        ManagedBackend {
            lifecycle: BackendLifecycle::Loading,
            model_profile_id: ModelProfileId::new("fixture").expect("profile ID"),
            model_id: ModelId("fixture".to_owned()),
            profile_role: ModelRole::Primary,
            residency: BackendResidency::Jit,
            engine_id: None,
            runtime_id: None,
            running: None,
            loading_process: None,
            loading_runtime_lease: None,
            cancel_loading: false,
            load_progress: None,
            failure: None,
            provenance: None,
            generation: 7,
            activity: BackendActivity::new(),
            retiring: false,
        }
    }

    #[tokio::test]
    async fn admission_returns_after_loading_generation_is_reserved() {
        let fixture = manager_fixture().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        *fixture.manager.load_start_gate.lock().await = Some(Arc::clone(&gate));

        let admitted = fixture
            .manager
            .start_load(fixture.profile_id.clone())
            .await
            .expect("load admission");
        let backend = admitted
            .backend(&fixture.profile_id)
            .expect("admitted backend");
        assert_eq!(backend.lifecycle, BackendLifecycle::Loading);
        assert_eq!(&backend.model_profile_id, &fixture.profile_id);
        assert_eq!(&backend.model_id, &fixture.model_id);
        assert_eq!(
            fixture
                .manager
                .status()
                .await
                .backend(&fixture.profile_id)
                .expect("backend")
                .generation,
            backend.generation
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
            .start_load(fixture.profile_id.clone())
            .await
            .expect("first admission");

        let error = fixture
            .manager
            .start_load(fixture.profile_id.clone())
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
            .reserve_loading(
                &fixture.profile_id,
                &fixture.model_id,
                ModelRole::Primary,
                BackendResidency::Jit,
                stale_epoch,
            )
            .await
            .expect_err("stale admission epoch");
        assert!(error.to_string().contains("cancelled before admission"));
        assert!(fixture.manager.status().await.backends.is_empty());
    }

    #[tokio::test]
    async fn admitted_background_failure_is_authoritative() {
        let fixture = manager_fixture().await;
        let admitted = fixture
            .manager
            .start_load(fixture.profile_id.clone())
            .await
            .expect("load admission");
        let failed = wait_for_lifecycle(&fixture.manager, BackendLifecycle::Failed).await;

        assert_eq!(
            failed
                .backend(&fixture.profile_id)
                .expect("failed backend")
                .generation,
            admitted
                .backend(&fixture.profile_id)
                .expect("admitted backend")
                .generation
        );
        assert!(
            failed
                .backend(&fixture.profile_id)
                .expect("failed backend")
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
            .start_load(fixture.profile_id.clone())
            .await
            .expect("load admission");

        let manager = Arc::clone(&fixture.manager);
        let profile_id = fixture.profile_id.clone();
        let unload = tokio::spawn(async move { manager.unload(profile_id).await });
        let _ = wait_for_lifecycle(&fixture.manager, BackendLifecycle::Stopping).await;
        gate.notify_one();
        let stopped = unload.await.expect("unload task").expect("unload");
        assert!(stopped.backends.is_empty());
    }

    #[tokio::test]
    async fn shutdown_cleans_up_an_admitted_background_load() {
        let fixture = manager_fixture().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        *fixture.manager.load_start_gate.lock().await = Some(Arc::clone(&gate));
        fixture
            .manager
            .start_load(fixture.profile_id.clone())
            .await
            .expect("load admission");

        let manager = Arc::clone(&fixture.manager);
        let shutdown = tokio::spawn(async move { manager.shutdown().await });
        let _ = wait_for_lifecycle(&fixture.manager, BackendLifecycle::Stopping).await;
        gate.notify_one();
        shutdown.await.expect("shutdown task");
        assert!(fixture.manager.status().await.backends.is_empty());
    }

    #[test]
    fn apply_load_progress_updates_the_current_generation() {
        let mut backend = empty_backend();
        backend.apply_load_progress(
            7,
            BackendLoadProgress::indeterminate(BackendLoadPhase::SelectingRuntime),
        );
        let progress = backend.load_progress.expect("progress was set");
        assert_eq!(progress.phase, BackendLoadPhase::SelectingRuntime);
        assert_eq!(progress.fraction, None);
    }

    #[test]
    fn an_old_load_generation_cannot_overwrite_newer_load_progress() {
        let mut backend = empty_backend();
        backend.apply_load_progress(
            7,
            BackendLoadProgress::indeterminate(BackendLoadPhase::LoadingModel),
        );
        // A newer load started under generation 8.
        backend.generation = 8;
        backend.apply_load_progress(
            7,
            BackendLoadProgress::indeterminate(BackendLoadPhase::SpawningBackend),
        );
        let progress = backend.load_progress.expect("progress is unchanged");
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
            let mut backend = empty_backend();
            backend.generation = 8;
            backend.load_progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::SelectingRuntime,
                "newer load",
            ));
            state.backends.insert(fixture.profile_id.clone(), backend);
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
        let progress = state
            .backends
            .get(&fixture.profile_id)
            .expect("backend")
            .load_progress
            .as_ref()
            .expect("new load progress");
        assert_eq!(progress.phase, BackendLoadPhase::SelectingRuntime);
        assert_eq!(progress.message.as_deref(), Some("newer load"));
    }

    #[test]
    fn apply_load_progress_is_ignored_once_loading_is_no_longer_active() {
        let mut backend = empty_backend();
        backend.lifecycle = BackendLifecycle::Running;
        backend.apply_load_progress(
            7,
            BackendLoadProgress::indeterminate(BackendLoadPhase::VerifyingStartup),
        );
        assert!(
            backend.load_progress.is_none(),
            "progress is not attached once the backend is Running"
        );

        let mut backend = empty_backend();
        backend.lifecycle = BackendLifecycle::Failed;
        backend.apply_load_progress(
            7,
            BackendLoadProgress::indeterminate(BackendLoadPhase::VerifyingStartup),
        );
        assert!(
            backend.load_progress.is_none(),
            "progress is not attached once the backend is Failed"
        );
    }

    #[test]
    fn apply_load_progress_sanitizes_untrustworthy_adapter_values() {
        let mut backend = empty_backend();
        backend.apply_load_progress(
            7,
            BackendLoadProgress {
                phase: BackendLoadPhase::LoadingModel,
                fraction: Some(1.5),
                current: Some(300),
                total: Some(200),
                message: None,
            },
        );
        let progress = backend.load_progress.expect("progress was sanitized");
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
            "KV mode fp8 served 180000 of the required 200000 context tokens; retrying with the next configured automatic KV mode"
        );
        assert!(!message.contains("next KV mode: fp8"));
    }
}
