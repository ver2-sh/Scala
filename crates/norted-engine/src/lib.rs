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
    ArtifactFormat, AvailableRuntime, EngineInstallation, EngineRevision, InstalledRuntime,
    ModelArtifact, RuntimeProbeObservation,
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
    RuntimeProviderError, compatibility_for, detect_host_capabilities,
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
    InstalledRuntimeStatus, RuntimeListSnapshot, RuntimePackError, RuntimePackManager,
    RuntimeSearchResult, RuntimeSearchSnapshot, RuntimeUpdateCheck,
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

#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub model: ModelArtifact,
    pub runtime: InstalledRuntime,
    pub backend_address: SocketAddr,
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
    pub native_arguments: Vec<String>,
    pub installation: EngineInstallation,
    pub runtime: InstalledRuntime,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveGenerationSettings {
    pub temperature: f64,
    pub top_p: f64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequest {
    pub model_id: norted_core::ModelId,
    pub messages: Vec<InferenceMessage>,
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
    fn native_options(&self) -> Vec<NativeOption>;
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
    use norted_core::{ArtifactFormat, ModelArtifact, ModelId};

    use super::{
        CompatibilityDecision, EngineAdapter, EngineCapabilities, EngineError, EngineIdentity,
        EngineProbe, EngineRegistry, LaunchRequest, LaunchSpec, NativeOption, ProcessDescriptor,
    };

    struct ArchitectureAdapter {
        id: &'static str,
        architecture: &'static str,
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
                artifact_formats: vec![ArtifactFormat::Gguf],
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
            unreachable!()
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
            }))
            .expect("register architecture-a adapter");
        registry
            .register(Arc::new(ArchitectureAdapter {
                id: "architecture-b",
                architecture: "architecture-b",
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
}
