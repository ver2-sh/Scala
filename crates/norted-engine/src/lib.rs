//! Engine-neutral contracts for managed upstream inference runtimes.

use std::collections::{BTreeMap, btree_map::Entry};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use norted_core::{ArtifactFormat, EngineInstallation, EngineRevision, ModelArtifact};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "target")]
pub enum AcquisitionTarget {
    Stable,
    Latest,
    Exact {
        version: Option<String>,
        revision: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationContext {
    pub install_root: PathBuf,
    pub cache_root: PathBuf,
    pub platform: String,
    pub architecture: String,
    pub build_options: BTreeMap<String, String>,
    pub runtime_variant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcquisitionRequest {
    pub target: AcquisitionTarget,
    pub context: InstallationContext,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub model: ModelArtifact,
    pub normalized_options: BTreeMap<String, String>,
    pub native_arguments: Vec<String>,
    pub native_environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub working_directory: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessDescriptor {
    pub process_id: u32,
    pub engine: EngineRevision,
    pub model_id: norted_core::ModelId,
    pub endpoint: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine operation is not implemented: {0}")]
    Unsupported(String),
    #[error("engine is not installed")]
    NotInstalled,
    #[error("engine operation failed: {0}")]
    Operation(String),
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
    fn native_options(&self) -> Vec<NativeOption>;
    async fn probe(&self) -> Result<EngineProbe, EngineError>;
    async fn install(&self, request: AcquisitionRequest)
    -> Result<EngineInstallation, EngineError>;
    async fn update(&self, request: AcquisitionRequest) -> Result<EngineInstallation, EngineError>;
    async fn build_launch_spec(&self, request: LaunchRequest) -> Result<LaunchSpec, EngineError>;
    async fn health(&self, process: &ProcessDescriptor) -> Result<bool, EngineError>;
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
}

#[derive(Default)]
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
            .filter(|adapter| adapter.capabilities().accepts(artifact.format))
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
