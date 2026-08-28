//! Engine-neutral contracts for managed upstream inference runtimes.

use std::collections::{BTreeMap, btree_map::Entry};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::Stream;
use norted_core::{
    AcceleratorDevice, ArtifactFormat, AuxiliaryArtifactRole, AvailableRuntime, EngineInstallation,
    EngineRevision, HostCapabilities, InstalledRuntime, LoadSettingDefinition, LoadSettingId,
    LoadSettingsError, LoadSettingsPatch, LoadSettingsSchema, ModelArtifact, ModelId,
    ModelRuntimeIdentity, ResolvedLoadSettings, RuntimeCompatibility, RuntimeId,
    RuntimeProbeObservation,
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

mod catalog;
mod control;
mod installer;
mod manager;
mod packs;
mod store;
mod supervisor;

pub use catalog::{
    CatalogError, GitHubRelease, GitHubReleaseAsset, GitHubReleaseClient, RuntimeCatalog,
    RuntimeCatalogEntry, RuntimeCatalogProvider, RuntimeCatalogSnapshot, RuntimeProviderAuthority,
    RuntimeProviderError, compatibility_for, compatibility_for_nvidia_device,
    detect_host_capabilities,
};

pub use control::{
    CONTROL_LOAD_PATH, CONTROL_STATUS_PATH, CONTROL_UNLOAD_PATH, ControlClient, ControlClientError,
    ControlErrorResponse, ControlLoadRequest,
};
pub use manager::{
    BackendLifecycle, BackendStatus, ControlStatus, EngineStatus, RuntimeError, RuntimeManager,
    RuntimeManagerOptions, RuntimeNotice, RuntimeNoticeLevel,
};
pub use packs::{
    InstalledRuntimeStatus, RuntimeListSnapshot, RuntimeModelCandidate, RuntimePackError,
    RuntimePackManager, RuntimeSearchResult, RuntimeSearchSnapshot, RuntimeUpdateCheck,
};
pub use store::{RuntimeLease, RuntimeStore, RuntimeStoreError, RuntimeStoreSnapshot};
pub use supervisor::{CapturedCommand, TokioProcessSupervisor, capture_command};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineIdentity {
    pub id: String,
    pub display_name: String,
    pub upstream_repository: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngineCapabilities {
    pub artifact_formats: Vec<ArtifactFormat>,
    pub api: Vec<ApiCapability>,
    pub features: Vec<EngineFeature>,
}

impl EngineCapabilities {
    pub fn accepts(&self, format: ArtifactFormat) -> bool {
        self.artifact_formats.contains(&format)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiCapability {
    Responses,
    ChatCompletions,
    Embeddings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineFeature {
    TextGeneration,
    ToolCalling,
    StructuredOutput,
    Vision,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum CompatibilityDecision {
    Supported,
    Unsupported { reason: String },
}

impl CompatibilityDecision {
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum InstallationState {
    NotInstalled,
    Installed {
        installation: Box<EngineInstallation>,
    },
    Invalid {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum UpdateState {
    Unknown,
    Current,
    Available { revision: EngineRevision },
    CheckFailed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineProbe {
    pub installation: InstallationState,
    pub update: UpdateState,
    pub healthy: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeOption {
    pub name: String,
    pub description: String,
    pub value_kind: OptionValueKind,
    pub repeatable: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionValueKind {
    Flag,
    String,
    Integer,
    Float,
    Path,
}

pub fn common_load_setting_definitions() -> Vec<LoadSettingDefinition> {
    vec![
        LoadSettingDefinition {
            id: LoadSettingId::new("context_length").expect("static setting ID"),
            label: "Context length".to_owned(),
            description: "Explicit context window requested from the selected runtime".to_owned(),
            kind: norted_core::LoadSettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            scope: norted_core::LoadSettingScope::Common,
            supported: true,
            unsupported_reason: None,
            unit: Some("tokens".to_owned()),
            upstream_default: Some("runtime/model automatic behavior".to_owned()),
            recommendation: None,
        },
        LoadSettingDefinition {
            id: LoadSettingId::new("parallel_requests").expect("static setting ID"),
            label: "Parallel requests".to_owned(),
            description: "Number of concurrent server slots".to_owned(),
            kind: norted_core::LoadSettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            scope: norted_core::LoadSettingScope::Common,
            supported: true,
            unsupported_reason: None,
            unit: Some("slots".to_owned()),
            upstream_default: Some("runtime-selected".to_owned()),
            recommendation: None,
        },
    ]
}

#[derive(Debug, Clone)]
pub struct PreparedAuxiliaryArtifact {
    pub role: AuxiliaryArtifactRole,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub content_sha256: String,
}

#[derive(Debug, Clone)]
pub struct PreparedModelInput {
    pub primary: ModelArtifact,
    pub auxiliary: Vec<PreparedAuxiliaryArtifact>,
}

impl PreparedModelInput {
    pub fn runtime_identity(&self) -> ModelRuntimeIdentity {
        ModelRuntimeIdentity {
            model_id: self.primary.id.clone(),
            artifact_path: self.primary.path.clone(),
            content_sha256: self.primary.hash.clone(),
            auxiliary: self
                .auxiliary
                .iter()
                .map(|artifact| norted_core::AuxiliaryRuntimeIdentity {
                    role: artifact.role.clone(),
                    artifact_path: artifact.path.clone(),
                    size_bytes: artifact.size_bytes,
                    content_sha256: artifact.content_sha256.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub model: PreparedModelInput,
    pub runtime: InstalledRuntime,
    pub accelerator: Option<AcceleratorDevice>,
    pub backend_address: SocketAddr,
    pub load_settings: ResolvedLoadSettings,
    pub load_settings_schema: LoadSettingsSchema,
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<String, String>,
    pub environment_remove: Vec<OsString>,
    pub inherits_parent_environment: bool,
    pub working_directory: Option<PathBuf>,
    pub endpoint: Option<String>,
    pub normalized_settings: BTreeMap<String, serde_json::Value>,
    pub load_settings: ResolvedLoadSettings,
    pub native_arguments: Vec<String>,
    pub installation: EngineInstallation,
    pub runtime: InstalledRuntime,
    pub model: PreparedModelInput,
    pub accelerator: Option<AcceleratorDevice>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveGenerationSettings {
    pub temperature: f64,
    pub top_p: f64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelServingCapabilities {
    pub model_id: ModelId,
    pub format: ArtifactFormat,
    pub compatible_engine_ids: Vec<String>,
    pub compatible_installed_runtime_ids: Vec<RuntimeId>,
    pub selected_runtime_id: Option<RuntimeId>,
    pub active: bool,
    pub text_input: bool,
    pub text_output: bool,
    pub responses: bool,
    pub chat_completions: bool,
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    pub structured_output: bool,
}

impl EffectiveGenerationSettings {
    pub fn merged(self, patch: &GenerationSettingsPatch) -> Self {
        Self {
            temperature: patch.temperature.unwrap_or(self.temperature),
            top_p: patch.top_p.unwrap_or(self.top_p),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessDescriptor {
    pub supervisor_id: String,
    pub process_id: u32,
    pub engine: EngineRevision,
    pub runtime_id: norted_core::RuntimeId,
    pub runtime_version: String,
    pub runtime_variant: String,
    pub runtime_executable_sha256: String,
    pub model_id: norted_core::ModelId,
    pub endpoint: Option<String>,
    pub launched_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessExit {
    pub process_id: u32,
    pub success: bool,
    pub code: Option<i32>,
    pub expected: bool,
    pub detail: String,
    pub stderr_tail: Vec<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InferenceRole {
    System,
    Developer,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceMessage {
    pub role: InferenceRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationSettingsPatch {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
}

impl GenerationSettingsPatch {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none() && self.top_p.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequest {
    pub model_id: norted_core::ModelId,
    pub messages: Vec<InferenceMessage>,
    pub generation_settings: GenerationSettingsPatch,
    pub max_output_tokens: Option<u32>,
    pub stream: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceOutput {
    pub text: String,
    pub usage: Option<InferenceUsage>,
    pub finish_reason: InferenceFinishReason,
}

pub struct RoutedInferenceOutput {
    pub output: InferenceOutput,
    pub effective_generation_settings: EffectiveGenerationSettings,
}

pub struct RoutedInferenceStream {
    pub stream: InferenceStream,
    pub effective_generation_settings: EffectiveGenerationSettings,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceFinishReason {
    Stop,
    MaxOutputTokens,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum InferenceEvent {
    TextDelta {
        delta: String,
    },
    Completed {
        usage: Option<InferenceUsage>,
        finish_reason: InferenceFinishReason,
    },
}

pub type InferenceStream =
    Pin<Box<dyn Stream<Item = Result<InferenceEvent, EngineError>> + Send + 'static>>;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine operation is not implemented: {0}")]
    Unsupported(String),
    #[error("engine is not installed")]
    NotInstalled,
    #[error("engine operation failed: {0}")]
    Operation(String),
    #[error("engine backend is unavailable: {0}")]
    BackendUnavailable(String),
    #[error("engine operation timed out: {0}")]
    TimedOut(String),
    #[error("invalid engine configuration: {0}")]
    InvalidConfiguration(String),
    #[error("invalid generation settings: {0}")]
    InvalidGenerationSettings(String),
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("engine ID cannot be empty")]
    EmptyEngineId,
    #[error("engine ID `{0}` is already registered")]
    DuplicateEngineId(String),
}

#[async_trait]
pub trait EngineAdapter: Send + Sync {
    fn identity(&self) -> EngineIdentity;
    fn capabilities(&self) -> EngineCapabilities;
    fn runtime_management_compatibility(&self) -> CompatibilityDecision {
        CompatibilityDecision::Supported
    }
    fn available_runtime_compatibility(
        &self,
        _runtime: &AvailableRuntime,
    ) -> CompatibilityDecision {
        CompatibilityDecision::Supported
    }
    fn runtime_compatibility(&self, _runtime: &InstalledRuntime) -> CompatibilityDecision {
        CompatibilityDecision::Supported
    }
    fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
        if self.capabilities().accepts(model.format) {
            CompatibilityDecision::Supported
        } else {
            CompatibilityDecision::Unsupported {
                reason: format!(
                    "artifact format `{}` is not supported by this engine",
                    model.format.as_str()
                ),
            }
        }
    }
    /// Evaluates requirements that depend on this concrete runtime, model, and
    /// host. The shared resolver uses this for every model-specific path.
    fn runtime_model_compatibility(
        &self,
        _runtime: &InstalledRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
    ) -> RuntimeCompatibility {
        match self.compatibility(model) {
            CompatibilityDecision::Supported => RuntimeCompatibility::Compatible,
            CompatibilityDecision::Unsupported { reason } => {
                RuntimeCompatibility::Incompatible(reason)
            }
        }
    }
    /// Lower values are preferred after compatibility. Engines own the
    /// semantic ordering of their runtime variants.
    fn runtime_model_preference(
        &self,
        _runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        _host: &HostCapabilities,
    ) -> u16 {
        100
    }
    /// Evaluates a catalog runtime against a concrete model without installing
    /// it. Ordinary engines inherit the same coarse model gate as installed
    /// runtimes.
    fn available_runtime_model_compatibility(
        &self,
        _runtime: &AvailableRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
    ) -> RuntimeCompatibility {
        match self.compatibility(model) {
            CompatibilityDecision::Supported => RuntimeCompatibility::Compatible,
            CompatibilityDecision::Unsupported { reason } => {
                RuntimeCompatibility::Incompatible(reason)
            }
        }
    }
    fn available_runtime_model_preference(
        &self,
        _runtime: &AvailableRuntime,
        _model: &ModelArtifact,
        _host: &HostCapabilities,
    ) -> u16 {
        100
    }
    /// Returns the exact accelerator selected by the same policy used for
    /// model/runtime compatibility. `None` means the engine does not bind one.
    fn runtime_model_accelerator(
        &self,
        _runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        _host: &HostCapabilities,
    ) -> Option<AcceleratorDevice> {
        None
    }
    async fn prepare_model_input(
        &self,
        model: &ModelArtifact,
    ) -> Result<PreparedModelInput, EngineError> {
        Ok(PreparedModelInput {
            primary: model.clone(),
            auxiliary: Vec::new(),
        })
    }
    fn native_options(&self) -> Vec<NativeOption>;
    /// Returns every stable setting this adapter understands, independent of
    /// whether one exact installed runtime currently supports it.
    fn load_setting_definitions(&self) -> Vec<LoadSettingDefinition> {
        Vec::new()
    }
    /// Gates the curated semantic settings against one exact runtime contract.
    async fn load_settings_schema(
        &self,
        runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        _host: &HostCapabilities,
    ) -> Result<LoadSettingsSchema, EngineError> {
        Ok(LoadSettingsSchema {
            engine_id: self.identity().id,
            runtime_id: Some(runtime.manifest.runtime_id.clone()),
            definitions: self.load_setting_definitions(),
        })
    }
    /// Reports the legacy/flexible-entry runtime configured directly for this
    /// adapter. Managed packs are discovered by the shared runtime store.
    async fn probe(&self) -> Result<EngineProbe, EngineError>;
    /// Validates one exact runtime instance and records what the executable
    /// itself reported. Install activation and launch both use this boundary.
    async fn probe_runtime(
        &self,
        runtime: &InstalledRuntime,
    ) -> Result<RuntimeProbeObservation, EngineError>;
    async fn build_launch_spec(&self, request: LaunchRequest) -> Result<LaunchSpec, EngineError>;
    async fn health(&self, process: &ProcessDescriptor) -> Result<bool, EngineError>;
    async fn effective_generation_settings(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError>;
    fn validate_generation_settings(
        &self,
        settings: &GenerationSettingsPatch,
    ) -> Result<(), EngineError> {
        if settings.is_empty() {
            Ok(())
        } else {
            Err(EngineError::InvalidGenerationSettings(format!(
                "engine `{}` does not support explicit request-time generation settings",
                self.identity().id
            )))
        }
    }
    async fn infer(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceOutput, EngineError>;
    async fn infer_stream(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceStream, EngineError>;
}

/// Common process mechanics live behind this boundary, not in engine adapters.
#[async_trait]
pub trait ProcessSupervisor: Send + Sync {
    async fn spawn(
        &self,
        spec: LaunchSpec,
        engine: EngineRevision,
        model_id: norted_core::ModelId,
    ) -> Result<ProcessDescriptor, EngineError>;
    async fn terminate(&self, process: &ProcessDescriptor) -> Result<(), EngineError>;
    async fn subscribe(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<watch::Receiver<Option<ProcessExit>>, EngineError>;
    async fn shutdown(&self);
}

#[derive(Clone, Default)]
pub struct EngineRegistry {
    adapters: BTreeMap<String, Arc<dyn EngineAdapter>>,
}

impl EngineRegistry {
    pub fn register(&mut self, adapter: Arc<dyn EngineAdapter>) -> Result<(), RegistryError> {
        let id = adapter.identity().id;
        if id.trim().is_empty() {
            return Err(RegistryError::EmptyEngineId);
        }
        match self.adapters.entry(id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(adapter);
                Ok(())
            }
            Entry::Occupied(_) => Err(RegistryError::DuplicateEngineId(id)),
        }
    }

    pub fn get(&self, engine_id: &str) -> Option<Arc<dyn EngineAdapter>> {
        self.adapters.get(engine_id).cloned()
    }

    pub fn adapters(&self) -> impl Iterator<Item = &Arc<dyn EngineAdapter>> {
        self.adapters.values()
    }

    pub fn compatible_with(&self, artifact: &ModelArtifact) -> Vec<Arc<dyn EngineAdapter>> {
        self.adapters
            .values()
            .filter(|adapter| adapter.compatibility(artifact).is_supported())
            .cloned()
            .collect()
    }

    pub fn load_setting_definitions(&self) -> Result<Vec<LoadSettingDefinition>, EngineError> {
        let mut definitions = BTreeMap::<LoadSettingId, LoadSettingDefinition>::new();
        for adapter in self.adapters.values() {
            for definition in adapter.load_setting_definitions() {
                match definitions.entry(definition.id.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(definition);
                    }
                    Entry::Occupied(entry) if entry.get() == &definition => {}
                    Entry::Occupied(entry) => {
                        return Err(EngineError::InvalidConfiguration(format!(
                            "load setting `{}` has conflicting adapter definitions",
                            entry.key()
                        )));
                    }
                }
            }
        }
        Ok(definitions.into_values().collect())
    }

    pub fn parse_load_settings(
        &self,
        assignments: &[String],
    ) -> Result<LoadSettingsPatch, EngineError> {
        let definitions = self
            .load_setting_definitions()?
            .into_iter()
            .map(|definition| (definition.id.clone(), definition))
            .collect::<BTreeMap<_, _>>();
        let mut patch = LoadSettingsPatch::default();
        for assignment in assignments {
            let (raw_id, raw_value) = assignment.split_once('=').ok_or_else(|| {
                EngineError::InvalidConfiguration(format!(
                    "load setting `{assignment}` must use SETTING_ID=VALUE"
                ))
            })?;
            let id = LoadSettingId::new(raw_id.to_owned())
                .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
            let definition = definitions.get(&id).ok_or_else(|| {
                EngineError::InvalidConfiguration(
                    LoadSettingsError::UnknownSetting(id.clone()).to_string(),
                )
            })?;
            let value = definition
                .parse(raw_value)
                .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
            patch.insert(id, value);
        }
        Ok(patch)
    }

    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use async_trait::async_trait;
    use norted_core::{
        AppPaths, ArtifactFormat, AuxiliaryArtifactRole, AvailableRuntime, HostCapabilities,
        ModelArtifact, ModelId, RuntimeArchiveFormat, RuntimeCompatibility, RuntimeDigest,
        RuntimeDownload, RuntimeIdentity, RuntimePackageIdentity, RuntimeReleaseChannel,
        RuntimeRequirements,
    };

    use super::{
        CompatibilityDecision, EffectiveGenerationSettings, EngineAdapter, EngineCapabilities,
        EngineError, EngineIdentity, EngineProbe, EngineRegistry, GenerationSettingsPatch,
        InferenceRole, InstallationState, LaunchRequest, LaunchSpec, NativeOption,
        PreparedAuxiliaryArtifact, PreparedModelInput, ProcessDescriptor, RuntimeCatalogProvider,
        RuntimePackManager, UpdateState,
    };

    struct ArchitectureAdapter {
        id: &'static str,
        architecture: &'static str,
        format: ArtifactFormat,
    }

    #[test]
    fn effective_generation_settings_merge_without_changing_backend_defaults() {
        let backend_defaults = EffectiveGenerationSettings {
            temperature: 0.7,
            top_p: 0.9,
        };
        let override_temperature = GenerationSettingsPatch {
            temperature: Some(0.2),
            top_p: None,
        };

        assert_eq!(
            backend_defaults.merged(&GenerationSettingsPatch::default()),
            backend_defaults
        );
        assert_eq!(
            backend_defaults.merged(&override_temperature),
            EffectiveGenerationSettings {
                temperature: 0.2,
                top_p: 0.9,
            }
        );
        assert_eq!(backend_defaults.temperature, 0.7);
        assert_eq!(backend_defaults.top_p, 0.9);
    }

    #[test]
    fn canonical_inference_roles_serialize_with_openai_names() {
        for (role, expected) in [
            (InferenceRole::Developer, "developer"),
            (InferenceRole::System, "system"),
            (InferenceRole::User, "user"),
            (InferenceRole::Assistant, "assistant"),
        ] {
            assert_eq!(serde_json::to_value(role).expect("role"), expected);
        }
    }

    #[async_trait]
    impl EngineAdapter for ArchitectureAdapter {
        fn identity(&self) -> EngineIdentity {
            EngineIdentity {
                id: self.id.to_owned(),
                display_name: self.id.to_owned(),
                upstream_repository: String::new(),
            }
        }

        fn capabilities(&self) -> EngineCapabilities {
            EngineCapabilities {
                artifact_formats: vec![self.format],
                ..EngineCapabilities::default()
            }
        }

        fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
            let coarse = if self.capabilities().accepts(model.format) {
                CompatibilityDecision::Supported
            } else {
                CompatibilityDecision::Unsupported {
                    reason: "format is unsupported".to_owned(),
                }
            };
            if !coarse.is_supported() {
                return coarse;
            }
            match model.architecture.as_deref() {
                Some(architecture) if architecture == self.architecture => {
                    CompatibilityDecision::Supported
                }
                architecture => CompatibilityDecision::Unsupported {
                    reason: format!(
                        "requires architecture `{}`, found `{}`",
                        self.architecture,
                        architecture.unwrap_or("unknown")
                    ),
                },
            }
        }

        fn native_options(&self) -> Vec<NativeOption> {
            Vec::new()
        }

        async fn probe(&self) -> Result<EngineProbe, EngineError> {
            Ok(EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "fixture adapter has no installed runtime".to_owned(),
            })
        }

        async fn probe_runtime(
            &self,
            _runtime: &norted_core::InstalledRuntime,
        ) -> Result<norted_core::RuntimeProbeObservation, EngineError> {
            unreachable!()
        }

        async fn build_launch_spec(
            &self,
            _request: LaunchRequest,
        ) -> Result<LaunchSpec, EngineError> {
            unreachable!()
        }

        async fn health(&self, _process: &ProcessDescriptor) -> Result<bool, EngineError> {
            unreachable!()
        }

        async fn effective_generation_settings(
            &self,
            _process: &ProcessDescriptor,
        ) -> Result<super::EffectiveGenerationSettings, EngineError> {
            unreachable!()
        }

        async fn infer(
            &self,
            _endpoint: &str,
            _request: super::InferenceRequest,
        ) -> Result<super::InferenceOutput, EngineError> {
            unreachable!()
        }

        async fn infer_stream(
            &self,
            _endpoint: &str,
            _request: super::InferenceRequest,
        ) -> Result<super::InferenceStream, EngineError> {
            unreachable!()
        }
    }

    #[test]
    fn registry_uses_adapter_model_compatibility_after_the_format_gate() {
        let mut registry = EngineRegistry::default();
        registry
            .register(Arc::new(ArchitectureAdapter {
                id: "architecture-a",
                architecture: "architecture-a",
                format: ArtifactFormat::Gguf,
            }))
            .expect("register architecture-a adapter");
        registry
            .register(Arc::new(ArchitectureAdapter {
                id: "architecture-b",
                architecture: "architecture-b",
                format: ArtifactFormat::Gguf,
            }))
            .expect("register architecture-b adapter");

        let model = ModelArtifact {
            id: ModelId("fixture".to_owned()),
            display_name: "Fixture".to_owned(),
            path: PathBuf::from("fixture.gguf"),
            format: ArtifactFormat::Gguf,
            size_bytes: 0,
            created: 0,
            hash: None,
            architecture: Some("architecture-b".to_owned()),
            context_length: None,
            provenance: None,
            auxiliary_artifacts: Vec::new(),
        };
        let compatible = registry.compatible_with(&model);

        assert_eq!(compatible.len(), 1);
        assert_eq!(compatible[0].identity().id, "architecture-b");
        assert_eq!(
            registry
                .get("architecture-a")
                .expect("architecture-a adapter")
                .compatibility(&model),
            CompatibilityDecision::Unsupported {
                reason: "requires architecture `architecture-a`, found `architecture-b`".to_owned()
            }
        );
    }

    #[test]
    fn adapter_default_rejects_explicit_generation_settings() {
        let adapter = ArchitectureAdapter {
            id: "no-request-samplers",
            architecture: "fixture",
            format: ArtifactFormat::Gguf,
        };
        assert!(
            adapter
                .validate_generation_settings(&GenerationSettingsPatch::default())
                .is_ok()
        );
        assert!(matches!(
            adapter.validate_generation_settings(&GenerationSettingsPatch {
                temperature: Some(0.5),
                top_p: None,
            }),
            Err(EngineError::InvalidGenerationSettings(_))
        ));
    }

    #[tokio::test]
    async fn model_serving_capabilities_are_registry_derived_and_allow_no_runtime() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path();
        let paths = AppPaths {
            config_dir: root.join("config"),
            config_file: root.join("config/config.toml"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
            runtimes_dir: root.join("data/runtimes"),
            runtime_cache_dir: root.join("cache/runtime-packs"),
            runtime_selections_file: root.join("data/runtime-selections.json"),
            load_profiles_file: root.join("data/load-profiles.json"),
            load_profiles_lock_file: root.join("data/.load-profiles.lock"),
        };
        paths.ensure_required().expect("application paths");

        let mut registry = EngineRegistry::default();
        registry
            .register(Arc::new(ArchitectureAdapter {
                id: "fixture-engine",
                architecture: "fixture-architecture",
                format: ArtifactFormat::Gguf,
            }))
            .expect("register fixture adapter");
        let providers = Vec::<Arc<dyn RuntimeCatalogProvider>>::new();
        let manager = RuntimePackManager::new(&paths, registry, providers).expect("pack manager");
        let model = ModelArtifact {
            id: ModelId("fixture-model".to_owned()),
            display_name: "Fixture model".to_owned(),
            path: root.join("model.gguf"),
            format: ArtifactFormat::Gguf,
            size_bytes: 0,
            created: 0,
            hash: None,
            architecture: Some("fixture-architecture".to_owned()),
            context_length: None,
            provenance: None,
            auxiliary_artifacts: Vec::new(),
        };

        let capabilities = manager
            .model_serving_capabilities(&model, Some(&model.id))
            .await
            .expect("capabilities");
        assert_eq!(capabilities.model_id, model.id);
        assert_eq!(capabilities.compatible_engine_ids, ["fixture-engine"]);
        assert!(capabilities.compatible_installed_runtime_ids.is_empty());
        assert_eq!(capabilities.selected_runtime_id, None);
        assert!(capabilities.active);
        assert!(capabilities.text_input && capabilities.text_output);
        assert!(capabilities.responses && capabilities.chat_completions && capabilities.streaming);
        assert!(!capabilities.tools);
        assert!(!capabilities.vision);
        assert!(!capabilities.structured_output);
        serde_json::to_value(&capabilities).expect("serializable capabilities");
    }

    #[test]
    fn available_model_compatibility_defaults_preserve_other_engines_for_the_format() {
        let adapter = ArchitectureAdapter {
            id: "second-q27-engine",
            architecture: "qwen35",
            format: ArtifactFormat::Q27,
        };
        let model = ModelArtifact {
            id: ModelId("fixture-q27".to_owned()),
            display_name: "Fixture Q27".to_owned(),
            path: PathBuf::from("fixture.q27"),
            format: ArtifactFormat::Q27,
            size_bytes: 0,
            created: 0,
            hash: None,
            architecture: Some("qwen35".to_owned()),
            context_length: None,
            provenance: None,
            auxiliary_artifacts: Vec::new(),
        };
        let identity = RuntimeIdentity {
            engine_id: "second-q27-engine".to_owned(),
            package_family: "fixture".to_owned(),
            version: "1".to_owned(),
            upstream_revision: None,
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            accelerator: "cpu".to_owned(),
            variant: "default".to_owned(),
            package: RuntimePackageIdentity {
                provider_id: "fixture".to_owned(),
                repository: Some("fixture/repository".to_owned()),
                release_tag: Some("v1".to_owned()),
                asset_id: Some("1".to_owned()),
                asset_name: Some("runtime.zip".to_owned()),
                additional_assets: Vec::new(),
            },
        };
        let runtime = AvailableRuntime {
            runtime_id: norted_core::RuntimeId::from_identity(&identity),
            identity,
            display_name: "Second Q27 engine".to_owned(),
            supported_formats: vec![ArtifactFormat::Q27],
            source_url: "https://github.com/fixture/repository/releases/tag/v1".to_owned(),
            published_at_unix: None,
            channels: vec![RuntimeReleaseChannel::Stable],
            prerelease: false,
            download: RuntimeDownload {
                url: "https://github.com/fixture/repository/releases/download/v1/runtime.zip"
                    .to_owned(),
                size_bytes: 1,
                digest: Some(RuntimeDigest::sha256("a".repeat(64)).expect("digest")),
                archive_format: RuntimeArchiveFormat::Zip,
                entrypoint_names: vec!["server".to_owned()],
            },
            additional_downloads: Vec::new(),
            requirements: RuntimeRequirements::default(),
        };
        assert!(matches!(
            adapter.available_runtime_model_compatibility(
                &runtime,
                &model,
                &HostCapabilities::current_without_accelerator_probe(),
            ),
            RuntimeCompatibility::Compatible
        ));
    }

    #[test]
    fn prepared_auxiliary_identity_is_preserved_for_launch_provenance() {
        let prepared = PreparedModelInput {
            primary: ModelArtifact {
                id: ModelId("fixture".to_owned()),
                display_name: "Fixture".to_owned(),
                path: PathBuf::from("model.q27"),
                format: ArtifactFormat::Q27,
                size_bytes: 100,
                created: 0,
                hash: None,
                architecture: None,
                context_length: None,
                provenance: None,
                auxiliary_artifacts: Vec::new(),
            },
            auxiliary: vec![PreparedAuxiliaryArtifact {
                role: AuxiliaryArtifactRole::Tokenizer,
                path: PathBuf::from("model.tok"),
                size_bytes: 8,
                content_sha256: "a".repeat(64),
            }],
        };
        let identity = prepared.runtime_identity();
        assert_eq!(identity.auxiliary.len(), 1);
        assert_eq!(identity.auxiliary[0].role, AuxiliaryArtifactRole::Tokenizer);
        assert_eq!(identity.auxiliary[0].size_bytes, 8);
        assert_eq!(identity.auxiliary[0].content_sha256, "a".repeat(64));
    }
}
